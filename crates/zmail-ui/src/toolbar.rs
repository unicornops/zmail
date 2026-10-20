//! The shell's toolbar: sidebar toggle, Compose, search and the sync
//! indicator.
//!
//! [`Toolbar`] is bare content (no background, border or fixed height):
//! it sits inside the window's title bar (`components::title_bar`), which
//! draws the background, the bottom border and the app-menu button. Its
//! row is `flex_1 min_w_0`, and every interactive element in it calls
//! `.occlude()`, so a press on it is not taken as a window drag.
//!
//! Every button dispatches its action from the window root, exactly like
//! its key binding. Clicking the toolbar leaves keyboard focus where it
//! was (only the search field takes focus), so Sync does not pull focus
//! out of the message list. Commands that used to be toolbar buttons
//! live elsewhere now: Drafts, Outbox and Add Account in the sidebar,
//! message actions in the reader, and Theme, Sync This Account and the
//! rest in the app menu (the menu bar on macOS, the title bar's app-menu
//! button elsewhere) and the keymap.

use chrono::{DateTime, Utc};
use gpui::{
    App, Context, Entity, FocusHandle, IntoElement, MouseButton, RenderOnce, Role, Window, div,
    prelude::*, px,
};
use zmail_core::{AccountSummary, SyncPhase, text::count_noun, time::parse_timestamp};

use super::WorkspaceView;
use crate::a11y::AccessibleExt;
use crate::actions::{NewMessage, SyncAll, ToggleSidebar};
use crate::components::{Button, IconButton, TextInput};
use crate::pane::dispatch_from;
use crate::styles::{SEARCH_FIELD_MAX_WIDTH, SEARCH_FIELD_MIN_WIDTH, UiColors};

impl WorkspaceView {
    /// Render the toolbar's content, for the title bar.
    ///
    /// Internal diagnostics that used to sit above the buttons live in
    /// [`crate::diagnostics`] until the Settings → About page exists.
    pub(crate) fn render_toolbar(
        &self,
        _colors: &UiColors,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Toolbar {
            focus_handle: self.focus_handle.clone(),
            search: self.search_query.clone(),
            sidebar_shown: self.sidebar_shown(),
            sync: sync_indicator(&self.accounts, Utc::now()),
        }
    }
}

/// The toolbar's content. See the module documentation.
#[derive(IntoElement)]
pub(crate) struct Toolbar {
    /// The window root, which the buttons dispatch their actions from.
    focus_handle: FocusHandle,
    search: Entity<TextInput>,
    /// Whether the sidebar is showing, for the toggle's label.
    sidebar_shown: bool,
    /// The sync indicator, or `None` with no accounts.
    sync: Option<SyncIndicator>,
}

impl RenderOnce for Toolbar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let sidebar_label = if self.sidebar_shown {
            "Hide sidebar"
        } else {
            "Show sidebar"
        };

        div()
            .id("toolbar")
            .accessible(Role::Toolbar, "Mail toolbar")
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .px_3()
            // Buttons here do not take keyboard focus; the search field
            // focuses itself before this runs.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            // ---- Leading: sidebar toggle and Compose ---------------------
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .debug_selector(|| "toolbar-sidebar".into())
                            .occlude()
                            .child(
                                IconButton::new("toolbar-sidebar", "panel-left", sidebar_label)
                                    .shortcut("secondary-b")
                                    .on_click(dispatch_from(&self.focus_handle, ToggleSidebar)),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "toolbar-compose".into())
                            .occlude()
                            .child(
                                Button::new("toolbar-compose", "Compose")
                                    .icon("pen-square")
                                    .primary()
                                    .shortcut("secondary-n")
                                    .on_click(dispatch_from(&self.focus_handle, NewMessage)),
                            ),
                    ),
            )
            // ---- Centre: search ------------------------------------------
            // The field fills the space between the two ends, up to a
            // comfortable width; it searches as you type (see search.rs).
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(SEARCH_FIELD_MIN_WIDTH))
                    .justify_center()
                    .child(
                        div()
                            .occlude()
                            .w_full()
                            .max_w(px(SEARCH_FIELD_MAX_WIDTH))
                            .child(self.search),
                    ),
            )
            // ---- Trailing: sync indicator ---------------------------------
            // The title bar puts the app-menu button after it.
            .when_some(self.sync, |this, sync| {
                this.child(
                    div()
                        .debug_selector(|| "toolbar-sync".into())
                        .flex_none()
                        .occlude()
                        .child(
                            IconButton::new("toolbar-sync", sync.icon(), sync.label)
                                .spinning(sync.state == SyncState::Syncing)
                                .shortcut("shift-f5")
                                .on_click(dispatch_from(&self.focus_handle, SyncAll)),
                        ),
                )
            })
    }
}

// ---------------------------------------------------------------------------
// Sync indicator
// ---------------------------------------------------------------------------

/// What the sync indicator shows, from the accounts' stored sync status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncState {
    /// Every account is idle.
    UpToDate,
    /// At least one account is syncing.
    Syncing,
    /// At least one account failed or is waiting to retry.
    Problem,
}

/// The indicator's state and its tooltip (also its accessible name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncIndicator {
    pub(crate) state: SyncState,
    pub(crate) label: String,
}

impl SyncIndicator {
    fn icon(&self) -> &'static str {
        match self.state {
            SyncState::UpToDate => "refresh-cw",
            SyncState::Syncing => "loader",
            SyncState::Problem => "alert-circle",
        }
    }
}

/// Longest error text the indicator's tooltip shows.
const MAX_ERROR_CHARS: usize = 80;

/// The indicator for `accounts` at `now`, read from the local store's sync
/// status (never from the server). A problem outranks a sync in progress,
/// so a failing account stays visible while another one syncs. `None`
/// when there are no accounts.
pub(crate) fn sync_indicator(
    accounts: &[AccountSummary],
    now: DateTime<Utc>,
) -> Option<SyncIndicator> {
    if accounts.is_empty() {
        return None;
    }
    let failing: Vec<&AccountSummary> = accounts
        .iter()
        .filter(|account| {
            matches!(
                account.sync_status.phase,
                SyncPhase::Failed | SyncPhase::Backoff
            )
        })
        .collect();
    let syncing: Vec<&AccountSummary> = accounts
        .iter()
        .filter(|account| {
            matches!(
                account.sync_status.phase,
                SyncPhase::Connecting | SyncPhase::Discovering | SyncPhase::Syncing
            )
        })
        .collect();

    let indicator = match (failing.as_slice(), syncing.as_slice()) {
        ([account], _) => SyncIndicator {
            state: SyncState::Problem,
            label: match account
                .sync_status
                .last_error_message
                .as_deref()
                .and_then(|message| message.lines().next())
                .map(str::trim)
                .filter(|message| !message.is_empty())
            {
                Some(message) => format!(
                    "Couldn't sync {}: {}",
                    account.display_name,
                    truncate(message, MAX_ERROR_CHARS)
                ),
                None => format!("Couldn't sync {}", account.display_name),
            },
        },
        ([_, _, ..], _) => SyncIndicator {
            state: SyncState::Problem,
            label: format!(
                "{} couldn't sync",
                count_noun(failing.len(), "account", "accounts")
            ),
        },
        ([], [account]) => SyncIndicator {
            state: SyncState::Syncing,
            label: format!("Syncing {}…", account.display_name),
        },
        ([], [_, _, ..]) => SyncIndicator {
            state: SyncState::Syncing,
            label: format!(
                "Syncing {}…",
                count_noun(syncing.len(), "account", "accounts")
            ),
        },
        ([], []) => SyncIndicator {
            state: SyncState::UpToDate,
            label: accounts
                .iter()
                .filter_map(|account| account.sync_status.last_success_at.as_deref())
                .filter_map(parse_timestamp)
                .max()
                .map(|at| updated_ago(at.with_timezone(&Utc), now))
                .unwrap_or_else(|| "Not synced yet".to_owned()),
        },
    };
    Some(indicator)
}

/// "Updated just now", "Updated 5 min ago", "Updated 3 hours ago",
/// "Updated yesterday" or "Updated 4 days ago".
fn updated_ago(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - at).num_seconds().max(0);
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if minutes < 1 {
        "Updated just now".to_owned()
    } else if hours < 1 {
        format!("Updated {minutes} min ago")
    } else if days < 1 {
        format!(
            "Updated {} ago",
            count_noun(hours as usize, "hour", "hours")
        )
    } else if days == 1 {
        "Updated yesterday".to_owned()
    } else {
        format!("Updated {days} days ago")
    }
}

/// `text` cut to `max` characters, with an ellipsis when it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};
    use zmail_core::{CachePolicySummary, ProtocolKind, SyncStatusSummary};

    use super::*;

    fn account(name: &str, phase: SyncPhase) -> AccountSummary {
        let mut sync_status = SyncStatusSummary::idle("test");
        sync_status.phase = phase;
        AccountSummary {
            id: 1,
            display_name: name.to_owned(),
            protocol: ProtocolKind::Imap,
            endpoint: String::new(),
            username: String::new(),
            identity_count: 1,
            cache_policy: CachePolicySummary::performance_first(),
            sync_status,
            color_override: None,
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 17, 12, 0, 0).unwrap()
    }

    fn label(accounts: &[AccountSummary]) -> (SyncState, String) {
        let indicator = sync_indicator(accounts, now()).expect("an indicator");
        (indicator.state, indicator.label)
    }

    #[test]
    fn no_accounts_hide_the_indicator() {
        assert_eq!(sync_indicator(&[], now()), None);
    }

    #[test]
    fn idle_accounts_show_when_they_last_synced() {
        let mut work = account("Work", SyncPhase::Idle);
        assert_eq!(
            label(std::slice::from_ref(&work)),
            (SyncState::UpToDate, "Not synced yet".to_owned())
        );
        work.sync_status.last_success_at = Some("2026-10-17T11:58:00Z".to_owned());
        let mut home = account("Home", SyncPhase::Idle);
        home.sync_status.last_success_at = Some("2026-10-17T09:00:00Z".to_owned());
        // The most recent sync of any account.
        assert_eq!(
            label(&[home, work]),
            (SyncState::UpToDate, "Updated 2 min ago".to_owned())
        );
    }

    #[test]
    fn relative_times_read_naturally() {
        let ago = |delta: Duration| updated_ago(now() - delta, now());
        assert_eq!(ago(Duration::seconds(20)), "Updated just now");
        assert_eq!(ago(Duration::minutes(59)), "Updated 59 min ago");
        assert_eq!(ago(Duration::minutes(61)), "Updated 1 hour ago");
        assert_eq!(ago(Duration::hours(5)), "Updated 5 hours ago");
        assert_eq!(ago(Duration::hours(30)), "Updated yesterday");
        assert_eq!(ago(Duration::days(4)), "Updated 4 days ago");
        // Clock skew never shows a future time.
        assert_eq!(ago(Duration::minutes(-5)), "Updated just now");
    }

    #[test]
    fn syncing_and_problems_are_shown_with_problems_first() {
        assert_eq!(
            label(&[account("Work", SyncPhase::Syncing)]),
            (SyncState::Syncing, "Syncing Work…".to_owned())
        );
        assert_eq!(
            label(&[
                account("Work", SyncPhase::Connecting),
                account("Home", SyncPhase::Discovering),
            ]),
            (SyncState::Syncing, "Syncing 2 accounts…".to_owned())
        );

        let mut failed = account("Home", SyncPhase::Failed);
        failed.sync_status.last_error_message =
            Some("authentication failed\nserver said: no".to_owned());
        assert_eq!(
            label(&[account("Work", SyncPhase::Syncing), failed.clone()]),
            (
                SyncState::Problem,
                "Couldn't sync Home: authentication failed".to_owned()
            )
        );
        assert_eq!(
            label(&[failed, account("Work", SyncPhase::Backoff)]),
            (SyncState::Problem, "2 accounts couldn't sync".to_owned())
        );
        assert_eq!(
            label(&[account("Work", SyncPhase::Backoff)]),
            (SyncState::Problem, "Couldn't sync Work".to_owned())
        );
    }

    #[test]
    fn long_errors_are_cut() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghijkl", 5), "abcd…");
    }
}
