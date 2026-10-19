//! The reader's commands: the action bar's icon buttons, and the entries
//! of its menus (the action bar's More, each message's More, and the
//! sender menu).

use gpui::{Action, AnyElement, FocusHandle, SharedString, div, prelude::*};
use zmail_core::{MailboxSummary, ThreadView, participants::sender_identity};

use super::ReaderState;
use crate::actions::{
    Archive, Delete, Forward, Reply, ReplyAll, SaveAttachments, ToggleFlag, ToggleRead,
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

/// A command the reader runs itself, rather than an action the shell
/// handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReaderCommand {
    /// Open the Move to… folder picker.
    MoveTo,
    /// Show the plain text or HTML source of the message at this index.
    ViewSource(usize),
    /// Copy the sender's address of the message at this index.
    CopyAddress(usize),
    /// Copy the sender's name and address of the message at this index.
    CopyNameAndAddress(usize),
}

/// One row of a reader menu.
pub(super) enum MoreItem {
    Action(ReaderAction),
    Command {
        label: SharedString,
        icon: &'static str,
        command: ReaderCommand,
    },
    Separator,
    Header(SharedString),
}

impl MoreItem {
    fn command(label: &'static str, icon: &'static str, command: ReaderCommand) -> Self {
        Self::Command {
            label: label.into(),
            icon,
            command,
        }
    }
}

/// Whether the conversation can move anywhere: the account has another
/// mailbox.
pub(super) fn can_move(thread: &ThreadView, state: &ReaderState) -> bool {
    state
        .mailboxes
        .iter()
        .any(|mailbox| mailbox.id != thread.mailbox_id)
}

/// The folders the Move to… picker offers: every other mailbox of the
/// account.
pub(super) fn move_destinations(thread: &ThreadView, state: &ReaderState) -> Vec<MailboxSummary> {
    state
        .mailboxes
        .iter()
        .filter(|mailbox| mailbox.id != thread.mailbox_id)
        .cloned()
        .collect()
}

/// The action bar's More menu: the actions a compact bar leaves out, then
/// Move to…. Empty when there is nothing to put in it (no thread, or a
/// full bar and no other mailbox).
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
    if can_move(thread, state) {
        if !items.is_empty() {
            items.push(MoreItem::Separator);
        }
        items.push(MoreItem::command(
            "Move to…",
            "folder",
            ReaderCommand::MoveTo,
        ));
    }
    items
}

/// A message's More menu: Move to…, Mark Read or Unread, View Plain Text
/// (or Source for an HTML message) and Save Attachments. Moving and the
/// read state apply to the conversation, which is what the store tracks.
pub(super) fn message_menu_items(
    thread: &ThreadView,
    index: usize,
    state: &ReaderState,
) -> Vec<MoreItem> {
    let Some(message) = thread.messages.get(index) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    if can_move(thread, state) {
        items.push(MoreItem::command(
            "Move to…",
            "folder",
            ReaderCommand::MoveTo,
        ));
    }
    items.push(MoreItem::Action(if thread.unread_count > 0 {
        ReaderAction::new("Mark Read", "eye", "secondary-shift-u", ToggleRead)
    } else {
        ReaderAction::new("Mark Unread", "eye-off", "secondary-shift-u", ToggleRead)
    }));
    items.push(MoreItem::Separator);
    items.push(MoreItem::command(
        if message.body_text.is_none() && message.sanitized_html.is_some() {
            "View Source"
        } else {
            "View Plain Text"
        },
        "file-text",
        ReaderCommand::ViewSource(index),
    ));
    if !message.attachments.is_empty() {
        items.push(MoreItem::Action(ReaderAction::new(
            "Save Attachments",
            "save",
            "secondary-shift-s",
            SaveAttachments,
        )));
    }
    items
}

/// The menu a sender's name opens: the address, and commands to copy it.
pub(super) fn sender_menu_items(sender: &str, index: usize) -> Vec<MoreItem> {
    let (name, email) = sender_identity(sender);
    let Some(email) = email else {
        return Vec::new();
    };
    let mut items = vec![
        MoreItem::Header(email.clone().into()),
        MoreItem::command("Copy Address", "copy", ReaderCommand::CopyAddress(index)),
    ];
    if name != email {
        items.push(MoreItem::command(
            "Copy Name and Address",
            "copy",
            ReaderCommand::CopyNameAndAddress(index),
        ));
    }
    items
}
