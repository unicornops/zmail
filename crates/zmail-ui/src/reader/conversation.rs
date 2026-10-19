//! The conversation view: the open thread as a variable-height `list`.
//!
//! The list's items are the conversation header (subject, mailbox and the
//! conversation's banners), then one item per message, then a footer that
//! holds the inline reply (UI plan Phase 5).
//!
//! Read messages collapse to a summary line; the latest message and the
//! unread ones open expanded. That choice is made when the conversation
//! opens, so marking it read a moment later does not collapse what the
//! user is reading. Messages that arrive while it is open follow the same
//! rule.

use std::collections::HashSet;

use gpui::{AnyElement, FontWeight, IntoElement, div, prelude::*, rgb};
use zmail_core::{ThreadView, text::count_noun};

use crate::styles::UiColors;
use crate::widgets::{empty_state, label_chip};

/// What the reader remembers about the open conversation: which messages
/// are expanded, which show their quoted text, and which is current.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConversationState {
    /// The open thread.
    thread_id: Option<i64>,
    /// Every message id the reader has decided an expansion for.
    seen: HashSet<i64>,
    /// Expanded message ids.
    expanded: HashSet<i64>,
    /// Message ids whose quoted tail is showing.
    unfolded: HashSet<i64>,
    /// The index of the message n/p moved to, which Enter toggles.
    current: Option<usize>,
}

impl ConversationState {
    /// The state for `thread` as it opens: the latest and unread messages
    /// expanded, and the first unread one (or the latest) current.
    pub(super) fn open(thread: Option<&ThreadView>) -> Self {
        let mut state = Self {
            thread_id: thread.map(|thread| thread.id),
            ..Self::default()
        };
        if let Some(thread) = thread {
            state.update(thread);
            state.current = thread
                .messages
                .iter()
                .position(|message| !message.is_read)
                .or_else(|| thread.messages.len().checked_sub(1));
        }
        state
    }

    /// Whether this state belongs to `thread`.
    pub(super) fn is_for(&self, thread: Option<&ThreadView>) -> bool {
        self.thread_id == thread.map(|thread| thread.id)
    }

    /// Takes in a reloaded `thread`: messages the reader has not seen yet
    /// get the opening rule; the rest keep how the user left them.
    pub(super) fn update(&mut self, thread: &ThreadView) {
        let last = thread.messages.len().saturating_sub(1);
        for (index, message) in thread.messages.iter().enumerate() {
            if !self.seen.insert(message.id) {
                continue;
            }
            if index == last || !message.is_read {
                self.expanded.insert(message.id);
            }
        }
        if let Some(current) = self.current {
            self.current = Some(current.min(last));
        }
    }

    pub(crate) fn is_expanded(&self, message_id: i64) -> bool {
        self.expanded.contains(&message_id)
    }

    pub(crate) fn is_unfolded(&self, message_id: i64) -> bool {
        self.unfolded.contains(&message_id)
    }

    pub(crate) fn current(&self) -> Option<usize> {
        self.current
    }

    pub(super) fn set_current(&mut self, index: usize) {
        self.current = Some(index);
    }

    /// Expands a collapsed message or collapses an expanded one.
    pub(super) fn toggle_expanded(&mut self, message_id: i64) {
        if !self.expanded.remove(&message_id) {
            self.expanded.insert(message_id);
        }
    }

    /// Shows or hides a message's quoted tail.
    pub(super) fn toggle_unfolded(&mut self, message_id: i64) {
        if !self.unfolded.remove(&message_id) {
            self.unfolded.insert(message_id);
        }
    }

    /// Expands every message (the screenshot fixture's expanded state).
    pub(super) fn expand_all(&mut self, thread: &ThreadView) {
        self.expanded
            .extend(thread.messages.iter().map(|message| message.id));
    }
}

/// What one item of the conversation list is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConversationItem {
    /// The subject, mailbox and conversation banners.
    Header,
    /// The message at this index.
    Message(usize),
    /// The space below the last message, where the inline reply goes.
    Footer,
}

impl ConversationItem {
    /// The item at list index `index` of a conversation of `messages`
    /// messages.
    pub(super) fn at(index: usize, messages: usize) -> Self {
        match index {
            0 => Self::Header,
            index if index <= messages => Self::Message(index - 1),
            _ => Self::Footer,
        }
    }

    /// The list index of the message at `message`.
    pub(super) fn message_index(message: usize) -> usize {
        message + 1
    }

    /// How many list items a conversation of `messages` messages has.
    pub(super) fn count(messages: usize) -> usize {
        messages + 2
    }
}

/// What the reader shows while the list has several conversations
/// selected.
pub(super) fn render_selection_summary(count: usize, colors: &UiColors) -> AnyElement {
    empty_state(
        &format!("{count} conversations selected"),
        "Archive, delete, move or flag them together from the bar above the list. \
         Esc clears the selection.",
        "check-circle",
        None,
        colors,
    )
}

/// What the reader shows with no conversation open.
pub(super) fn render_no_conversation(colors: &UiColors) -> AnyElement {
    empty_state(
        "No message selected",
        "Select a conversation to read it here.",
        "mail-open",
        None,
        colors,
    )
}

/// The first item of the list: the subject, where the conversation is and
/// how long it is, then the conversation's banners.
pub(super) fn render_conversation_header(thread: &ThreadView, colors: &UiColors) -> AnyElement {
    let banners = conversation_banners(thread, colors);
    div()
        .debug_selector(|| "reader-subject".into())
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .px_4()
        .pt_4()
        .pb_4()
        // The subject gets the full pane width so it wraps at the pane
        // edge.
        .child(
            div()
                .w_full()
                .text_xl()
                .font_weight(FontWeight::BOLD)
                .child(thread.subject.clone()),
        )
        .child(div().text_sm().text_color(rgb(colors.muted)).child(format!(
            "{} \u{2022} {}",
            thread.mailbox_name,
            count_noun(thread.message_count, "message", "messages")
        )))
        .when(!banners.is_empty(), |this| {
            this.child(div().flex().flex_wrap().gap_2().children(banners))
        })
        .into_any_element()
}

/// The conversation's banner slot, under the subject.
fn conversation_banners(thread: &ThreadView, colors: &UiColors) -> Vec<AnyElement> {
    let mut badges = Vec::new();

    if thread.unread_count > 0 {
        badges.push(
            label_chip(
                format!("{} unread", thread.unread_count),
                Some("eye-off"),
                colors.button_background,
                colors.accent,
            )
            .into_any_element(),
        );
    }

    if thread.is_flagged {
        badges.push(
            label_chip(
                "Flagged",
                Some("flag"),
                colors.button_background,
                colors.warning,
            )
            .into_any_element(),
        );
    }

    if thread.has_attachments {
        badges.push(
            label_chip(
                "Attachments",
                Some("paperclip"),
                colors.button_background,
                colors.info,
            )
            .into_any_element(),
        );
    }

    if thread.has_remote_content {
        badges.push(
            label_chip(
                "Remote content blocked",
                Some("shield"),
                colors.button_background,
                colors.danger,
            )
            .into_any_element(),
        );
    }

    badges
}
