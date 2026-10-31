//! Files for compose: the path a file prompt (attach, read into the body)
//! completes with Tab, and the folder listing `^T` browses.
//!
//! File names are the user's own, but a downloaded file can be named
//! anything, escape sequences included, so a name is shown filtered
//! ([`sanitize_line`]) and Tab never completes to a name that the filter
//! would change: such a file can still be picked in the `^T` browser,
//! which keeps the real path.

use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use zmail_security::terminal::sanitize_line;

/// The most entries a folder listing reads.
const LISTING_LIMIT: usize = 2_000;

/// `~` and `~/…` as the home folder.
pub fn expand_home(input: &str) -> PathBuf {
    let home = std::env::home_dir();
    match (input, home) {
        ("~", Some(home)) => home,
        (input, Some(home)) if input.starts_with("~/") || input.starts_with("~\\") => {
            home.join(&input[2..])
        }
        (input, _) => PathBuf::from(input),
    }
}

/// Whether `ch` separates folders in a typed path.
fn is_separator(ch: char) -> bool {
    ch == '/' || (cfg!(windows) && ch == '\\')
}

/// What Tab can complete `input` to: each entry of the folder it names
/// whose name starts with what follows the last separator, as the full
/// typed path (folders end with a separator). Hidden entries only when
/// the name typed starts with a dot.
pub fn path_candidates(input: &str) -> Vec<String> {
    let split = input
        .char_indices()
        .rfind(|(_, ch)| is_separator(*ch))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    let (folder, prefix) = input.split_at(split);
    let listed = if folder.is_empty() {
        PathBuf::from(".")
    } else {
        expand_home(folder)
    };
    let Ok(entries) = std::fs::read_dir(&listed) else {
        return Vec::new();
    };
    let mut candidates: Vec<String> = entries
        .take(LISTING_LIMIT)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with(prefix)
                || (name.starts_with('.') && !prefix.starts_with('.'))
                || sanitize_line(&name) != name
            {
                return None;
            }
            let folder_entry = entry.path().is_dir();
            Some(format!(
                "{folder}{name}{}",
                if folder_entry {
                    MAIN_SEPARATOR.to_string()
                } else {
                    String::new()
                }
            ))
        })
        .collect();
    candidates.sort();
    candidates
}

/// The longest start every candidate shares.
fn common_prefix(candidates: &[String]) -> String {
    let Some(first) = candidates.first() else {
        return String::new();
    };
    let mut length = first.len();
    for candidate in &candidates[1..] {
        length = first
            .char_indices()
            .zip(candidate.chars())
            .take_while(|((_, left), right)| left == right)
            .last()
            .map_or(0, |((index, ch), _)| index + ch.len_utf8())
            .min(length);
    }
    first[..length].to_owned()
}

/// A typed path and what Tab found for it: Tab completes as far as the
/// matches agree, then cycles through them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathInput {
    /// What has been typed, pasted or completed.
    pub input: String,
    /// What the last Tab found, shown until the field changes.
    pub matches: Option<Vec<String>>,
    /// Which match Tab filled in, once Tab cycles.
    pub cycle: Option<usize>,
}

impl PathInput {
    /// Tab (or Shift-Tab, `forward` false).
    pub fn complete(&mut self, forward: bool) {
        if let (Some(matches), Some(cycle)) = (&self.matches, self.cycle)
            && matches.len() > 1
        {
            let count = matches.len();
            let next = if forward {
                (cycle + 1) % count
            } else {
                (cycle + count - 1) % count
            };
            self.cycle = Some(next);
            self.input = matches[next].clone();
            return;
        }
        let matches = path_candidates(&self.input);
        match matches.as_slice() {
            [] => {}
            [only] => self.input = only.clone(),
            _ => {
                let prefix = common_prefix(&matches);
                if prefix.len() > self.input.len() {
                    self.input = prefix;
                } else {
                    let first = if forward { 0 } else { matches.len() - 1 };
                    self.cycle = Some(first);
                    self.input = matches[first].clone();
                }
            }
        }
        self.matches = Some(matches);
    }

    /// The field changed: forget what Tab found.
    pub fn edited(&mut self) {
        self.matches = None;
        self.cycle = None;
    }

    /// The path typed, with `~` expanded.
    pub fn path(&self) -> PathBuf {
        expand_home(self.input.trim())
    }
}

/// An entry of the `^T` file browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEntry {
    /// The name shown, filtered; folders end with a separator.
    pub label: String,
    /// The real path.
    pub path: PathBuf,
    /// Whether it is a folder (or the parent, `..`).
    pub folder: bool,
}

/// The entries of `folder` for the file browser: its parent first, then
/// folders, then files, by name; hidden entries left out.
pub fn browse(folder: &Path) -> std::io::Result<Vec<BrowserEntry>> {
    let mut entries: Vec<BrowserEntry> = std::fs::read_dir(folder)?
        .take(LISTING_LIMIT)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            let folder = path.is_dir();
            Some(BrowserEntry {
                label: format!(
                    "{}{}",
                    sanitize_line(&name),
                    if folder {
                        MAIN_SEPARATOR.to_string()
                    } else {
                        String::new()
                    }
                ),
                path,
                folder,
            })
        })
        .collect();
    entries.sort_by(|left, right| {
        right
            .folder
            .cmp(&left.folder)
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
    });
    if let Some(parent) = folder.parent() {
        entries.insert(
            0,
            BrowserEntry {
                label: format!("..{MAIN_SEPARATOR}"),
                path: parent.to_path_buf(),
                folder: true,
            },
        );
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::path::MAIN_SEPARATOR;

    use super::{PathInput, browse, path_candidates};

    fn scratch(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "zmail-tui-paths-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(directory.join("reports")).expect("folders");
        for file in ["report-q3.pdf", "report-q4.pdf", "notes.txt", ".hidden"] {
            std::fs::write(directory.join(file), b"x").expect("files");
        }
        directory
    }

    #[test]
    fn tab_completes_a_path_as_far_as_the_entries_agree_then_cycles() {
        let directory = scratch("complete");
        let base = format!("{}{MAIN_SEPARATOR}", directory.display());
        let mut field = PathInput {
            input: format!("{base}rep"),
            ..PathInput::default()
        };
        field.complete(true);
        assert_eq!(field.input, format!("{base}report"));
        field.complete(true);
        assert_eq!(
            field.input,
            format!("{base}report-q3.pdf"),
            "the first match"
        );
        field.complete(true);
        assert_eq!(field.input, format!("{base}report-q4.pdf"));
        field.complete(true);
        assert_eq!(
            field.input,
            format!("{base}reports{MAIN_SEPARATOR}"),
            "folders end with a separator"
        );
        field.input = format!("{base}no");
        field.edited();
        field.complete(true);
        assert_eq!(field.input, format!("{base}notes.txt"));
        assert!(
            path_candidates(&base)
                .iter()
                .all(|candidate| !candidate.ends_with(".hidden")),
            "hidden files only when a dot is typed"
        );
        assert_eq!(
            path_candidates(&format!("{base}.h")),
            [format!("{base}.hidden")]
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn the_browser_lists_the_parent_then_folders_then_files() {
        let directory = scratch("browse");
        let labels: Vec<String> = browse(&directory)
            .expect("the folder lists")
            .into_iter()
            .map(|entry| entry.label)
            .collect();
        assert_eq!(
            labels,
            [
                format!("..{MAIN_SEPARATOR}"),
                format!("reports{MAIN_SEPARATOR}"),
                "notes.txt".to_owned(),
                "report-q3.pdf".to_owned(),
                "report-q4.pdf".to_owned(),
            ]
        );
        let _ = std::fs::remove_dir_all(directory);
    }
}
