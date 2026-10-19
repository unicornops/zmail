//! Opening and saving received attachments: the workspace's handlers for
//! the attachment cards' Open and Save As buttons and for Save All.
//!
//! - **Open** copies the attachment into a private cache directory (owner
//!   only on macOS and Linux) and hands it to the system's default app. A
//!   program or script (`attachment_safety::is_executable_or_script`)
//!   gets a warning first, and opens only on "Open Anyway".
//! - **Save As** asks for a path with the platform save dialog, which
//!   itself asks before replacing an existing file.
//! - **Save All** ([`SaveAttachments`](crate::actions::SaveAttachments))
//!   asks for a folder and saves every attachment of the conversation
//!   there. A name that is taken gets a number ("report (1).pdf"); no
//!   file is ever overwritten.
//!
//! Every name written to disk goes through
//! `attachment_safety::sanitize_filename` first. The copies run on the
//! background executor.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gpui::{
    AnyElement, App, Context, IntoElement, PathPromptOptions, SharedString, Window, div, prelude::*,
};
use zmail_core::{ReceivedAttachment, text::count_noun};
use zmail_security::attachment_safety::{is_executable_or_script, sanitize_filename};

use crate::WorkspaceView;
use crate::actions::{OpenAttachment, SaveAttachmentAs};
use crate::components::{Button, Modal, ModalState};

/// Directory under the store's directory that opened attachments are
/// copied into.
const OPEN_CACHE_DIR: &str = "opened-attachments";

/// How many numbered names Save All tries before giving up on a file.
const MAX_NUMBERED_NAMES: u32 = 999;

/// The workspace's attachment state: the open warning and where Save As
/// starts.
pub(crate) struct AttachmentState {
    open_warning: ModalState,
    /// The attachment waiting for "Open Anyway".
    pending_open: Option<ReceivedAttachment>,
    /// The folder of the last Save As or Save All, where the next Save As
    /// dialog starts.
    last_save_dir: Option<PathBuf>,
    /// Test instrumentation: the files handed to the system to open. (The
    /// test platform cannot open files.)
    #[cfg(test)]
    pub(crate) opened: Vec<PathBuf>,
}

impl AttachmentState {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            open_warning: ModalState::new(cx),
            pending_open: None,
            last_save_dir: None,
            #[cfg(test)]
            opened: Vec::new(),
        }
    }

    /// The attachment the open warning is asking about.
    #[cfg(test)]
    pub(crate) fn pending_open(&self) -> Option<&ReceivedAttachment> {
        self.pending_open.as_ref()
    }
}

impl WorkspaceView {
    /// The attachment with `attachment_id` in the open conversation.
    fn find_attachment(&mut self, attachment_id: i64) -> Option<ReceivedAttachment> {
        self.selected_thread_view()?
            .messages
            .into_iter()
            .flat_map(|message| message.attachments)
            .find(|attachment| attachment.id == attachment_id)
    }

    /// Open: warn first for programs and scripts, otherwise open now.
    pub(crate) fn open_attachment(
        &mut self,
        action: &OpenAttachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(attachment) = self.find_attachment(action.attachment_id) else {
            self.show_error("That attachment is no longer in the open conversation.", cx);
            return;
        };
        if is_executable_or_script(&attachment.filename, &attachment.content_type) {
            self.attachments.pending_open = Some(attachment);
            self.attachments.open_warning.open(window, cx);
            cx.notify();
        } else {
            self.open_attachment_now(&attachment, cx);
        }
    }

    fn confirm_open_anyway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attachments.open_warning.close(window, cx);
        if let Some(attachment) = self.attachments.pending_open.take() {
            self.open_attachment_now(&attachment, cx);
        }
        cx.notify();
    }

    fn cancel_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attachments.open_warning.close(window, cx);
        self.attachments.pending_open = None;
        cx.notify();
    }

    /// Copies the attachment into the private cache and opens it with the
    /// system's default app.
    fn open_attachment_now(&mut self, attachment: &ReceivedAttachment, cx: &mut Context<Self>) {
        let (source, filename) = match self.store.attachment_blob(attachment.id) {
            Ok(blob) => blob,
            Err(error) => return self.show_error(format!("{error:#}"), cx),
        };
        let cache_root = self.store_dir().join(OPEN_CACHE_DIR);
        let attachment_id = attachment.id;
        let copy = cx.background_spawn(async move {
            copy_to_open_cache(&source, &cache_root, attachment_id, &filename)
        });
        cx.spawn(async move |this, cx| {
            let result = copy.await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(path) => view.open_with_system(&path, cx),
                Err(error) => {
                    view.show_error(format!("Could not open the attachment: {error}"), cx)
                }
            });
        })
        .detach();
    }

    #[cfg(not(test))]
    fn open_with_system(&mut self, path: &Path, cx: &mut Context<Self>) {
        cx.open_with_system(path);
    }

    #[cfg(test)]
    fn open_with_system(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.attachments.opened.push(path.to_owned());
        cx.notify();
    }

    /// Save As: asks for a path, starting in the last folder saved to.
    pub(crate) fn save_attachment_as(
        &mut self,
        action: &SaveAttachmentAs,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (source, filename) = match self.store.attachment_blob(action.attachment_id) {
            Ok(blob) => blob,
            Err(error) => return self.show_error(format!("{error:#}"), cx),
        };
        let directory = self
            .attachments
            .last_save_dir
            .clone()
            .unwrap_or_else(default_save_dir);
        let suggested = sanitize_filename(&filename);
        let prompt = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let destination = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |view, cx| {
                        view.show_error(format!("Could not show the save dialog: {error:#}"), cx)
                    });
                    return;
                }
            };
            // The save dialog has already asked before replacing a file.
            let target = destination.clone();
            let result = cx
                .background_spawn(async move { fs::copy(&source, &target).map(|_| ()) })
                .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(()) => {
                    view.attachments.last_save_dir = destination.parent().map(Path::to_owned);
                    view.show_status(format!("Saved {}.", destination.display()), cx);
                }
                Err(error) => view.show_error(
                    format!("Could not save {}: {error}", destination.display()),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Save All: asks for a folder and saves every attachment of the open
    /// conversation into it, never overwriting a file. The More menu and
    /// the Save Attachments command (`SaveAttachments`) call this.
    pub(crate) fn save_all_attachments(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.selected_thread_view() else {
            return self.show_error("Select a conversation first.", cx);
        };
        let mut files = Vec::new();
        for attachment in thread
            .messages
            .iter()
            .flat_map(|message| &message.attachments)
        {
            match self.store.attachment_blob(attachment.id) {
                Ok(blob) => files.push(blob),
                Err(error) => return self.show_error(format!("{error:#}"), cx),
            }
        }
        if files.is_empty() {
            return self.show_error("This conversation has no attachments.", cx);
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from("Save Here")),
        });
        cx.spawn(async move |this, cx| {
            let folder = match prompt.await {
                Ok(Ok(Some(mut paths))) if !paths.is_empty() => paths.swap_remove(0),
                Ok(Ok(_)) | Err(_) => return,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |view, cx| {
                        view.show_error(format!("Could not show the folder dialog: {error:#}"), cx)
                    });
                    return;
                }
            };
            let target = folder.clone();
            let results = cx
                .background_spawn(async move {
                    files
                        .iter()
                        .map(|(source, filename)| {
                            copy_to_unique_path(source, &target, &sanitize_filename(filename))
                                .map_err(|error| format!("{filename}: {error}"))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let saved = results.iter().filter(|result| result.is_ok()).count();
                let failures: Vec<String> = results.into_iter().filter_map(Result::err).collect();
                view.attachments.last_save_dir = Some(folder.clone());
                if failures.is_empty() {
                    view.show_status(
                        format!(
                            "Saved {} to {}.",
                            count_noun(saved, "attachment", "attachments"),
                            folder.display()
                        ),
                        cx,
                    );
                } else {
                    view.show_error(
                        format!(
                            "Saved {} to {}; could not save {}",
                            count_noun(saved, "attachment", "attachments"),
                            folder.display(),
                            failures.join("; ")
                        ),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// The warning shown before opening a program or script.
    pub(crate) fn render_open_warning(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.attachments.open_warning.is_open() {
            return None;
        }
        let attachment = self.attachments.pending_open.as_ref()?;
        Some(
            Modal::new(
                "attachment-open-warning",
                self.attachments.open_warning.focus_handle(),
            )
            .title(format!(
                "Open \u{201C}{}\u{201D}?",
                sanitize_filename(&attachment.filename)
            ))
            .description(
                "This kind of file can run programs on your computer. Open it only if you \
                 expected it and trust the sender.",
            )
            .footer(
                div()
                    .debug_selector(|| "attachment-open-cancel".into())
                    .child(
                        Button::new("attachment-open-cancel", "Cancel").on_click(
                            cx.listener(|view, _, window, cx| view.cancel_open(window, cx)),
                        ),
                    ),
            )
            .footer(
                div()
                    .debug_selector(|| "attachment-open-anyway".into())
                    .child(
                        Button::new("attachment-open-anyway", "Open Anyway")
                            .danger()
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.confirm_open_anyway(window, cx)
                            })),
                    ),
            )
            .on_dismiss(cx.listener(|view, _, window, cx| view.cancel_open(window, cx)))
            .into_any_element(),
        )
    }

    /// The directory holding the store, its attachment blobs and the open
    /// cache.
    fn store_dir(&self) -> PathBuf {
        self.store
            .path()
            .parent()
            .map(Path::to_owned)
            .unwrap_or_default()
    }
}

/// Where Save As starts when nothing has been saved yet: Downloads, else
/// the home directory, else the working directory.
fn default_save_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match home {
        Some(home) if home.join("Downloads").is_dir() => home.join("Downloads"),
        Some(home) => home,
        None => std::env::current_dir().unwrap_or_default(),
    }
}

/// Copies `source` to `<cache_root>/<attachment_id>/<sanitised name>` and
/// returns that path. The cache directories are created owner-only on
/// Unix, and an earlier copy of the same attachment is replaced.
pub(crate) fn copy_to_open_cache(
    source: &Path,
    cache_root: &Path,
    attachment_id: i64,
    filename: &str,
) -> io::Result<PathBuf> {
    let directory = cache_root.join(attachment_id.to_string());
    create_private_dir(cache_root)?;
    create_private_dir(&directory)?;
    let destination = directory.join(sanitize_filename(filename));
    match fs::remove_file(&destination) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    copy_new(source, &destination)?;
    Ok(destination)
}

/// Creates `directory` if needed and makes it owner-only on Unix (also when
/// it already existed). Windows keeps the per-user ACL of the app data
/// directory it sits in.
fn create_private_dir(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Copies `source` into `directory` as `filename`, or the first free
/// numbered name ("report (1).pdf", "report (2).pdf", …). Returns the path
/// written. Never replaces an existing file, even one created meanwhile.
pub(crate) fn copy_to_unique_path(
    source: &Path,
    directory: &Path,
    filename: &str,
) -> io::Result<PathBuf> {
    for number in 0..=MAX_NUMBERED_NAMES {
        let destination = directory.join(numbered_name(filename, number));
        match copy_new(source, &destination) {
            Ok(()) => return Ok(destination),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{filename} and {MAX_NUMBERED_NAMES} numbered copies already exist"),
    ))
}

/// `filename` with ` (number)` before its extension; `number` 0 is the
/// name itself.
fn numbered_name(filename: &str, number: u32) -> String {
    if number == 0 {
        return filename.to_owned();
    }
    match filename.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} ({number}).{extension}"),
        _ => format!("{filename} ({number})"),
    }
}

/// Copies `source` to a new file at `destination`, failing with
/// `AlreadyExists` if anything is there.
fn copy_new(source: &Path, destination: &Path) -> io::Result<()> {
    let mut input = fs::File::open(source)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    if let Err(error) = io::copy(&mut input, &mut output) {
        drop(output);
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    output.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zmail-attachments-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the scratch dir is created");
        dir
    }

    #[test]
    fn numbered_names_go_before_the_extension() {
        assert_eq!(numbered_name("report.pdf", 0), "report.pdf");
        assert_eq!(numbered_name("report.pdf", 1), "report (1).pdf");
        assert_eq!(numbered_name("archive.tar.gz", 2), "archive.tar (2).gz");
        assert_eq!(numbered_name("README", 3), "README (3)");
    }

    #[test]
    fn saving_never_overwrites_an_existing_file() {
        let dir = scratch("unique");
        let source = dir.join("blob");
        fs::write(&source, b"new").expect("the source is written");
        let folder = dir.join("out");
        fs::create_dir_all(&folder).expect("the folder is created");
        fs::write(folder.join("report.pdf"), b"old").expect("the existing file is written");

        let first = copy_to_unique_path(&source, &folder, "report.pdf").expect("saves");
        let second = copy_to_unique_path(&source, &folder, "report.pdf").expect("saves");
        assert_eq!(first, folder.join("report (1).pdf"));
        assert_eq!(second, folder.join("report (2).pdf"));
        assert_eq!(fs::read(folder.join("report.pdf")).expect("reads"), b"old");
        assert_eq!(fs::read(&first).expect("reads"), b"new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_open_cache_is_private_and_keeps_names_inside_it() {
        let dir = scratch("cache");
        let source = dir.join("blob");
        fs::write(&source, b"payload").expect("the source is written");
        let cache = dir.join(OPEN_CACHE_DIR);

        let path = copy_to_open_cache(&source, &cache, 7, "../../escape.txt").expect("copies");
        assert_eq!(path, cache.join("7").join("_.._escape.txt"));
        assert_eq!(fs::read(&path).expect("reads"), b"payload");
        // Opening again replaces the cached copy.
        fs::write(&source, b"updated").expect("the source is rewritten");
        let again = copy_to_open_cache(&source, &cache, 7, "../../escape.txt").expect("copies");
        assert_eq!(again, path);
        assert_eq!(fs::read(&path).expect("reads"), b"updated");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [&cache, &cache.join("7")] {
                let mode = fs::metadata(directory).expect("stat").permissions().mode();
                assert_eq!(mode & 0o777, 0o700, "{}", directory.display());
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
