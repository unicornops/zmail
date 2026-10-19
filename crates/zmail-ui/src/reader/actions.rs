//! The reader's commands: the action bar's icon buttons and the More
//! menu.

use gpui::{Action, AnyElement, FocusHandle, SharedString, div, prelude::*};
use zmail_core::{MailboxSummary, ThreadView};

use super::ReaderState;
use crate::actions::{
    Archive, Delete, Forward, MoveToMailbox, Reply, ReplyAll, SaveAttachments, ToggleFlag,
    ToggleRead,
};
use crate::components::IconButton;
use crate::styles::UiColors;
use crate::widgets::action_divider;

/// One conversation command: its label, icon, shortcut and action.
pub(super) struct ReaderAction {
    pub(super) label: SharedString,
    pub(super) icon: &'static str,
    /// The key binding, shown in tooltips and menus.
    pub(super) shortcut: Option<&'static str>,
    pub(super) action: Box<dyn Action>,
    /// Whether the button stays in a compact action bar.
    compact: bool,
}

impl ReaderAction {
    fn new(
        label: &'static str,
        icon: &'static str,
        shortcut: &'static str,
        action: impl Action,
    ) -> Self {
        Self {
            label: label.into(),
            icon,
            shortcut: Some(shortcut),
            action: Box::new(action),
            compact: false,
        }
    }

    /// Keep the button when the bar is compact.
    fn keep_when_compact(mut self) -> Self {
        self.compact = true;
        self
    }
}

/// A group of [`ReaderAction`]s; groups are separated in the action bar
/// and the More menu.
type ActionGroup = Vec<ReaderAction>;

/// The thread's commands, in order, grouped: respond, file, mark.
fn reader_actions(thread: &ThreadView) -> [ActionGroup; 3] {
    let mut mark = vec![
        ReaderAction::new(
            if thread.is_flagged { "Unflag" } else { "Flag" },
            "flag",
            "secondary-shift-l",
            ToggleFlag,
        ),
        if thread.unread_count > 0 {
            ReaderAction::new("Mark Read", "eye", "secondary-shift-u", ToggleRead)
        } else {
            ReaderAction::new("Mark Unread", "eye-off", "secondary-shift-u", ToggleRead)
        },
    ];
    let thread_has_attachments = thread
        .messages
        .iter()
        .any(|message| !message.attachments.is_empty());
    if thread_has_attachments {
        mark.push(ReaderAction::new(
            "Save Attachments",
            "save",
            "secondary-shift-s",
            SaveAttachments,
        ));
    }
    [
        vec![
            ReaderAction::new("Reply", "reply", "secondary-r", Reply).keep_when_compact(),
            ReaderAction::new("Reply All", "reply-all", "secondary-shift-r", ReplyAll),
            ReaderAction::new("Forward", "forward", "secondary-shift-f", Forward),
        ],
        vec![
            ReaderAction::new("Archive", "archive", "secondary-e", Archive).keep_when_compact(),
            ReaderAction::new("Delete", "trash-2", "secondary-backspace", Delete)
                .keep_when_compact(),
        ],
        mark,
    ]
}

/// The action bar's icon buttons, with dividers between the groups. Each
/// button dispatches its action from the reader, so it does exactly what
/// the key binding does.
///
/// Returns an empty [`Vec`] when no thread is selected.
pub(super) fn action_bar_buttons(
    thread: Option<&ThreadView>,
    compact: bool,
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> Vec<AnyElement> {
    let Some(thread) = thread else {
        return Vec::new();
    };
    let mut elements = Vec::new();
    for group in reader_actions(thread) {
        let buttons: Vec<AnyElement> = group
            .into_iter()
            .filter(|action| !compact || action.compact)
            .map(|action| {
                let focus_handle = focus_handle.clone();
                let command = action.action;
                // Icons are unique within the bar, so they name the buttons.
                let mut button =
                    IconButton::new(format!("reader-{}", action.icon), action.icon, action.label)
                        .selected(action.icon == "flag" && thread.is_flagged)
                        .on_click(move |_, window, cx| {
                            focus_handle.dispatch_action(command.as_ref(), window, cx)
                        });
                if let Some(shortcut) = action.shortcut {
                    button = button.shortcut(shortcut);
                }
                button.into_any_element()
            })
            .collect();
        if buttons.is_empty() {
            continue;
        }
        if !elements.is_empty() {
            elements.push(
                div()
                    .mx_1()
                    .child(action_divider(colors))
                    .into_any_element(),
            );
        }
        elements.extend(buttons);
    }
    elements
}

/// One row of the More menu.
pub(super) enum MoreItem {
    Action(ReaderAction),
    Separator,
    Header(String),
}

/// The More menu: the actions a compact bar leaves out, then Move to…
/// with one entry per other mailbox. Empty when there is nothing to put
/// in it (no thread, or a full bar and no other mailbox).
pub(super) fn more_menu_items(thread: Option<&ThreadView>, state: &ReaderState) -> Vec<MoreItem> {
    let Some(thread) = thread else {
        return Vec::new();
    };
    let mut items = Vec::new();
    if state.compact_actions {
        for group in reader_actions(thread) {
            let hidden: Vec<ReaderAction> =
                group.into_iter().filter(|action| !action.compact).collect();
            if hidden.is_empty() {
                continue;
            }
            if !items.is_empty() {
                items.push(MoreItem::Separator);
            }
            items.extend(hidden.into_iter().map(MoreItem::Action));
        }
    }
    let destinations: Vec<&MailboxSummary> = state
        .mailboxes
        .iter()
        .filter(|mailbox| mailbox.id != thread.mailbox_id)
        .collect();
    if !destinations.is_empty() {
        if !items.is_empty() {
            items.push(MoreItem::Separator);
        }
        items.push(MoreItem::Header("Move to".to_owned()));
        items.extend(destinations.into_iter().map(|mailbox| {
            MoreItem::Action(ReaderAction {
                label: mailbox.name.clone().into(),
                icon: "folder",
                shortcut: None,
                action: Box::new(MoveToMailbox {
                    mailbox_id: mailbox.id,
                }),
                compact: false,
            })
        }));
    }
    items
}
