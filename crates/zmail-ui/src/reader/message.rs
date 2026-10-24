//! The message view: one message of the conversation.
//!
//! An expanded message shows its header (`header`), the From/To/Date
//! details when asked for, then the banner slot (the sender-safety
//! banner), the body with its quoted tail folded behind a "…" toggle,
//! then the attachments slot. A collapsed message is a single
//! summary line: sender, a snippet and the date. Clicking either header
//! switches between the two.
//!
//! An expanded message with a long body takes several conversation list
//! items, one per chunk of its body: the first has the header and the
//! first chunk, the last the attachments, and together they draw one card.

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
use super::body::{BodyContext, render_pieces};
use super::header::{render_details, render_header};
use super::quoted::split_folded_tail;
use super::{attachments, banners};
use crate::a11y::AccessibleExt;
use crate::components::{Avatar, ButtonSize, HitTargetExt, IconButton};
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
    /// Which of the message's list items this is, and how many it takes.
    pub(super) part: usize,
    pub(super) parts: usize,
}

/// One message as a card: expanded or a summary line. For a message that
/// takes several list items, the part of the card this item draws.
pub(super) fn render_message(
    reader: &ReaderView,
    message: &ThreadMessage,
    display: MessageDisplay,
    context: &MessageContext<'_>,
    body: &BodyContext<'_>,
    web: Option<AnyElement>,
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &context.colors;
    let index = display.index;
    let first = display.part == 0;
    let last = display.part + 1 >= display.parts;
    let card = if first {
        let reader_entity = cx.entity().downgrade();
        div()
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
    } else {
        // The rest of a long message's card, below its first list item.
        let part = display.part as u64;
        div()
            .id(("reader-message-part", (message.id as u64) << 16 | part))
            .accessible(
                Role::Group,
                format!("{}, continued", message_label(message, context)),
            )
    };
    let card = card
        .border_color(rgb(if display.current && context.focused {
            colors.focus_ring
        } else {
            colors.border
        }))
        .border_l_1()
        .border_r_1()
        .when(first, |this| this.border_t_1().rounded_t_lg())
        .when(last, |this| this.border_b_1().rounded_b_lg())
        .bg(rgb(colors.surface))
        .overflow_hidden();
    let card = if display.expanded {
        card.when(first, |this| {
            this.child(render_header(reader, message, display, context, cx))
        })
        .child(expanded_content(
            reader, message, display, context, body, web, cx,
        ))
    } else {
        card.child(collapsed_summary(message, display, context, cx))
    };
    // Padding, not margin: the list measures an item without its margins.
    div()
        .px_4()
        .when(last, |this| this.pb_2())
        .child(card)
        .into_any_element()
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
        .hit_target(move || format!("reader-message-summary-{index}"))
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
/// sender's name. A part of a long message has its share of the body: the
/// first part also has the banners, the last the attachments. A message
/// shown formatted (`web`) is one part: its page instead of the text.
fn expanded_content(
    reader: &ReaderView,
    message: &ThreadMessage,
    display: MessageDisplay,
    context: &MessageContext<'_>,
    body: &BodyContext<'_>,
    web: Option<AnyElement>,
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &context.colors;
    let first = display.part == 0;
    let last = display.part + 1 >= display.parts;
    let mut banners = if first {
        banners::message_banners(message, context.sender_warnings)
    } else {
        Vec::new()
    };
    if first && let Some(failure) = reader.web_failure_banner(message) {
        banners.push(failure);
    }
    let attachments = if last {
        message_attachments(message, colors)
    } else {
        Vec::new()
    };
    if let Some(web) = web {
        return div()
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
            .child(
                div()
                    .debug_selector(move || format!("reader-web-body-{}", display.index))
                    .w_full()
                    .child(web),
            )
            .when(!attachments.is_empty(), |this| {
                this.child(div().flex().flex_wrap().gap_2().children(attachments))
            })
            .into_any_element();
    }
    let index = display.index;
    let fold_label = if display.unfolded {
        "Hide quoted text"
    } else {
        "Show quoted text"
    };
    // This part's pieces above and below the fold; the fold toggle goes
    // after the last piece above it.
    let model = body.model;
    let fold = model.visible_pieces();
    let pieces = body.pieces.clone();
    let above = pieces.start.min(fold)..pieces.end.min(fold);
    let below = pieces.start.max(fold)..pieces.end.max(fold);
    let fold_toggle =
        model.fold_start.is_some() && pieces.end >= fold && (first || pieces.start < fold);
    // A part that goes on from the one above starts with the space between
    // paragraphs when its first piece starts one.
    let starts_paragraph = !first
        && model
            .pieces
            .get(pieces.start)
            .is_some_and(|piece| piece.starts_paragraph);

    div()
        .flex()
        .flex_col()
        .gap_3()
        .pl(px(16. + AVATAR_SIZE + 12.))
        .pr_4()
        .when(last, |this| this.pb_4())
        .when(starts_paragraph, |this| this.pt_3())
        .min_w_0()
        .when(first && display.details, |this| {
            this.child(render_details(message, context))
        })
        .when(!banners.is_empty(), |this| {
            this.child(div().flex().flex_col().gap_2().children(banners))
        })
        .when(first || !above.is_empty(), |this| {
            this.child(render_pieces(body, above, false, cx))
        })
        .when(fold_toggle, |this| {
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
        })
        .when(display.unfolded && !below.is_empty(), |this| {
            this.child(render_pieces(body, below, true, cx))
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
