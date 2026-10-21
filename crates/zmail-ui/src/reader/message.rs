//! The message view: one message of the conversation.
//!
//! An expanded message shows its header (`header`), the From/To/Date
//! details when asked for, then the banner slot (the sender-safety
//! banner), the body with its quoted tail folded behind a "…" toggle,
//! then the attachments slot. A collapsed message is a single
//! summary line: sender, a snippet and the date. Clicking either header
//! switches between the two.

use chrono::{DateTime, Local};
use gpui::{
    AccessibleAction, AnyElement, Context, FontWeight, IntoElement, Role, div, prelude::*, px, rgb,
};
use zmail_core::{
    ThreadMessage,
    participants::sender_identity,
    preferences::SenderWarnings,
    time::{HourCycle, format_full_date, format_list_date},
};

use super::ReaderView;
use super::body::{BodyContext, render_paragraphs};
use super::header::{render_details, render_header};
use super::quoted::split_folded_tail;
use super::{attachments, banners};
use crate::a11y::AccessibleExt;
use crate::components::{Avatar, ButtonSize, IconButton};
use crate::styles::{AVATAR_SIZE, UiColors, rems_from_px};
use crate::widgets::EllipsisExt;

/// The avatar size on a collapsed message's summary line.
const COLLAPSED_AVATAR_SIZE: f32 = 24.0;

/// The longest snippet a collapsed message shows, in characters; the line
/// ends in an ellipsis long before this at any pane width.
const SNIPPET_CHARS: usize = 200;

/// What every message of the conversation renders with.
pub(super) struct MessageContext<'a> {
    pub(super) now: DateTime<Local>,
    pub(super) hour_cycle: HourCycle,
    pub(super) own_addresses: &'a [String],
    pub(super) colors: UiColors,
    /// Whether the reader has keyboard focus, so the current message
    /// shows the focus ring.
    pub(super) focused: bool,
    /// Which sender-safety warnings show.
    pub(super) sender_warnings: SenderWarnings,
    /// Messages in the conversation.
    pub(super) message_count: usize,
}

/// How one message shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MessageDisplay {
    /// Position in the conversation.
    pub(super) index: usize,
    pub(super) expanded: bool,
    /// The quoted tail is showing.
    pub(super) unfolded: bool,
    /// The full From, To and Date lines are showing.
    pub(super) details: bool,
    /// The message n/p moved to; Enter expands or collapses it.
    pub(super) current: bool,
}

/// One message as a card: expanded or a summary line.
pub(super) fn render_message(
    reader: &ReaderView,
    message: &ThreadMessage,
    display: MessageDisplay,
    context: &MessageContext<'_>,
    body: &BodyContext<'_>,
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &context.colors;
    let index = display.index;
    let reader_entity = cx.entity().downgrade();
    let card = div()
        .id(("reader-message", message.id as u64))
        .debug_selector(move || format!("reader-message-{index}"))
        .accessible(Role::Article, message_label(message, context))
        .aria_expanded(display.expanded)
        .aria_position_in_set(index + 1)
        .aria_size_of_set(context.message_count)
        // n and p move between messages while focus stays on the reader.
        .when(display.current && context.focused, |this| {
            this.aria_active_descendant()
        })
        .on_a11y_action(
            if display.expanded {
                AccessibleAction::Collapse
            } else {
                AccessibleAction::Expand
            },
            move |_, _, cx| {
                let _ = reader_entity.update(cx, |reader, cx| reader.toggle_message(index, cx));
            },
        )
        .rounded_lg()
        .border_1()
        .border_color(rgb(if display.current && context.focused {
            colors.focus_ring
        } else {
            colors.border
        }))
        .bg(rgb(colors.surface))
        .overflow_hidden();
    let card = if display.expanded {
        card.child(render_header(reader, message, display, context, cx))
            .child(expanded_content(message, display, context, body, cx))
    } else {
        card.child(collapsed_summary(message, display, context, cx))
    };
    // Padding, not margin: the list measures an item without its margins.
    div().px_4().pb_2().child(card).into_any_element()
}

/// What a screen reader reads for a message card: whether it is unread,
/// who sent it and when. The body follows as the card's text.
pub(super) fn message_label(message: &ThreadMessage, context: &MessageContext<'_>) -> String {
    let (sender_name, _) = sender_identity(&message.sender);
    let date = format_full_date(
        &message.received_at,
        &context.now.timezone(),
        context.hour_cycle,
    );
    let state = if message.is_read {
        ""
    } else {
        "Unread message"
    };
    let mut parts: Vec<String> = Vec::new();
    if !state.is_empty() {
        parts.push(state.to_owned());
    }
    parts.push(format!("from {sender_name}"));
    parts.push(date);
    parts.join(", ")
}

/// A collapsed message: avatar, sender, snippet and date on one line.
fn collapsed_summary(
    message: &ThreadMessage,
    display: MessageDisplay,
    context: &MessageContext<'_>,
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &context.colors;
    let (sender_name, sender_email) = sender_identity(&message.sender);
    let date = format_list_date(&message.received_at, &context.now, context.hour_cycle);
    let index = display.index;
    let mut avatar = Avatar::new(sender_name.clone()).size(COLLAPSED_AVATAR_SIZE);
    if let Some(email) = sender_email {
        avatar = avatar.address(email);
    }
    div()
        .id(("reader-message-summary", message.id as u64))
        .debug_selector(move || format!("reader-message-summary-{index}"))
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(colors.hover_background)))
        .on_click(cx.listener(move |reader, _, _, cx| reader.toggle_message(index, cx)))
        .child(avatar)
        .child(
            div()
                .flex_none()
                .max_w(rems_from_px(220.))
                .ellipsis()
                .when(!message.is_read, |this| {
                    this.font_weight(FontWeight::SEMIBOLD)
                })
                .child(sender_name),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .ellipsis()
                .text_sm()
                .text_color(rgb(colors.muted))
                .child(snippet(&message.rendered_body)),
        )
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_sm()
                .text_color(rgb(colors.muted))
                .child(date),
        )
        .into_any_element()
}

/// The start of the visible body on one line, for a collapsed message.
pub(super) fn snippet(body: &str) -> String {
    let (visible, _) = split_folded_tail(body);
    visible
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(SNIPPET_CHARS)
        .collect()
}

/// An expanded message below its header: banners, body, the fold toggle
/// and the quoted tail, then attachments. Indented to line up with the
/// sender's name.
fn expanded_content(
    message: &ThreadMessage,
    display: MessageDisplay,
    context: &MessageContext<'_>,
    body: &BodyContext<'_>,
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &context.colors;
    let banners = banners::message_banners(message, context.sender_warnings);
    let attachments = message_attachments(message, colors);
    let index = display.index;
    let fold_label = if display.unfolded {
        "Hide quoted text"
    } else {
        "Show quoted text"
    };

    div()
        .flex()
        .flex_col()
        .gap_3()
        .pl(px(16. + AVATAR_SIZE + 12.))
        .pr_4()
        .pb_4()
        .min_w_0()
        .when(display.details, |this| {
            this.child(render_details(message, context))
        })
        .when(!banners.is_empty(), |this| {
            this.child(div().flex().flex_col().gap_2().children(banners))
        })
        .child(render_paragraphs(body, false, cx))
        .when(body.model.fold_start.is_some(), |this| {
            // In a row, so the toggle is only as wide as its button.
            this.child(
                div().flex().child(
                    div()
                        .debug_selector(move || format!("reader-fold-{index}"))
                        .child(
                            IconButton::new(
                                ("reader-fold", message.id as u64),
                                "ellipsis",
                                fold_label,
                            )
                            .size(ButtonSize::Small)
                            .selected(display.unfolded)
                            .on_click(cx.listener(
                                move |reader, _, _, cx| {
                                    cx.stop_propagation();
                                    reader.toggle_fold(index, cx);
                                },
                            )),
                        ),
                ),
            )
            .when(display.unfolded, |this| {
                this.child(render_paragraphs(body, true, cx))
            })
        })
        .when(!attachments.is_empty(), |this| {
            this.child(div().flex().flex_wrap().gap_2().children(attachments))
        })
        .into_any_element()
}

/// The attachments slot of an expanded message, below its body: a card
/// per attachment.
fn message_attachments(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    attachments::attachment_cards(message, colors)
}
