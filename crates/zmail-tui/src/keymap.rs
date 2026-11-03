//! The user's keymap file: rebinds the terminal's keys per screen (ADR
//! 0007, section 2: "the keymap is data, so a user keymap file can rebind
//! it").
//!
//! The file is `tui-keymap.json` beside the mail store, in the format
//! family of the desktop app's keymap (`zmail-ui/src/keymap.rs`, a subset
//! of Zed's): a JSON array of sections, each with an optional `context`
//! (the screen) and a `bindings` object from keys to commands:
//!
//! ```json
//! [
//!   { "context": "MessageIndex", "bindings": { "x": "Delete", "d": null } },
//!   { "bindings": { "ctrl-p": "Palette" } }
//! ]
//! ```
//!
//! - A section without a context applies on every screen listed in
//!   [`CONTEXTS`]; one with a context wins over it there.
//! - Keys are written as in Zed: `x`, `ctrl-x`, `alt-x`, `shift-tab`,
//!   `enter`, `space`, `pageup`, `f5`, `<`. Letters work in either case,
//!   so `D` and `shift-d` are `d`.
//! - A command is a [`Cmd`] by name ([`ACTIONS`]); `null` makes the key do
//!   nothing there.
//! - A user binding wins over the built-in key, which keeps working
//!   unless something else is bound to it. The command bar, Help and the
//!   palette show the user's key for a command.
//! - Esc and Ctrl-L are the terminal's own and can't be bound; nor can
//!   Compose's and the account forms' keys, which are text.
//!
//! A file that doesn't parse, or names an unknown screen, key or command,
//! is ignored as a whole and the status line says why: a broken keymap
//! never stops the TUI from starting, nor half-applies.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use zmail_security::terminal::sanitize_line;

use crate::app::{App, Cmd, Screen};

/// The keymap file's name, beside the mail store.
pub const KEYMAP_FILE: &str = "tui-keymap.json";

/// The screens a section's `context` can name, and their names.
pub const CONTEXTS: &[(&str, Screen)] = &[
    ("MainMenu", Screen::MainMenu),
    ("FolderList", Screen::FolderList),
    ("MessageIndex", Screen::Index),
    ("MessageText", Screen::Message),
    ("AttachmentIndex", Screen::Attachments),
    ("Setup", Screen::Setup),
    ("Help", Screen::Help),
    ("Postponed", Screen::Postponed),
    ("Outbox", Screen::Outbox),
    ("Accounts", Screen::Accounts),
];

/// The commands a key can be bound to, by name.
pub const ACTIONS: &[(&str, Cmd)] = &[
    ("Help", Cmd::Help),
    ("Back", Cmd::Back),
    ("Select", Cmd::Select),
    ("Prev", Cmd::Prev),
    ("Next", Cmd::Next),
    ("PageUp", Cmd::PageUp),
    ("PageDown", Cmd::PageDown),
    ("Home", Cmd::Home),
    ("End", Cmd::End),
    ("WhereIs", Cmd::WhereIs),
    ("Threads", Cmd::Threads),
    ("Preview", Cmd::Preview),
    ("MainMenu", Cmd::MainMenu),
    ("FolderList", Cmd::FolderList),
    ("Index", Cmd::Index),
    ("Setup", Cmd::Setup),
    ("Compose", Cmd::Compose),
    ("Reply", Cmd::Reply),
    ("Forward", Cmd::Forward),
    ("Outbox", Cmd::Outbox),
    ("Retry", Cmd::Retry),
    ("CancelSend", Cmd::CancelSend),
    ("DeleteDraft", Cmd::DeleteDraft),
    ("AddressBook", Cmd::AddressBook),
    ("Quit", Cmd::Quit),
    ("OtherCmds", Cmd::OtherCmds),
    ("NextLink", Cmd::NextLink),
    ("PrevLink", Cmd::PrevLink),
    ("Mark", Cmd::Mark),
    ("Copy", Cmd::Copy),
    ("Delete", Cmd::Delete),
    ("Undo", Cmd::Undo),
    ("Archive", Cmd::Archive),
    ("Save", Cmd::Save),
    ("Flag", Cmd::Flag),
    ("ReadState", Cmd::ReadState),
    ("ToggleSelected", Cmd::ToggleSelected),
    ("Apply", Cmd::Apply),
    ("Accounts", Cmd::Accounts),
    ("AddAccount", Cmd::AddAccount),
    ("ChangeAccount", Cmd::ChangeAccount),
    ("DeleteAccount", Cmd::DeleteAccount),
    ("Attachments", Cmd::Attachments),
    ("SaveAttachment", Cmd::SaveAttachment),
    ("SaveAllAttachments", Cmd::SaveAllAttachments),
    ("LooksSafe", Cmd::LooksSafe),
    ("Palette", Cmd::Palette),
];

/// The name of `cmd` in a keymap file.
pub fn action_name(cmd: Cmd) -> Option<&'static str> {
    ACTIONS
        .iter()
        .find(|(_, action)| *action == cmd)
        .map(|(name, _)| *name)
}

/// The name of `screen` in a keymap file.
pub fn context_name(screen: Screen) -> Option<&'static str> {
    CONTEXTS
        .iter()
        .find(|(_, context)| *context == screen)
        .map(|(name, _)| *name)
}

/// A key as a keymap names it: letters lower case, Shift only on Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// The key.
    pub code: KeyCode,
    /// With Control.
    pub ctrl: bool,
    /// With Alt (Option, Meta).
    pub alt: bool,
}

impl Key {
    /// The key `event` is, as a keymap names it.
    pub fn of(event: KeyEvent) -> Self {
        let code = match event.code {
            KeyCode::Char(ch) => KeyCode::Char(ch.to_ascii_lowercase()),
            code => code,
        };
        Self {
            code,
            ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
            alt: event.modifiers.contains(KeyModifiers::ALT),
        }
    }

    /// Parses Zed's keystroke syntax: `x`, `ctrl-x`, `alt-shift-tab`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut key = Self {
            code: KeyCode::Null,
            ctrl: false,
            alt: false,
        };
        let mut shift = false;
        let mut rest = text;
        // A key can be `-` itself: `ctrl--`, `-`.
        while let Some((modifier, after)) = rest.split_once('-')
            && !after.is_empty()
        {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => key.ctrl = true,
                "alt" | "option" | "meta" => key.alt = true,
                "shift" => shift = true,
                _ => return Err(format!("{text:?} has an unknown modifier {modifier:?}")),
            }
            rest = after;
        }
        let name = rest.to_ascii_lowercase();
        key.code = match name.as_str() {
            "enter" | "return" => KeyCode::Enter,
            "tab" if shift => KeyCode::BackTab,
            "tab" => KeyCode::Tab,
            "space" => KeyCode::Char(' '),
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "escape" | "esc" => {
                return Err(format!(
                    "{text:?}: Esc is the terminal's own and can't be bound"
                ));
            }
            name if name.len() > 1
                && name.starts_with('f')
                && let Ok(number) = name[1..].parse::<u8>()
                && (1..=12).contains(&number) =>
            {
                KeyCode::F(number)
            }
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) if !ch.is_control() => KeyCode::Char(ch.to_ascii_lowercase()),
                    _ => return Err(format!("{text:?} is not a key")),
                }
            }
        };
        if key.ctrl && key.code == KeyCode::Char('l') {
            return Err(format!(
                "{text:?}: Ctrl-L redraws the screen and can't be bound"
            ));
        }
        Ok(key)
    }

    /// How the command bar shows the key: `D`, `^X`, `Ret`, `Spc`.
    pub fn label(self) -> String {
        let name = match self.code {
            KeyCode::Char(' ') => "Spc".to_owned(),
            KeyCode::Char(ch) => ch.to_ascii_uppercase().to_string(),
            KeyCode::Enter => "Ret".to_owned(),
            KeyCode::Tab => "Tab".to_owned(),
            KeyCode::BackTab => "S-Tab".to_owned(),
            KeyCode::Backspace => "BkSp".to_owned(),
            KeyCode::Delete => "Del".to_owned(),
            KeyCode::Insert => "Ins".to_owned(),
            KeyCode::Up => "Up".to_owned(),
            KeyCode::Down => "Down".to_owned(),
            KeyCode::Left => "Left".to_owned(),
            KeyCode::Right => "Right".to_owned(),
            KeyCode::PageUp => "PgUp".to_owned(),
            KeyCode::PageDown => "PgDn".to_owned(),
            KeyCode::Home => "Home".to_owned(),
            KeyCode::End => "End".to_owned(),
            KeyCode::F(number) => format!("F{number}"),
            _ => "?".to_owned(),
        };
        let alt = if self.alt { "M-" } else { "" };
        let ctrl = if self.ctrl { "^" } else { "" };
        format!("{alt}{ctrl}{name}")
    }
}

/// One binding of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The screen, or every screen.
    pub screen: Option<Screen>,
    /// The key.
    pub key: Key,
    /// The command, or nothing (`null`).
    pub cmd: Option<Cmd>,
}

/// The user's keymap: its bindings, and why the file was ignored if it
/// was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserKeymap {
    /// Where the file is.
    pub path: Option<PathBuf>,
    /// Its bindings, in file order.
    pub bindings: Vec<Binding>,
    /// Why the file was ignored.
    pub error: Option<String>,
}

impl UserKeymap {
    /// Reads the keymap at `path`. No file is no keymap; a file that can't
    /// be read or parsed is ignored, with the reason.
    pub fn load(path: &Path) -> Self {
        let mut keymap = match std::fs::read_to_string(path) {
            Ok(source) => Self::parse(&source),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => Self {
                error: Some(format!("it can't be read: {error}")),
                ..Self::default()
            },
        };
        keymap.path = Some(path.to_path_buf());
        keymap
    }

    /// Parses a keymap file's text: every binding, or none and the first
    /// problem.
    pub fn parse(source: &str) -> Self {
        match parse_bindings(source) {
            Ok(bindings) => Self {
                bindings,
                ..Self::default()
            },
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }

    /// What `key` does on `screen` by the user's keymap: `Some(Some(cmd))`
    /// runs `cmd`, `Some(None)` nothing; `None` leaves it to the built-in
    /// keys. A section for the screen wins over one for every screen, and
    /// the later binding over the earlier.
    pub fn lookup(&self, screen: Screen, key: Key) -> Option<Option<Cmd>> {
        let find = |wanted: Option<Screen>| {
            self.bindings
                .iter()
                .rev()
                .find(|binding| binding.screen == wanted && binding.key == key)
                .map(|binding| binding.cmd)
        };
        find(Some(screen)).or_else(|| {
            (context_name(screen).is_some())
                .then(|| find(None))
                .flatten()
        })
    }

    /// The first key the user bound to `cmd` on `screen` (that still runs
    /// it there), else `None`.
    pub fn key_for(&self, screen: Screen, cmd: Cmd) -> Option<Key> {
        self.bindings
            .iter()
            .filter(|binding| {
                binding.cmd == Some(cmd)
                    && (binding.screen == Some(screen)
                        || (binding.screen.is_none() && context_name(screen).is_some()))
            })
            .map(|binding| binding.key)
            .find(|key| self.lookup(screen, *key) == Some(Some(cmd)))
    }

    /// Whether the user's keymap takes the key the bar shows as `label`
    /// away from its built-in command on `screen`: bound to something
    /// else, or to nothing.
    pub fn shadows(&self, screen: Screen, label: &str, cmd: Cmd) -> bool {
        self.bindings
            .iter()
            .filter(|binding| binding.key.label() == label)
            .any(|binding| {
                self.lookup(screen, binding.key)
                    .is_some_and(|bound| bound != Some(cmd))
            })
    }
}

impl App {
    /// Uses `keymap`, the user's, and says on the status line when its
    /// file was ignored.
    pub fn set_keymap(&mut self, keymap: UserKeymap) {
        if let Some(error) = &keymap.error {
            // The reason first, where the status line has room; Help
            // says where the file is.
            self.status = Some(sanitize_line(&format!(
                "Your keymap file isn't used: {error}. Help (?) says where it is"
            )));
        }
        self.keymap = keymap;
    }

    /// Runs `cmd`, which the user's keymap binds the key to, when it works
    /// on this screen: a command of its bar, or one a built-in key runs.
    pub(crate) fn run_bound(&mut self, cmd: Cmd) {
        let works = matches!(cmd, Cmd::Home | Cmd::End)
            || self.builtin_commands().iter().any(|entry| entry.cmd == cmd)
            || builtin_keys().any(|key| self.command_for(key) == Some(cmd));
        if works {
            self.run(cmd);
        } else {
            self.status = Some(format!(
                "{} doesn't work on this screen",
                action_name(cmd).unwrap_or("That command")
            ));
        }
    }

    /// The key to show for `cmd` on `screen`, whose built-in key is
    /// `builtin`: the user's, else the built-in one unless the user's
    /// keymap took it, else none.
    pub(crate) fn key_label(&self, screen: Screen, cmd: Cmd, builtin: &str) -> String {
        match self.keymap.key_for(screen, cmd) {
            Some(key) => key.label(),
            None if self.keymap.shadows(screen, builtin, cmd) => String::new(),
            None => builtin.to_owned(),
        }
    }
}

/// Every key a built-in command can be on, to find which commands a
/// screen's keys run.
fn builtin_keys() -> impl Iterator<Item = KeyEvent> {
    let plain = (' '..='~').map(|ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    let special = [
        KeyCode::Enter,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Home,
        KeyCode::End,
    ]
    .into_iter()
    .map(|code| KeyEvent::new(code, KeyModifiers::NONE));
    let control = [' ', 'c'].map(|ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
    plain.chain(special).chain(control)
}

fn parse_bindings(source: &str) -> Result<Vec<Binding>, String> {
    let sections: Value =
        serde_json::from_str(source).map_err(|error| format!("it is not valid JSON ({error})"))?;
    let sections = sections
        .as_array()
        .ok_or("it must be a JSON array of sections")?;
    let mut bindings = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        let number = index + 1;
        let section = section
            .as_object()
            .ok_or_else(|| format!("section {number} is not an object"))?;
        let screen = match section.get("context") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) => Some(
                CONTEXTS
                    .iter()
                    .find(|(context, _)| context == name)
                    .map(|(_, screen)| *screen)
                    .ok_or_else(|| format!("section {number}: no screen is called {name:?}"))?,
            ),
            Some(_) => return Err(format!("section {number}: context must be a string")),
        };
        let entries = section
            .get("bindings")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("section {number} has no bindings object"))?;
        // In key order: whether serde_json keeps the file's order depends
        // on the build's features, and Help lists them.
        let mut entries: Vec<_> = entries.iter().collect();
        entries.sort_by(|left, right| left.0.cmp(right.0));
        for (keystroke, target) in entries {
            let key =
                Key::parse(keystroke).map_err(|error| format!("section {number}: {error}"))?;
            let cmd = match target {
                Value::Null => None,
                Value::String(name) => Some(
                    ACTIONS
                        .iter()
                        .find(|(action, _)| action == name)
                        .map(|(_, cmd)| *cmd)
                        .ok_or_else(|| {
                            format!(
                                "section {number}, {keystroke:?}: no command is called {name:?}"
                            )
                        })?,
                ),
                _ => {
                    return Err(format!(
                        "section {number}, {keystroke:?}: a command name or null"
                    ));
                }
            };
            bindings.push(Binding { screen, key, cmd });
        }
    }
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> Key {
        Key::parse(text).expect("a key")
    }

    #[test]
    fn keys_parse_as_zed_writes_them_and_show_as_the_bar_does() {
        for (text, label) in [
            ("x", "X"),
            ("X", "X"),
            ("shift-x", "X"),
            ("ctrl-x", "^X"),
            ("alt-x", "M-X"),
            ("ctrl-space", "^Spc"),
            ("space", "Spc"),
            ("enter", "Ret"),
            ("tab", "Tab"),
            ("shift-tab", "S-Tab"),
            ("pageup", "PgUp"),
            ("f5", "F5"),
            ("<", "<"),
            ("-", "-"),
            ("ctrl--", "^-"),
            (":", ":"),
            ("*", "*"),
        ] {
            assert_eq!(key(text).label(), label, "{text}");
        }
        assert_eq!(key("D"), key("d"));
        for bad in ["escape", "ctrl-l", "hyper-x", "xy", "f13", ""] {
            assert!(Key::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_screen_section_wins_over_one_for_every_screen() {
        let keymap = UserKeymap::parse(
            r#"[
                { "bindings": { "x": "Archive", "ctrl-p": "Palette" } },
                { "context": "MessageIndex", "bindings": { "x": "Delete", "d": null } }
            ]"#,
        );
        assert_eq!(keymap.error, None);
        let x = key("x");
        assert_eq!(keymap.lookup(Screen::Index, x), Some(Some(Cmd::Delete)));
        assert_eq!(keymap.lookup(Screen::Message, x), Some(Some(Cmd::Archive)));
        assert_eq!(keymap.lookup(Screen::Index, key("d")), Some(None));
        assert_eq!(keymap.lookup(Screen::Message, key("d")), None);
        // Compose's keys are text: no section reaches them.
        assert_eq!(keymap.lookup(Screen::Compose, x), None);
        assert_eq!(keymap.key_for(Screen::Index, Cmd::Delete), Some(x));
        assert_eq!(keymap.key_for(Screen::Index, Cmd::Archive), None);
        assert_eq!(keymap.key_for(Screen::Message, Cmd::Archive), Some(x));
        assert!(keymap.shadows(Screen::Index, "D", Cmd::Delete));
        assert!(keymap.shadows(Screen::Index, "X", Cmd::LooksSafe));
        assert!(!keymap.shadows(Screen::Index, "E", Cmd::Archive));
    }

    #[test]
    fn a_broken_file_is_ignored_whole_with_the_reason() {
        for (source, expected) in [
            ("{", "not valid JSON"),
            ("{}", "JSON array"),
            ("[3]", "section 1 is not an object"),
            (r#"[{"context": "Compose", "bindings": {}}]"#, "no screen"),
            (r#"[{"context": 3, "bindings": {}}]"#, "context must be"),
            (r#"[{"context": "Help"}]"#, "no bindings"),
            (
                r#"[{"bindings": {"x": "Explode"}}]"#,
                "no command is called",
            ),
            (r#"[{"bindings": {"x": 3}}]"#, "a command name or null"),
            (
                r#"[{"bindings": {"d": "Delete"}}, {"bindings": {"escape": "Quit"}}]"#,
                "section 2",
            ),
        ] {
            let keymap = UserKeymap::parse(source);
            assert!(keymap.bindings.is_empty(), "{source}");
            let error = keymap.error.unwrap_or_default();
            assert!(error.contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn no_file_is_no_keymap() {
        let keymap = UserKeymap::load(Path::new("/nonexistent/zmail/tui-keymap.json"));
        assert_eq!(keymap.error, None);
        assert!(keymap.bindings.is_empty());
    }

    #[test]
    fn every_command_name_is_unique_and_every_context_a_command_screen() {
        for (index, (name, cmd)) in ACTIONS.iter().enumerate() {
            assert!(
                ACTIONS[index + 1..]
                    .iter()
                    .all(|(other, other_cmd)| other != name && other_cmd != cmd),
                "{name}"
            );
            assert!(!cmd.description().is_empty(), "{name} has a description");
        }
        assert!(context_name(Screen::Compose).is_none());
    }
}
