//! Persisted UI preferences.
//!
//! [`UiPreferences`] is the one place the desktop shell keeps choices that
//! should survive a restart: theme mode, list density, reading-pane
//! position, pane widths, sidebar state, the last open mailbox, the keymap,
//! the undo-send delay, motion and the text size. `zmail-store` persists it
//! as JSON in the local store's `app_settings` table, next to the rest of
//! the app's state.
//!
//! Loading is deliberately forgiving so a preferences record can never stop
//! the app from starting:
//!
//! - fields missing from an older record take their defaults;
//! - unknown fields written by a newer version are ignored;
//! - a field whose value no longer parses (say an enum variant that was
//!   removed) falls back to its default without discarding the others.

use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};

use crate::MailboxSelection;

/// Whether the UI follows the system light/dark appearance or forces one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// Follow the operating system's light/dark appearance.
    #[default]
    System,
    /// Always use the light theme.
    Light,
    /// Always use the dark theme.
    Dark,
}

/// Whether the UI animates: follow the system's reduce-motion setting, or
/// force reduced or full motion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionMode {
    /// Follow the operating system's reduce-motion setting (full motion
    /// where the platform cannot tell).
    #[default]
    System,
    /// Never animate: loading placeholders and spinners stay still.
    Reduce,
    /// Always animate, whatever the system says.
    Full,
}

/// How much vertical space list rows take.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    /// Three-line message rows with an avatar, subject and snippet.
    #[default]
    Comfortable,
    /// One-line message rows: sender, subject, snippet and date.
    Compact,
}

/// Where the reader pane sits relative to the message list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingPanePosition {
    /// Reader to the right of the message list (three columns).
    #[default]
    Right,
    /// Reader below the message list.
    Bottom,
    /// No reader pane; opening a message replaces the list.
    Off,
}

/// The keyboard shortcut set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymapName {
    /// The default keymap, built on the platform's primary modifier.
    #[default]
    Default,
    /// The default keymap plus Gmail-style single-key shortcuts.
    Gmail,
}

/// Which sender-safety warnings the reader shows (Settings → Privacy).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderWarnings {
    /// Every warning: lookalike senders, Reply-To on an unrelated domain,
    /// and Reply-To on another part of the sender's own domain.
    #[default]
    Standard,
    /// Leave out the informational warning for a Reply-To on a parent or
    /// subdomain of the sender's domain. Warnings about lookalike senders
    /// and unrelated Reply-To domains always show.
    Fewer,
}

/// User-chosen pane widths in logical pixels. `None` means "use the layout
/// default", so changing a default later still reaches users who never
/// dragged that pane.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneWidths {
    /// Width of the sidebar (accounts and mailboxes).
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<f32>,
    /// Width of the message list.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub message_list: Option<f32>,
}

/// What the sidebar's account trees have collapsed. Everything starts
/// expanded; only what the user collapses is recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarTree {
    /// Accounts whose folder tree is collapsed.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Vec::is_empty")]
    pub collapsed_accounts: Vec<i64>,
    /// Folders whose subfolders are hidden.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Vec::is_empty")]
    pub collapsed_folders: Vec<FolderRef>,
}

/// A folder in an account's tree, by its server name (a folder-tree node
/// may be a parent that holds no mail, so it has no mailbox id).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FolderRef {
    /// Store id of the account.
    pub account_id: i64,
    /// Server name of the folder.
    pub name: String,
}

/// When background body indexing runs (see `zmail_sync`'s indexer).
///
/// Indexing downloads message bodies so search can find mail that was never
/// opened. It always stays within the account's disk budget; these switches
/// decide whether it runs at all and whether it pauses on battery power or
/// a metered connection, where the platform can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexingPreferences {
    /// Whether background indexing runs.
    #[serde(deserialize_with = "lenient_true")]
    pub enabled: bool,
    /// Keep indexing while running on battery power.
    #[serde(deserialize_with = "lenient")]
    pub on_battery: bool,
    /// Keep indexing on a metered network connection.
    #[serde(deserialize_with = "lenient")]
    pub on_metered: bool,
}

impl Default for IndexingPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            on_battery: false,
            on_metered: false,
        }
    }
}

/// Default undo-send delay, in seconds.
pub const DEFAULT_UNDO_SEND_DELAY_SECS: u32 = 10;

/// Longest undo-send delay the app honours, in seconds.
pub const MAX_UNDO_SEND_DELAY_SECS: u32 = 30;

/// The text sizes Settings → Appearance offers, in percent of the default.
pub const TEXT_SIZES: [u16; 5] = [100, 125, 150, 175, 200];

/// The default text size, in percent.
pub const DEFAULT_TEXT_SIZE: u16 = 100;

/// The most entries [`UiPreferences::recent_commands`] keeps.
pub const MAX_RECENT_COMMANDS: usize = 8;

/// Persisted UI preferences. See the module documentation for how older and
/// newer records load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPreferences {
    /// System, light or dark theme.
    #[serde(deserialize_with = "lenient")]
    pub theme_mode: ThemeMode,
    /// List row density.
    #[serde(deserialize_with = "lenient")]
    pub density: Density,
    /// Reading-pane position.
    #[serde(deserialize_with = "lenient")]
    pub reading_pane: ReadingPanePosition,
    /// User-resized pane widths.
    #[serde(deserialize_with = "lenient")]
    pub pane_widths: PaneWidths,
    /// Whether the sidebar is collapsed.
    #[serde(deserialize_with = "lenient")]
    pub sidebar_collapsed: bool,
    /// Collapsed accounts and folders in the sidebar.
    #[serde(deserialize_with = "lenient")]
    pub sidebar_tree: SidebarTree,
    /// The mailbox (or virtual mailbox) that was open when the app last
    /// closed.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub last_mailbox: Option<MailboxSelection>,
    /// Keyboard shortcut set.
    #[serde(deserialize_with = "lenient")]
    pub keymap: KeymapName,
    /// Seconds a sent message waits in the outbox so it can be undone.
    /// Read it through [`UiPreferences::undo_send_delay`], which clamps it.
    #[serde(deserialize_with = "lenient_undo_send_delay")]
    pub undo_send_delay_secs: u32,
    /// Background body indexing switches.
    #[serde(deserialize_with = "lenient")]
    pub indexing: IndexingPreferences,
    /// The command palette's recently used entries, most recent first, as
    /// the palette's own keys (`"action:zmail::Archive"`, `"mailbox:42"`).
    /// Update it through [`UiPreferences::note_recent_command`].
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Vec::is_empty")]
    pub recent_commands: Vec<String>,
    /// Which sender-safety warnings the reader shows.
    #[serde(deserialize_with = "lenient")]
    pub sender_warnings: SenderWarnings,
    /// Whether the UI follows the system's reduce-motion setting.
    #[serde(deserialize_with = "lenient")]
    pub motion: MotionMode,
    /// Text size in percent of the default, one of [`TEXT_SIZES`]. Read
    /// it through [`UiPreferences::text_size`], which keeps it to them.
    #[serde(rename = "text_size", deserialize_with = "lenient_text_size")]
    pub text_size_percent: u16,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::default(),
            density: Density::default(),
            reading_pane: ReadingPanePosition::default(),
            pane_widths: PaneWidths::default(),
            sidebar_collapsed: false,
            sidebar_tree: SidebarTree::default(),
            last_mailbox: None,
            keymap: KeymapName::default(),
            undo_send_delay_secs: DEFAULT_UNDO_SEND_DELAY_SECS,
            indexing: IndexingPreferences::default(),
            recent_commands: Vec::new(),
            sender_warnings: SenderWarnings::default(),
            motion: MotionMode::default(),
            text_size_percent: DEFAULT_TEXT_SIZE,
        }
    }
}

impl UiPreferences {
    /// Parses a stored preferences record. Never fails: a record that is
    /// not a JSON object at all yields the defaults, and individual bad
    /// fields fall back to their defaults.
    pub fn from_json(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }

    /// Serialises the preferences for storage.
    pub fn to_json(&self) -> String {
        // Every field is a plain enum, number, bool or option of those, so
        // serialisation cannot fail; the fallback keeps this panic-free.
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Moves `key` to the front of the recently used palette entries,
    /// keeping at most [`MAX_RECENT_COMMANDS`].
    pub fn note_recent_command(&mut self, key: &str) {
        self.recent_commands.retain(|recent| recent != key);
        self.recent_commands.insert(0, key.to_owned());
        self.recent_commands.truncate(MAX_RECENT_COMMANDS);
    }

    /// The text size in percent: the stored value, or the nearest of
    /// [`TEXT_SIZES`] when it is not one of them.
    pub fn text_size(&self) -> u16 {
        TEXT_SIZES
            .into_iter()
            .min_by_key(|size| size.abs_diff(self.text_size_percent))
            .unwrap_or(DEFAULT_TEXT_SIZE)
    }

    /// The undo-send delay, clamped to [`MAX_UNDO_SEND_DELAY_SECS`].
    pub fn undo_send_delay(&self) -> Duration {
        Duration::from_secs(u64::from(
            self.undo_send_delay_secs.min(MAX_UNDO_SEND_DELAY_SECS),
        ))
    }
}

/// Deserialises a field, falling back to its default if the stored value
/// does not parse, so one bad field does not discard the whole record.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// [`lenient`] for the undo-send delay, whose default is not zero.
fn lenient_undo_send_delay<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(u32::deserialize(value).unwrap_or(DEFAULT_UNDO_SEND_DELAY_SECS))
}

/// [`lenient`] for the text size, whose default is not zero.
fn lenient_text_size<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(u16::deserialize(value).unwrap_or(DEFAULT_TEXT_SIZE))
}

/// [`lenient`] for switches that default to on.
fn lenient_true<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(bool::deserialize(value).unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn customised() -> UiPreferences {
        UiPreferences {
            theme_mode: ThemeMode::Dark,
            density: Density::Compact,
            reading_pane: ReadingPanePosition::Bottom,
            pane_widths: PaneWidths {
                sidebar: Some(240.0),
                message_list: Some(420.5),
            },
            sidebar_collapsed: true,
            sidebar_tree: SidebarTree {
                collapsed_accounts: vec![2],
                collapsed_folders: vec![FolderRef {
                    account_id: 3,
                    name: "Projects".to_owned(),
                }],
            },
            last_mailbox: Some(MailboxSelection::Mailbox {
                account_id: 3,
                mailbox_id: 42,
            }),
            keymap: KeymapName::Gmail,
            undo_send_delay_secs: 20,
            indexing: IndexingPreferences {
                enabled: false,
                on_battery: true,
                on_metered: true,
            },
            recent_commands: vec!["mailbox:42".to_owned(), "action:zmail::Archive".to_owned()],
            sender_warnings: SenderWarnings::Fewer,
            motion: MotionMode::Reduce,
            text_size_percent: 150,
        }
    }

    #[test]
    fn virtual_mailboxes_round_trip() {
        use crate::{MailboxRole, VirtualMailbox};
        for selection in [
            MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox)),
            MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Sent)),
            MailboxSelection::Virtual(VirtualMailbox::Flagged),
        ] {
            let prefs = UiPreferences {
                last_mailbox: Some(selection),
                ..UiPreferences::default()
            };
            assert_eq!(UiPreferences::from_json(&prefs.to_json()), prefs);
        }
    }

    #[test]
    fn defaults_match_the_plan() {
        let prefs = UiPreferences::default();
        assert_eq!(prefs.theme_mode, ThemeMode::System);
        assert_eq!(prefs.density, Density::Comfortable);
        assert_eq!(prefs.reading_pane, ReadingPanePosition::Right);
        assert_eq!(prefs.pane_widths, PaneWidths::default());
        assert!(!prefs.sidebar_collapsed);
        assert_eq!(prefs.last_mailbox, None);
        assert_eq!(prefs.keymap, KeymapName::Default);
        assert_eq!(prefs.undo_send_delay(), Duration::from_secs(10));
        assert!(prefs.indexing.enabled);
        assert!(!prefs.indexing.on_battery);
        assert!(!prefs.indexing.on_metered);
    }

    #[test]
    fn indexing_switches_fall_back_one_by_one() {
        let prefs = UiPreferences::from_json(r#"{"indexing":{"enabled":"yes","on_battery":true}}"#);
        assert!(prefs.indexing.enabled, "a bad value keeps indexing on");
        assert!(prefs.indexing.on_battery);
        assert!(!prefs.indexing.on_metered);
    }

    #[test]
    fn round_trips_through_json() {
        let prefs = customised();
        assert_eq!(UiPreferences::from_json(&prefs.to_json()), prefs);
        let defaults = UiPreferences::default();
        assert_eq!(UiPreferences::from_json(&defaults.to_json()), defaults);
    }

    #[test]
    fn serialises_readable_snake_case_values() {
        let json = customised().to_json();
        assert!(json.contains(r#""theme_mode":"dark""#), "{json}");
        assert!(json.contains(r#""reading_pane":"bottom""#), "{json}");
        assert!(json.contains(r#""keymap":"gmail""#), "{json}");
        assert!(json.contains(r#""sender_warnings":"fewer""#), "{json}");
        assert!(json.contains(r#""motion":"reduce""#), "{json}");
        assert!(json.contains(r#""text_size":150"#), "{json}");
    }

    #[test]
    fn sender_warnings_default_to_standard_and_fall_back_to_it() {
        assert_eq!(
            UiPreferences::default().sender_warnings,
            SenderWarnings::Standard
        );
        let prefs = UiPreferences::from_json(r#"{"sender_warnings":"none","density":"compact"}"#);
        assert_eq!(prefs.sender_warnings, SenderWarnings::Standard);
        assert_eq!(prefs.density, Density::Compact);
    }

    #[test]
    fn motion_defaults_to_system_and_falls_back_to_it() {
        assert_eq!(UiPreferences::default().motion, MotionMode::System);
        let full = UiPreferences::from_json(r#"{"motion":"full"}"#);
        assert_eq!(full.motion, MotionMode::Full);
        let prefs = UiPreferences::from_json(r#"{"motion":"wobbly","density":"compact"}"#);
        assert_eq!(prefs.motion, MotionMode::System);
        assert_eq!(prefs.density, Density::Compact);
    }

    #[test]
    fn text_size_defaults_to_100_and_keeps_to_the_offered_sizes() {
        assert_eq!(UiPreferences::default().text_size(), 100);
        let at = |json: &str| UiPreferences::from_json(json).text_size();
        assert_eq!(at(r#"{"text_size":175}"#), 175);
        assert_eq!(at(r#"{"text_size":180}"#), 175, "the nearest offered size");
        assert_eq!(at(r#"{"text_size":900}"#), 200);
        assert_eq!(at(r#"{"text_size":0}"#), 100);
        assert_eq!(at(r#"{"text_size":"huge"}"#), 100);
        assert_eq!(at(r#"{"text_size":-1}"#), 100);
    }

    #[test]
    fn loads_a_record_written_before_new_fields_existed() {
        // A record from a version that only knew about the theme mode.
        let prefs = UiPreferences::from_json(r#"{"theme_mode":"light"}"#);
        assert_eq!(
            prefs,
            UiPreferences {
                theme_mode: ThemeMode::Light,
                ..UiPreferences::default()
            }
        );
        assert_eq!(UiPreferences::from_json("{}"), UiPreferences::default());
        let partial_widths = UiPreferences::from_json(r#"{"pane_widths":{"sidebar":250}}"#);
        assert_eq!(partial_widths.pane_widths.sidebar, Some(250.0));
        assert_eq!(partial_widths.pane_widths.message_list, None);
    }

    #[test]
    fn ignores_fields_from_a_newer_version() {
        let prefs = UiPreferences::from_json(r#"{"density":"compact","future_setting":[1,2,3]}"#);
        assert_eq!(prefs.density, Density::Compact);
    }

    #[test]
    fn a_bad_field_falls_back_without_losing_the_others() {
        let prefs = UiPreferences::from_json(
            r#"{
                "theme_mode": "sepia",
                "density": "compact",
                "undo_send_delay_secs": -4,
                "last_mailbox": {"account_id": "x"},
                "sidebar_collapsed": true
            }"#,
        );
        assert_eq!(prefs.theme_mode, ThemeMode::System);
        assert_eq!(prefs.density, Density::Compact);
        assert_eq!(prefs.undo_send_delay_secs, DEFAULT_UNDO_SEND_DELAY_SECS);
        assert_eq!(prefs.last_mailbox, None);
        assert!(prefs.sidebar_collapsed);
    }

    #[test]
    fn unreadable_records_yield_defaults() {
        assert_eq!(UiPreferences::from_json(""), UiPreferences::default());
        assert_eq!(UiPreferences::from_json("[]"), UiPreferences::default());
        assert_eq!(
            UiPreferences::from_json("not json"),
            UiPreferences::default()
        );
    }

    #[test]
    fn recent_commands_move_to_the_front_and_are_capped() {
        let mut prefs = UiPreferences::default();
        for index in 0..MAX_RECENT_COMMANDS + 3 {
            prefs.note_recent_command(&format!("mailbox:{index}"));
        }
        assert_eq!(prefs.recent_commands.len(), MAX_RECENT_COMMANDS);
        assert_eq!(
            prefs.recent_commands[0],
            format!("mailbox:{}", MAX_RECENT_COMMANDS + 2)
        );
        prefs.note_recent_command("mailbox:5");
        assert_eq!(prefs.recent_commands[0], "mailbox:5");
        assert_eq!(
            prefs.recent_commands.len(),
            MAX_RECENT_COMMANDS,
            "no duplicates"
        );
        let bad = UiPreferences::from_json(r#"{"recent_commands":[1,2],"density":"compact"}"#);
        assert!(bad.recent_commands.is_empty());
        assert_eq!(bad.density, Density::Compact);
    }

    #[test]
    fn undo_send_delay_is_clamped() {
        let prefs = UiPreferences {
            undo_send_delay_secs: 600,
            ..UiPreferences::default()
        };
        assert_eq!(
            prefs.undo_send_delay(),
            Duration::from_secs(u64::from(MAX_UNDO_SEND_DELAY_SECS))
        );
        let off = UiPreferences {
            undo_send_delay_secs: 0,
            ..UiPreferences::default()
        };
        assert_eq!(off.undo_send_delay(), Duration::ZERO);
    }
}
