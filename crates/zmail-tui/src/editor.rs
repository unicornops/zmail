//! The external editor, opt-in (ADR 0007, section 6; Rob's decision):
//! Compose's `^_` hands the message text to `$VISUAL` or `$EDITOR`.
//!
//! The store is encrypted (ADR 0005), and this is the one place a draft
//! leaves it: while the editor is open, the text sits unencrypted in a
//! temporary file. To keep that short and private, the file is made in a
//! fresh folder that only the user can open (0700 on Unix, the file
//! itself 0600; on Windows the user's own temporary folder), and when the
//! editor exits, whether it succeeded, failed or never started, every file
//! in that folder (the draft, and any swap or backup file the editor left)
//! is overwritten with zeros and removed, then the folder. Setup says so
//! where the option is turned on.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use zmail_security::terminal::sanitize;

use crate::clipboard::split_command;

/// Tells apart the folders made by one process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The draft file's name inside its folder.
const FILE_NAME: &str = "zmail-draft.txt";

/// The editor to run: `$VISUAL`, else `$EDITOR`, split into the program
/// and its arguments as a copy command is (no shell).
pub fn editor_command() -> Option<Vec<String>> {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| split_command(&value))
        .find(|command| !command.is_empty())
}

/// The draft's text in a temporary file, in a folder of its own; dropping
/// it overwrites and removes both.
pub struct TempDraft {
    folder: PathBuf,
    file: PathBuf,
}

impl TempDraft {
    /// Writes `text` to a new file in a new private folder under `base`.
    pub fn create(base: &Path, text: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let folder = base.join(format!(
            "zmail-edit-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        // Never an existing folder: `create` fails if the name is taken.
        builder
            .create(&folder)
            .with_context(|| format!("couldn't make a private folder in {}", base.display()))?;
        let draft = Self {
            file: folder.join(FILE_NAME),
            folder,
        };
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&draft.file)
            .context("couldn't make the temporary file")?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .context("couldn't write the temporary file")?;
        Ok(draft)
    }

    /// The file the editor opens.
    pub fn path(&self) -> &Path {
        &self.file
    }

    /// The folder it is in.
    #[cfg(test)]
    pub fn folder(&self) -> &Path {
        &self.folder
    }
}

/// Overwrites the file at `path` with zeros, as long as it is.
fn overwrite(path: &Path) {
    let Ok(length) = fs::metadata(path).map(|metadata| metadata.len()) else {
        return;
    };
    if let Ok(mut file) = fs::OpenOptions::new().write(true).open(path) {
        let zeros = vec![0_u8; 64 * 1024];
        let mut left = length;
        while left > 0 {
            let chunk = left.min(zeros.len() as u64) as usize;
            if file.write_all(&zeros[..chunk]).is_err() {
                break;
            }
            left -= chunk as u64;
        }
        let _ = file.sync_all();
    }
}

impl Drop for TempDraft {
    fn drop(&mut self) {
        // The draft, and whatever the editor left beside it.
        if let Ok(entries) = fs::read_dir(&self.folder) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    overwrite(&path);
                }
            }
        }
        let _ = fs::remove_dir_all(&self.folder);
    }
}

/// Runs `command` on a temporary copy of `text` and returns what the
/// editor saved, filtered for the terminal. An editor that adds a final
/// newline the text didn't have loses it again. The copy is overwritten
/// and removed however the editor ends.
pub fn edit(base: &Path, text: &str, command: &[String]) -> Result<String> {
    let Some((program, arguments)) = command.split_first() else {
        bail!("set VISUAL or EDITOR to the editor to use");
    };
    let draft = TempDraft::create(base, text)?;
    let status = Command::new(program)
        .args(arguments)
        .arg(draft.path())
        .status()
        .with_context(|| format!("couldn't start {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}; the text is as it was");
    }
    let edited = fs::read(draft.path()).context("couldn't read the text back")?;
    drop(draft);
    let mut edited = sanitize(&String::from_utf8_lossy(&edited));
    if edited.ends_with('\n') && !text.ends_with('\n') {
        edited.pop();
    }
    Ok(edited)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{TempDraft, edit};

    fn base(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "zmail-tui-editor-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base).expect("the base folder");
        base
    }

    fn empty(base: &PathBuf) -> bool {
        std::fs::read_dir(base).expect("lists").next().is_none()
    }

    #[test]
    fn the_temporary_file_is_private_and_gone_when_dropped() {
        let base = base("private");
        let draft = TempDraft::create(&base, "secret draft").expect("created");
        assert_eq!(
            std::fs::read_to_string(draft.path()).expect("reads"),
            "secret draft"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &std::path::Path| {
                std::fs::metadata(path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777
            };
            assert_eq!(mode(draft.folder()), 0o700);
            assert_eq!(mode(draft.path()), 0o600);
        }
        // A swap file the editor left goes too.
        std::fs::write(draft.folder().join(".zmail-draft.txt.swp"), "swap").expect("swap");
        drop(draft);
        assert!(empty(&base), "the folder and its files are removed");
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn the_editors_text_comes_back_filtered_and_the_file_is_removed() {
        let base = base("edit");
        let command = |script: &str| vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()];
        // `sh -c script path`: the path is $0.
        let edited = edit(
            &base,
            "Hello",
            &command(r#"printf ' world\033[2J\n' >> "$0""#),
        )
        .expect("the editor ran");
        assert_eq!(edited, "Hello world");
        assert!(empty(&base));

        let failed = edit(&base, "Hello", &command("exit 3"));
        assert!(failed.is_err());
        assert!(empty(&base), "removed after a failure too");

        let missing = edit(&base, "Hello", &["zmail-no-such-editor".to_owned()]);
        assert!(missing.is_err());
        assert!(empty(&base), "and when the editor never started");
        assert!(edit(&base, "Hello", &[]).is_err());
        let _ = std::fs::remove_dir_all(base);
    }
}
