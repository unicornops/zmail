//! Hover quick actions: Archive, Delete, Flag and Read/Unread on the row
//! under the pointer, in place of its date (UI plan §2).
//!
//! They act on the hovered thread, which need not be the selected one, so
//! they call [`MailActions`] with its id rather than dispatching the
//! selection-based actions. That is the same path every other mail change
//! takes, so the change is recorded for undo and the shell refreshes from
//! the [`MailEvent`](crate::mail_actions::MailEvent) it emits.

use anyhow::Result;
use gpui::{AnyElement, Context, IntoElement, div, prelude::*};

use super::row::MessageRow;
use super::{MessageListEvent, MessageListView};
use crate::components::{ButtonSize, IconButton};
use crate::mail_actions::MailActions;

/// A quick action on one thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuickAction {
    Archive,
    Delete,
    SetFlagged(bool),
    SetRead(bool),
}

impl QuickAction {
    /// The actions for a row, in order, with their part name, icon and
    /// label. Flag and Read show the change they would make.
    fn for_row(row: &MessageRow) -> [(Self, &'static str, &'static str, &'static str); 4] {
        [
            (Self::Archive, "quick-archive", "archive", "Archive"),
            (Self::Delete, "quick-delete", "trash-2", "Delete"),
            if row.is_flagged {
                (Self::SetFlagged(false), "quick-flag", "flag", "Unflag")
            } else {
                (Self::SetFlagged(true), "quick-flag", "flag", "Flag")
            },
            if row.unread {
                (Self::SetRead(true), "quick-read", "eye", "Mark Read")
            } else {
                (Self::SetRead(false), "quick-read", "eye-off", "Mark Unread")
            },
        ]
    }

    fn run(
        self,
        actions: &mut MailActions,
        thread_id: i64,
        cx: &mut Context<MailActions>,
    ) -> Result<()> {
        match self {
            Self::Archive => actions.archive_threads(&[thread_id], cx),
            Self::Delete => actions.delete_threads(&[thread_id], cx),
            Self::SetFlagged(flagged) => actions.set_threads_flagged(&[thread_id], flagged, cx),
            Self::SetRead(read) => actions.set_threads_read(&[thread_id], read, cx),
        }
    }
}

impl MessageListView {
    /// Tracks the row under the pointer; re-renders only when it changes.
    pub(super) fn set_hovered_thread(
        &mut self,
        thread_id: i64,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let hovered_thread = if hovered {
            Some(thread_id)
        } else if self.hovered_thread == Some(thread_id) {
            None
        } else {
            return;
        };
        if self.hovered_thread != hovered_thread {
            self.hovered_thread = hovered_thread;
            cx.notify();
        }
    }

    /// The quick-action buttons for the hovered row.
    pub(super) fn quick_actions(
        &self,
        thread_id: i64,
        row: &MessageRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .children(
                QuickAction::for_row(row)
                    .into_iter()
                    .map(|(action, part, icon, label)| {
                        let id = format!("{}-{part}", row.id);
                        div()
                            .debug_selector({
                                let id = id.clone();
                                move || id
                            })
                            .child(
                                IconButton::new(id, icon, label)
                                    .size(ButtonSize::Small)
                                    .selected(action == QuickAction::SetFlagged(false))
                                    .on_click(cx.listener(move |list, _, _, cx| {
                                        // The row's own click would open the thread.
                                        cx.stop_propagation();
                                        list.run_quick_action(thread_id, action, cx);
                                    })),
                            )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn run_quick_action(
        &mut self,
        thread_id: i64,
        action: QuickAction,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self
            .mail_actions
            .update(cx, |actions, cx| action.run(actions, thread_id, cx))
        {
            cx.emit(MessageListEvent::Error(error.to_string()));
        }
    }
}
