//! Persisted UI preferences.
//!
//! [`UiPreferences`] is the one place the desktop shell keeps choices that
//! should survive a restart: theme mode, list density, reading-pane
//! position, pane widths, sidebar state, the last open mailbox, the keymap
//! and the undo-send delay. `zmail-store` persists it as JSON in the local
//! store's `app_settings` table, next to the rest of the app's state.
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

/// A mailbox to reopen on the next launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxRef {
    /// Store id of the account that owns the mailbox.
    pub account_id: i64,
    /// Store id of the mailbox.
    pub mailbox_id: i64,
}

/// Default undo-send delay, in seconds.
pub const DEFAULT_UNDO_SEND_DELAY_SECS: u32 = 10;

/// Longest undo-send delay the app honours, in seconds.
pub const MAX_UNDO_SEND_DELAY_SECS: u32 = 30;

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
    /// The mailbox that was open when the app last closed.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub last_mailbox: Option<MailboxRef>,
    /// Keyboard shortcut set.
    #[serde(deserialize_with = "lenient")]
    pub keymap: KeymapName,
    /// Seconds a sent message waits in the outbox so it can be undone.
    /// Read it through [`UiPreferences::undo_send_delay`], which clamps it.
    #[serde(deserialize_with = "lenient_undo_send_delay")]
    pub undo_send_delay_secs: u32,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::default(),
            density: Density::default(),
            reading_pane: ReadingPanePosition::default(),
            pane_widths: PaneWidths::default(),
            sidebar_collapsed: false,
            last_mailbox: None,
            keymap: KeymapName::default(),
            undo_send_delay_secs: DEFAULT_UNDO_SEND_DELAY_SECS,
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
            last_mailbox: Some(MailboxRef {
                account_id: 3,
                mailbox_id: 42,
            }),
            keymap: KeymapName::Gmail,
            undo_send_delay_secs: 20,
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
