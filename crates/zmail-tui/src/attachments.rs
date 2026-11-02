//! Saving attachments from Message Text: `V` (Alpine's ViewAttch) opens
//! the Attachment Index, the open conversation's attachments with their
//! names, types and sizes, every one filtered for the terminal. `S` (or
//! Enter) saves the one under the cursor to a path typed with Tab
//! completion, `A` saves them all into a folder.
//!
//! - A file is saved under its sanitised name
//!   ([`sanitize_filename`]) in the folder last saved to, else Downloads,
//!   else the home folder, unless the user types another path.
//! - Saving goes through [`AttachmentBlob::save_to`], which decrypts to a
//!   temporary file beside the destination and renames it over, so a
//!   failed save leaves nothing partial behind.
//! - A file is never replaced without asking; saving them all numbers a
//!   name that is taken (`report (1).pdf`), as the desktop app's Save All
//!   does.
//! - Nothing is opened or launched: handing a file from mail to another
//!   program needs a design of its own, so the terminal only saves.
//!
//! [`AttachmentBlob::save_to`]: zmail_store::AttachmentBlob::save_to

use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use anyhow::{Context, Result, bail};
use zmail_core::{participants::sender_identity, text::count_noun};
use zmail_security::{
    attachment_safety::{MAX_NUMBERED_NAMES, numbered_filename, sanitize_filename},
    terminal::sanitize_line,
};
use zmail_store::LocalStore;

use crate::{
    app::{App, Prompt, Screen},
    compose::{PathPrompt, PathPurpose},
    message::size_label,
    pane::PaneId,
    paths::PathInput,
};

/// An attachment as the Attachment Index lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRow {
    /// The attachment.
    pub id: i64,
    /// Which message of the conversation carries it, from 1.
    pub message: usize,
    /// That message's sender, filtered.
    pub sender: String,
    /// Its name as the sender gave it, filtered for the terminal.
    pub name: String,
    /// The name it is saved under ([`sanitize_filename`]).
    pub file_name: String,
    /// Its MIME type, filtered.
    pub content_type: String,
    /// Its size in bytes.
    pub size: u64,
}

/// The Attachment Index's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentList {
    /// The conversation.
    pub thread_id: i64,
    /// Its subject, filtered.
    pub subject: String,
    /// How many messages it has.
    pub messages: usize,
    /// Its attachments, in message order.
    pub rows: Vec<AttachmentRow>,
    /// The row the cursor is on.
    pub cursor: usize,
}

impl AttachmentList {
    /// Reads conversation `thread_id`'s attachments; `None` when it's gone.
    pub fn load(store: &LocalStore, thread_id: i64) -> Result<Option<Self>> {
        let Some(view) = store.thread_view(thread_id)? else {
            return Ok(None);
        };
        let rows = view
            .messages
            .iter()
            .enumerate()
            .flat_map(|(index, message)| {
                let sender = sanitize_line(&sender_identity(&message.sender).0);
                message
                    .attachments
                    .iter()
                    .map(move |attachment| AttachmentRow {
                        id: attachment.id,
                        message: index + 1,
                        sender: sender.clone(),
                        name: sanitize_line(&attachment.filename),
                        file_name: sanitize_filename(&attachment.filename),
                        content_type: sanitize_line(&attachment.content_type),
                        size: attachment.size_bytes,
                    })
            })
            .collect();
        Ok(Some(Self {
            thread_id,
            subject: sanitize_line(&view.subject),
            messages: view.messages.len(),
            rows,
            cursor: 0,
        }))
    }

    /// The row under the cursor.
    pub fn current(&self) -> Option<&AttachmentRow> {
        self.rows.get(self.cursor)
    }
}

/// Where saving starts when nothing has been saved yet: Downloads in the
/// home folder `home`, else the home folder, else the working folder, as
/// in the desktop app.
pub fn default_save_folder(home: Option<PathBuf>) -> PathBuf {
    match home {
        Some(home) if home.join("Downloads").is_dir() => home.join("Downloads"),
        Some(home) => home,
        None => std::env::current_dir().unwrap_or_default(),
    }
}

/// `path` as Zmail shows it, filtered: `~` for the home folder `home`.
pub fn shown_path(path: &Path, home: Option<&Path>) -> String {
    sanitize_line(&match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{MAIN_SEPARATOR}{}", rest.display()),
        None => path.display().to_string(),
    })
}

/// `folder` as a path field starts it: `~` for the home folder, and a
/// separator at the end so Tab lists what is inside.
pub fn folder_input(folder: &Path, home: Option<&Path>) -> String {
    let shown = shown_path(folder, home);
    if shown.ends_with(MAIN_SEPARATOR) {
        shown
    } else {
        format!("{shown}{MAIN_SEPARATOR}")
    }
}

/// Saves attachment `id` of `store` to `destination`, replacing a file
/// there only when `replace`. Returns its size.
pub fn save_one(store: &LocalStore, id: i64, destination: &Path, replace: bool) -> Result<u64> {
    if !replace && destination.exists() {
        bail!("{} already exists", destination.display());
    }
    let folder = destination.parent().unwrap_or(Path::new("."));
    if !folder.as_os_str().is_empty() && !folder.is_dir() {
        bail!("there is no folder {}", folder.display());
    }
    store.attachment_blob(id)?.save_to(destination)
}

/// Saves each attachment in `rows` into `folder` under its sanitised name,
/// numbered when the name is taken, never replacing a file. Returns the
/// paths saved and the (filtered) names of those that failed.
pub fn save_all(
    store: &LocalStore,
    rows: &[AttachmentRow],
    folder: &Path,
) -> (Vec<PathBuf>, Vec<String>) {
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    for row in rows {
        let result = free_name(folder, &row.file_name).and_then(|destination| {
            save_one(store, row.id, &destination, false).map(|_| destination)
        });
        match result {
            Ok(destination) => saved.push(destination),
            Err(_) => failed.push(row.name.clone()),
        }
    }
    (saved, failed)
}

/// The first of `file_name`, `file_name (1)`, … not taken in `folder`.
fn free_name(folder: &Path, file_name: &str) -> Result<PathBuf> {
    (0..=MAX_NUMBERED_NAMES)
        .map(|number| folder.join(numbered_filename(file_name, number)))
        .find(|destination| !destination.exists())
        .with_context(|| format!("{file_name} and {MAX_NUMBERED_NAMES} numbered copies exist"))
}

impl App {
    /// `V` in Message Text: the Attachment Index of the open conversation.
    pub(crate) fn open_attachments(&mut self) {
        let Some(thread_id) = self
            .message
            .as_ref()
            .map(|state| state.conversation.thread_id)
        else {
            return;
        };
        match AttachmentList::load(&self.store, thread_id) {
            Ok(Some(list)) if !list.rows.is_empty() => {
                self.attachments = Some(list);
                self.scroll.insert(PaneId::Attachments, 0);
                self.goto(Screen::Attachments);
            }
            Ok(_) => self.status = Some("This conversation has no attachments".to_owned()),
            Err(error) => self.report(error),
        }
    }

    /// The folder saving starts in.
    fn save_folder(&self) -> PathBuf {
        self.save_folder
            .clone()
            .unwrap_or_else(|| default_save_folder(std::env::home_dir()))
    }

    /// `S` or Enter: asks where to save the attachment under the cursor.
    pub(crate) fn ask_save_attachment(&mut self) {
        let Some(row) = self.attachments.as_ref().and_then(AttachmentList::current) else {
            return;
        };
        let (id, file_name) = (row.id, row.file_name.clone());
        let folder = folder_input(&self.save_folder(), std::env::home_dir().as_deref());
        self.prompt = Some(Prompt::Path(PathPrompt {
            purpose: PathPurpose::SaveAttachment(id),
            field: PathInput {
                input: format!("{folder}{file_name}"),
                ..PathInput::default()
            },
        }));
    }

    /// `A`: asks which folder to save every attachment into.
    pub(crate) fn ask_save_all_attachments(&mut self) {
        if self.attachments.is_none() {
            return;
        }
        let folder = folder_input(&self.save_folder(), std::env::home_dir().as_deref());
        self.prompt = Some(Prompt::Path(PathPrompt {
            purpose: PathPurpose::SaveAll,
            field: PathInput {
                input: folder,
                ..PathInput::default()
            },
        }));
    }

    /// Enter in the save prompt: saves to `path` (into it, when it is a
    /// folder), or asks first when a file is there.
    pub(crate) fn save_attachment_to(&mut self, id: i64, prompt: PathPrompt) {
        let Some(row) = self
            .attachments
            .as_ref()
            .and_then(|list| list.rows.iter().find(|row| row.id == id))
            .cloned()
        else {
            return;
        };
        let mut path = prompt.field.path();
        if path.is_dir() {
            path = path.join(&row.file_name);
        }
        if path.exists() {
            self.prompt = Some(Prompt::ReplaceFile { prompt, path });
            return;
        }
        self.write_attachment(&row, &path, false);
    }

    /// "Replace it?" answered.
    pub(crate) fn replace_file_key(&mut self, prompt: PathPrompt, path: PathBuf, replace: bool) {
        let PathPurpose::SaveAttachment(id) = prompt.purpose else {
            return;
        };
        let row = self
            .attachments
            .as_ref()
            .and_then(|list| list.rows.iter().find(|row| row.id == id))
            .cloned();
        match row {
            Some(row) if replace => self.write_attachment(&row, &path, true),
            // Back to the path, to type another name.
            _ => self.prompt = Some(Prompt::Path(prompt)),
        }
    }

    fn write_attachment(&mut self, row: &AttachmentRow, path: &Path, replace: bool) {
        match save_one(&self.store, row.id, path, replace) {
            Ok(size) => {
                self.save_folder = path.parent().map(Path::to_path_buf);
                let folder = path.parent().unwrap_or(Path::new("."));
                self.status = Some(format!(
                    "Saved {} ({}) to {}",
                    sanitize_line(&path.file_name().unwrap_or_default().to_string_lossy()),
                    size_label(size),
                    shown_path(folder, std::env::home_dir().as_deref())
                ));
            }
            Err(error) => {
                self.status = Some(format!(
                    "Couldn't save {}: {}",
                    row.name,
                    sanitize_line(&format!("{error:#}"))
                ));
            }
        }
    }

    /// Enter in the Save All prompt.
    pub(crate) fn save_all_attachments_to(&mut self, folder: &Path) {
        let Some(list) = self.attachments.as_ref() else {
            return;
        };
        if !folder.is_dir() {
            self.status = Some(format!(
                "There is no folder {}",
                sanitize_line(&folder.display().to_string())
            ));
            return;
        }
        let total = list.rows.len();
        let (saved, failed) = save_all(&self.store, &list.rows, folder);
        let shown = shown_path(folder, std::env::home_dir().as_deref());
        self.status = Some(if failed.is_empty() {
            format!(
                "Saved {} to {shown}",
                count_noun(saved.len(), "attachment", "attachments")
            )
        } else {
            // Each one that failed says why when saved on its own (S).
            format!(
                "Saved {} of {total} to {shown}; couldn't save {}",
                saved.len(),
                failed.join(", ")
            )
        });
        self.save_folder = Some(folder.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let folder = std::env::temp_dir().join(format!(
            "zmail-tui-attachments-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&folder).expect("the folder is made");
        folder
    }

    #[test]
    fn saving_starts_in_downloads_else_home() {
        let home = scratch("home");
        assert_eq!(default_save_folder(Some(home.clone())), home);
        std::fs::create_dir(home.join("Downloads")).expect("Downloads is made");
        assert_eq!(
            default_save_folder(Some(home.clone())),
            home.join("Downloads")
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_folder_field_shows_home_as_a_tilde_and_ends_in_a_separator() {
        let home = Path::new("/home/ada");
        let sep = MAIN_SEPARATOR;
        assert_eq!(
            folder_input(&home.join("Downloads"), Some(home)),
            format!("~{sep}Downloads{sep}")
        );
        assert_eq!(folder_input(home, Some(home)), format!("~{sep}"));
        assert_eq!(
            folder_input(Path::new("/srv/mail"), Some(home)),
            format!("/srv/mail{sep}")
        );
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let folder = scratch("free");
        assert_eq!(
            free_name(&folder, "a.pdf").expect("free"),
            folder.join("a.pdf")
        );
        std::fs::write(folder.join("a.pdf"), b"x").expect("written");
        std::fs::write(folder.join("a (1).pdf"), b"x").expect("written");
        assert_eq!(
            free_name(&folder, "a.pdf").expect("free"),
            folder.join("a (2).pdf")
        );
        let _ = std::fs::remove_dir_all(folder);
    }
}
