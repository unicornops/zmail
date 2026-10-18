//! What the list shows when it has no rows (UI plan Phase 3 item 5):
//! skeleton rows while an account syncs for the first time, and empty
//! states with one action each.

use gpui::{AnyElement, IntoElement, SharedString, div, prelude::*};
use zmail_core::{MailboxRole, MailboxSelection, VirtualMailbox};

use super::MessageListView;
use crate::actions::{NewMessage, ShowMail, SyncAccount, SyncAll};
use crate::components::{Button, SkeletonRow};
use crate::pane::dispatch_from;
use crate::styles::UiColors;
use crate::widgets::empty_state;

/// Placeholder rows drawn during a first sync: enough to fill a tall
/// window in either density.
const SKELETON_ROWS: usize = 12;

impl MessageListView {
    /// Grey placeholder rows in the list's density while the account's
    /// first sync fills the mailbox.
    pub(super) fn render_skeleton(&self) -> AnyElement {
        let density = self.density_tokens();
        div()
            .flex_1()
            .overflow_hidden()
            .debug_selector(|| "list-skeleton".into())
            .children(
                (0..SKELETON_ROWS)
                    .map(|index| SkeletonRow::new(("list-skeleton", index)).density(density)),
            )
            .into_any_element()
    }

    /// An empty mailbox. The Inbox (or Unified Inbox) is "all caught up";
    /// both offer to check for new mail.
    pub(super) fn render_empty_mailbox(&self, colors: &UiColors) -> AnyElement {
        let Some(mailbox) = self.state.mailbox.as_ref() else {
            return tagged(
                "list-empty-unsynced",
                empty_state(
                    "No local mail yet",
                    "Sync the account to bring its mail into this window.",
                    "inbox",
                    Some(self.action_button("list-empty-sync", "Sync Now", SyncAccount)),
                    colors,
                ),
            );
        };
        let virtual_mailbox = matches!(mailbox.selection, MailboxSelection::Virtual(_));
        let check = if virtual_mailbox {
            self.action_button("list-empty-sync", "Check for New Mail", SyncAll)
        } else {
            self.action_button("list-empty-sync", "Check for New Mail", SyncAccount)
        };
        let is_inbox = mailbox.role == Some(MailboxRole::Inbox)
            || mailbox.selection
                == MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        if is_inbox {
            tagged(
                "list-empty-inbox",
                empty_state(
                    "You\u{2019}re all caught up",
                    "New mail will appear here.",
                    "check-circle",
                    Some(check),
                    colors,
                ),
            )
        } else {
            tagged(
                "list-empty-mailbox",
                empty_state(
                    &format!("Nothing in {}", mailbox.name),
                    "Conversations moved or synced here will appear in this list.",
                    "folder",
                    Some(check),
                    colors,
                ),
            )
        }
    }

    /// A search that found nothing (yet).
    pub(super) fn render_no_results(&self, colors: &UiColors) -> AnyElement {
        let query = &self.state.search_query;
        if self.state.searching_server {
            return empty_state(
                "Searching the server\u{2026}",
                "Nothing matched in local mail. Results from the server will appear here.",
                "search",
                None,
                colors,
            );
        }
        if query.is_empty() {
            return empty_state(
                "Search your mail",
                "Type in the search field and press Enter.",
                "search",
                Some(self.action_button("list-search-clear", "Back to Mail", ShowMail)),
                colors,
            );
        }
        empty_state(
            &format!("No results for \u{201c}{query}\u{201d}"),
            "Search covers every mailbox in this account.",
            "search",
            Some(self.action_button("list-search-clear", "Clear Search", ShowMail)),
            colors,
        )
    }

    pub(super) fn render_no_drafts(&self, colors: &UiColors) -> AnyElement {
        empty_state(
            "No drafts",
            "Messages you start and don\u{2019}t send are kept here.",
            "pen-square",
            Some(self.action_button("list-new-message", "New Message", NewMessage)),
            colors,
        )
    }

    pub(super) fn render_empty_outbox(&self, colors: &UiColors) -> AnyElement {
        empty_state(
            "Nothing waiting to send",
            "Messages appear here while they send, and stay if sending fails.",
            "send",
            Some(self.action_button("list-new-message", "New Message", NewMessage)),
            colors,
        )
    }

    /// An empty state's one action: a button that dispatches `action` from
    /// the list, like its key binding.
    fn action_button(
        &self,
        id: &'static str,
        label: &'static str,
        action: impl gpui::Action,
    ) -> AnyElement {
        div()
            .debug_selector(move || id.into())
            .child(
                Button::new(SharedString::from(id), label)
                    .on_click(dispatch_from(&self.focus_handle, action)),
            )
            .into_any_element()
    }
}

/// Wraps an empty state so tests can tell which one is drawn.
fn tagged(selector: &'static str, state: AnyElement) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .debug_selector(move || selector.into())
        .child(state)
        .into_any_element()
}
