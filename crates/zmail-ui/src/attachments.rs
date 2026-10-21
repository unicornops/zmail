//! Opening and saving received attachments: the workspace's handlers for
//! the attachment cards' Open and Save As buttons and for Save All.
//!
//! - **Open** decrypts the attachment into a private cache directory (owner
//!   only on macOS and Linux) and hands it to the system's default app. A
//!   program or script, or a web page or SVG that would run scripts in the
//!   browser (`attachment_safety::open_risk`), gets a warning first, and
//!   opens only on "Open Anyway". The other app can only read a plaintext
//!   file, so the copy is kept short-lived (ADR 0005 §2): on Linux the
//!   cache is in `$XDG_RUNTIME_DIR`, a per-user RAM file system; copies
//!   older than an hour are removed while Zmail runs
//!   ([`prune_open_cache`]), and the whole cache when it quits and again
//!   when the window opens.
//! - **Save As** asks for a path with the platform save dialog, which
//!   itself asks before replacing an existing file.
//! - **Save All** ([`SaveAttachments`](crate::actions::SaveAttachments)
//!   for the conversation, [`SaveMessageAttachments`] for one message)
//!   asks for a folder and saves every attachment there. A name that is taken gets a number ("report (1).pdf"); no
//!   file is ever overwritten.
//!
//! The stored attachments are encrypted; every copy here is decrypted from
//! them. Every name written to disk goes through
//! `attachment_safety::sanitize_filename` first. The copies run on the
//! background executor.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use gpui::{
    AnyElement, App, Context, IntoElement, PathPromptOptions, SharedString, Subscription, Window,
    div, prelude::*,
};
use zmail_core::{ReceivedAttachment, text::count_noun};
use zmail_security::attachment_safety::{OpenRisk, open_risk, sanitize_filename};
use zmail_store::AttachmentBlob;

use crate::WorkspaceView;
use crate::actions::{OpenAttachment, SaveAttachmentAs, SaveMessageAttachments};
use crate::components::{Button, Modal, ModalState};

/// Directory that opened attachments are decrypted into, under
/// `$XDG_RUNTIME_DIR/zmail` on Linux and the store's directory elsewhere.
const OPEN_CACHE_DIR: &str = "opened-attachments";

/// How long an opened copy stays in the open cache while Zmail runs: the
/// app it was handed to has read it by then, and the plaintext should not
/// outlive that by much.
const OPEN_CACHE_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// How often the open cache is pruned while Zmail runs.
const OPEN_CACHE_PRUNE_INTERVAL: Duration = Duration::from_secs(10 * 60);

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
    /// Empties the open cache when the app quits.
    _quit_subscription: Option<Subscription>,
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
            _quit_subscription: None,
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

    /// Open: warn first for anything that can run code, otherwise open now.
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
        if open_risk(&attachment.filename, &attachment.content_type) != OpenRisk::None {
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

    /// Decrypts the attachment into the private cache and opens it with
    /// the system's default app.
    fn open_attachment_now(&mut self, attachment: &ReceivedAttachment, cx: &mut Context<Self>) {
        let blob = match self.store.attachment_blob(attachment.id) {
            Ok(blob) => blob,
            Err(error) => return self.show_error(format!("{error:#}"), cx),
        };
        let cache_root = self.open_cache_root();
        let copy = cx.background_spawn(async move { copy_to_open_cache(&blob, &cache_root) });
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
        let blob = match self.store.attachment_blob(action.attachment_id) {
            Ok(blob) => blob,
            Err(error) => return self.show_error(format!("{error:#}"), cx),
        };
        let directory = self
            .attachments
            .last_save_dir
            .clone()
            .unwrap_or_else(default_save_dir);
        let suggested = sanitize_filename(blob.filename());
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
                .background_spawn(async move { blob.save_to(&target).map(|_| ()) })
                .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(()) => {
                    view.attachments.last_save_dir = destination.parent().map(Path::to_owned);
                    view.show_status(format!("Saved {}.", destination.display()), cx);
                }
                Err(error) => view.show_error(
                    format!("Could not save {}: {error:#}", destination.display()),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Save All for one message: its More menu's Save Attachments.
    pub(crate) fn save_message_attachments(
        &mut self,
        action: &SaveMessageAttachments,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_all_attachments(Some(action.message_id), cx);
    }

    /// Save All: asks for a folder and saves every attachment of the open
    /// conversation (or of its message `message_id`) into it, never
    /// overwriting a file. The Save Attachments command
    /// (`SaveAttachments`) saves the whole conversation.
    pub(crate) fn save_all_attachments(&mut self, message_id: Option<i64>, cx: &mut Context<Self>) {
        let Some(thread) = self.selected_thread_view() else {
            return self.show_error("Select a conversation first.", cx);
        };
        let mut files = Vec::new();
        for attachment in thread
            .messages
            .iter()
            .filter(|message| message_id.is_none_or(|id| message.id == id))
            .flat_map(|message| &message.attachments)
        {
            match self.store.attachment_blob(attachment.id) {
                Ok(blob) => files.push(blob),
                Err(error) => return self.show_error(format!("{error:#}"), cx),
            }
        }
        if files.is_empty() {
            return self.show_error(
                if message_id.is_some() {
                    "This message has no attachments."
                } else {
                    "This conversation has no attachments."
                },
                cx,
            );
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
                        .map(|blob| {
                            copy_to_unique_path(
                                |destination| save_new(blob, destination),
                                &target,
                                &sanitize_filename(blob.filename()),
                            )
                            .map_err(|error| format!("{}: {error}", blob.filename()))
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

    /// Keeps the open cache short-lived. Called once when the window
    /// opens: removes every copy left from an earlier run, then removes
    /// copies older than [`OPEN_CACHE_MAX_AGE`] every
    /// [`OPEN_CACHE_PRUNE_INTERVAL`], and the whole cache when the app
    /// quits.
    pub(crate) fn prune_open_cache_later(&mut self, cx: &mut Context<Self>) {
        let cache_root = self.open_cache_root();
        let startup_root = cache_root.clone();
        cx.background_spawn(async move {
            // A failed prune leaves files behind; it never blocks opening.
            let _ = prune_open_cache(&startup_root, SystemTime::now());
        })
        .detach();

        let periodic_root = cache_root.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(OPEN_CACHE_PRUNE_INTERVAL)
                    .await;
                if this.upgrade().is_none() {
                    break;
                }
                let root = periodic_root.clone();
                cx.background_spawn(async move {
                    let cutoff = SystemTime::now()
                        .checked_sub(OPEN_CACHE_MAX_AGE)
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    let _ = prune_open_cache(&root, cutoff);
                })
                .await;
            }
        })
        .detach();

        self.attachments._quit_subscription = Some(cx.on_app_quit(move |_, _| {
            let _ = fs::remove_dir_all(&cache_root);
            async {}
        }));
    }

    /// Where opened attachments are decrypted to. Tests keep the cache
    /// beside their ephemeral store, away from the user's runtime directory.
    fn open_cache_root(&self) -> PathBuf {
        let runtime_dir = if cfg!(test) {
            None
        } else {
            std::env::var_os("XDG_RUNTIME_DIR")
        };
        open_cache_root(
            &self.store_dir(),
            runtime_dir,
            &self.store.store_key().id().to_string(),
        )
    }

    /// The warning shown before opening a program, a script, or a web page
    /// or SVG that would run scripts in the browser.
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
            .description(open_warning_text(open_risk(
                &attachment.filename,
                &attachment.content_type,
            )))
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

/// The open cache for the store in `store_dir`: in the per-user RAM file
/// system `$XDG_RUNTIME_DIR` on Linux when there is one, so the plaintext
/// copies never reach the disk (apart from swap), and beside the store
/// elsewhere. In the runtime directory each store (named by its key id)
/// has its own cache, so one store's pruning never touches another's.
fn open_cache_root(
    store_dir: &Path,
    runtime_dir: Option<std::ffi::OsString>,
    store_id: &str,
) -> PathBuf {
    if cfg!(target_os = "linux")
        && let Some(runtime_dir) = runtime_dir.map(PathBuf::from)
        && runtime_dir.is_absolute()
        && runtime_dir.is_dir()
    {
        return runtime_dir
            .join("zmail")
            .join(store_id)
            .join(OPEN_CACHE_DIR);
    }
    store_dir.join(OPEN_CACHE_DIR)
}

/// Decrypts `blob` to `<cache_root>/<random>/<sanitised name>` and returns
/// that path. The directories are created owner-only on Unix; the random
/// directory name keeps the path unguessable and each open separate.
pub(crate) fn copy_to_open_cache(blob: &AttachmentBlob, cache_root: &Path) -> io::Result<PathBuf> {
    create_private_dir(cache_root)?;
    let directory = cache_root.join(zmail_security::vault::random_hex(8));
    create_private_dir(&directory)?;
    let destination = directory.join(sanitize_filename(blob.filename()));
    save_new(blob, &destination)?;
    Ok(destination)
}

/// Decrypts `blob` to a new file at `destination`, failing with
/// `AlreadyExists` if anything is there.
fn save_new(blob: &AttachmentBlob, destination: &Path) -> io::Result<()> {
    blob.save_new(destination).map(|_| ()).map_err(|error| {
        let kind = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<io::Error>())
            .map_or(io::ErrorKind::Other, io::Error::kind);
        io::Error::new(kind, format!("{error:#}"))
    })
}

/// The open warning's explanation for a file of `risk`.
fn open_warning_text(risk: OpenRisk) -> &'static str {
    match risk {
        OpenRisk::ActiveContent => {
            "This file opens in your web browser and can run scripts there. Fake sign-in \
             pages are often sent this way. Open it only if you expected it and trust the \
             sender."
        }
        OpenRisk::Program | OpenRisk::None => {
            "This kind of file can run programs on your computer. Open it only if you \
             expected it and trust the sender."
        }
    }
}

/// Removes every attachment directory in the open cache whose files are
/// all older than `cutoff`, and returns how many it removed. A missing
/// cache is not an error.
pub(crate) fn prune_open_cache(cache_root: &Path, cutoff: SystemTime) -> io::Result<usize> {
    let entries = match fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries {
        let directory = entry?.path();
        if directory.is_dir() && newest_modification(&directory)? < cutoff {
            fs::remove_dir_all(&directory)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// The latest modification time of the files in `directory`, or the
/// directory's own when it is empty.
fn newest_modification(directory: &Path) -> io::Result<SystemTime> {
    let mut newest = None;
    for entry in fs::read_dir(directory)? {
        let modified = entry?.metadata()?.modified()?;
        newest = Some(newest.map_or(modified, |newest: SystemTime| newest.max(modified)));
    }
    match newest {
        Some(newest) => Ok(newest),
        None => fs::metadata(directory)?.modified(),
    }
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

/// Writes a file into `directory` as `filename`, or the first free
/// numbered name ("report (1).pdf", "report (2).pdf", …), with `write_new`,
/// which must fail with `AlreadyExists` when its destination exists.
/// Returns the path written. Never replaces an existing file, even one
/// created meanwhile.
pub(crate) fn copy_to_unique_path(
    write_new: impl Fn(&Path) -> io::Result<()>,
    directory: &Path,
    filename: &str,
) -> io::Result<PathBuf> {
    for number in 0..=MAX_NUMBERED_NAMES {
        let destination = directory.join(numbered_name(filename, number));
        match write_new(&destination) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn age_file(path: &Path, age: Duration) {
        fs::File::options()
            .write(true)
            .open(path)
            .expect("the file opens for writing")
            .set_modified(SystemTime::now() - age)
            .expect("the modification time is set");
    }

    /// A copy in the open cache, as [`copy_to_open_cache`] lays it out.
    fn cached_copy(cache: &Path, directory: &str, name: &str) -> PathBuf {
        let directory = cache.join(directory);
        fs::create_dir_all(&directory).expect("the copy's directory is created");
        let path = directory.join(name);
        fs::write(&path, b"pdf").expect("the copy is written");
        path
    }

    #[test]
    fn pruning_removes_only_copies_older_than_the_cutoff() {
        let root = scratch("prune");
        let cache = root.join(OPEN_CACHE_DIR);
        let old = cached_copy(&cache, "a1", "old.pdf");
        let fresh = cached_copy(&cache, "b2", "fresh.pdf");
        age_file(&old, Duration::from_secs(2 * 60 * 60));

        let cutoff = SystemTime::now() - OPEN_CACHE_MAX_AGE;
        assert_eq!(prune_open_cache(&cache, cutoff).expect("prune"), 1);
        assert!(!old.exists());
        assert!(!cache.join("a1").exists());
        assert!(fresh.exists());

        // At startup everything left from the last run goes.
        assert_eq!(
            prune_open_cache(&cache, SystemTime::now() + Duration::from_secs(1)).expect("prune"),
            1
        );
        assert!(!fresh.exists());

        assert_eq!(
            prune_open_cache(&root.join("missing"), cutoff).expect("a missing cache"),
            0
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn web_pages_get_their_own_open_warning() {
        assert!(open_warning_text(OpenRisk::ActiveContent).contains("web browser"));
        assert!(open_warning_text(OpenRisk::Program).contains("run programs"));
    }

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
    fn the_open_cache_is_in_the_runtime_dir_on_linux_one_per_store() {
        let dir = scratch("cache-root");
        let store_dir = dir.join("store");
        let runtime = dir.join("run");
        fs::create_dir_all(&runtime).expect("the runtime dir is created");

        let root = open_cache_root(&store_dir, Some(runtime.clone().into()), "abc");
        if cfg!(target_os = "linux") {
            assert_eq!(root, runtime.join("zmail").join("abc").join(OPEN_CACHE_DIR));
        } else {
            assert_eq!(root, store_dir.join(OPEN_CACHE_DIR));
        }
        assert_eq!(
            open_cache_root(&store_dir, None, "abc"),
            store_dir.join(OPEN_CACHE_DIR)
        );
        assert_eq!(
            open_cache_root(&store_dir, Some("relative".into()), "abc"),
            store_dir.join(OPEN_CACHE_DIR),
            "a relative runtime dir is not trusted"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Writes "new" to a new file, as [`save_new`] does with a blob.
    fn write_new(destination: &Path) -> io::Result<()> {
        use std::io::Write;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?
            .write_all(b"new")
    }

    #[test]
    fn saving_never_overwrites_an_existing_file() {
        let dir = scratch("unique");
        let folder = dir.join("out");
        fs::create_dir_all(&folder).expect("the folder is created");
        fs::write(folder.join("report.pdf"), b"old").expect("the existing file is written");

        let first = copy_to_unique_path(write_new, &folder, "report.pdf").expect("saves");
        let second = copy_to_unique_path(write_new, &folder, "report.pdf").expect("saves");
        assert_eq!(first, folder.join("report (1).pdf"));
        assert_eq!(second, folder.join("report (2).pdf"));
        assert_eq!(fs::read(folder.join("report.pdf")).expect("reads"), b"old");
        assert_eq!(fs::read(&first).expect("reads"), b"new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_open_cache_is_private_and_keeps_names_inside_it() {
        let dir = scratch("cache");
        let store = zmail_store::LocalStore::open_ephemeral().expect("the store opens");
        let account_id = store
            .create_demo_account(zmail_core::ProtocolKind::Imap)
            .expect("the account is created");
        let target = store
            .sync_target(account_id)
            .expect("the target loads")
            .expect("the account exists");
        let storage_path = target
            .blobs
            .write("payload", b"payload")
            .expect("the blob is written");
        let mut snapshot = zmail_core::AccountSyncSnapshot {
            checkpoint: "test".to_owned(),
            detail: "test".to_owned(),
            identities: None,
            mailboxes: Vec::new(),
        };
        snapshot.mailboxes.push(zmail_core::MailboxSnapshot {
            name: "Inbox".to_owned(),
            role: Some("inbox".to_owned()),
            threads: vec![zmail_core::ThreadSnapshot {
                remote_thread_id: "t".to_owned(),
                subject: "s".to_owned(),
                participants: "p".to_owned(),
                preview: "p".to_owned(),
                is_flagged: false,
                has_attachments: true,
                unread_count: 0,
                latest_message_at: "2026-03-24T09:00:00Z".to_owned(),
                messages: vec![zmail_core::MessageSnapshot {
                    remote_id: "m".to_owned(),
                    identity_email: "me@example.test".to_owned(),
                    sender: "a@example.test".to_owned(),
                    reply_to: None,
                    recipients: "me@example.test".to_owned(),
                    subject: "s".to_owned(),
                    body_preview: "p".to_owned(),
                    body_text: Some("p".to_owned()),
                    sanitized_html: None,
                    has_remote_content: false,
                    is_read: true,
                    is_flagged: false,
                    received_at: "2026-03-24T09:00:00Z".to_owned(),
                    attachments: vec![zmail_core::MessageAttachment {
                        filename: "../../escape.txt".to_owned(),
                        content_type: "text/plain".to_owned(),
                        size_bytes: 7,
                        storage_path,
                    }],
                }],
            }],
            ..Default::default()
        });
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("the snapshot applies");
        let thread = store.list_all_mailboxes().expect("mailboxes")[0].id;
        let thread = store.list_threads(thread, 0..1).expect("threads")[0].id;
        let attachment = store
            .thread_view(thread)
            .expect("the thread loads")
            .expect("the thread exists")
            .messages[0]
            .attachments[0]
            .id;
        let blob = store.attachment_blob(attachment).expect("the blob loads");
        let cache = dir.join(OPEN_CACHE_DIR);

        let path = copy_to_open_cache(&blob, &cache).expect("copies");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("_.._escape.txt")
        );
        assert_eq!(path.parent().and_then(Path::parent), Some(cache.as_path()));
        assert_eq!(fs::read(&path).expect("reads"), b"payload");
        // Each open gets its own unguessable directory.
        let again = copy_to_open_cache(&blob, &cache).expect("copies");
        assert_ne!(again.parent(), path.parent());
        assert_eq!(
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::len),
            Some(16)
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let copy_dir = path.parent().expect("the copy has a directory");
            for directory in [cache.as_path(), copy_dir] {
                let mode = fs::metadata(directory).expect("stat").permissions().mode();
                assert_eq!(mode & 0o777, 0o700, "{}", directory.display());
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
