//! The message view: one message of the conversation.
//!
//! An expanded message shows its header (`header`), the From/To/Date
//! details when asked for, then the banner slot (status and sender-safety
//! warnings), the body with its quoted tail folded behind a "…" toggle,
//! then the attachments slot. A collapsed message is a single
//! summary line: sender, a snippet and the date. Clicking either header
//! switches between the two.

use chrono::{DateTime, Local};
use gpui::{AnyElement, Context, FontWeight, IntoElement, div, prelude::*, px, rgb};
use zmail_core::{
    ThreadMessage,
    participants::sender_identity,
    time::{HourCycle, format_list_date},
};

use super::ReaderView;
use super::header::{render_details, render_header};
use super::quoted::split_folded_tail;
use crate::components::{Avatar, ButtonSize, IconButton};
use crate::styles::{AVATAR_SIZE, UiColors};
use crate::widgets::{EllipsisExt, format_size, label_chip};

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
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &context.colors;
    let index = display.index;
    let card = div()
        .id(("reader-message", message.id as u64))
        .debug_selector(move || format!("reader-message-{index}"))
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
            .child(expanded_content(message, display, context, cx))
    } else {
        card.child(collapsed_summary(message, display, context, cx))
    };
    // Padding, not margin: the list measures an item without its margins.
    div().px_4().pb_2().child(card).into_any_element()
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
                .max_w(px(220.))
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
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &context.colors;
    let (visible, folded) = split_folded_tail(&message.rendered_body);
    let banners = message_banners(message, colors);
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
        .child(div().w_full().min_w_0().text_sm().child(visible.to_owned()))
        .when_some(folded, |this, folded| {
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
                this.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .child(folded.to_owned()),
                )
            })
        })
        .when(!attachments.is_empty(), |this| {
            this.child(div().flex().flex_wrap().gap_2().children(attachments))
        })
        .into_any_element()
}

/// The banner slot of an expanded message, between its header and body:
/// the status chips and the sender-safety warning.
fn message_banners(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    let mut banners = Vec::new();
    let status_badges = message_status_badges(message, colors);
    if !status_badges.is_empty() {
        banners.push(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(status_badges)
                .into_any_element(),
        );
    }
    banners.extend(sender_safety_banner(message, colors));
    banners
}

/// The attachments slot of an expanded message, below its body.
fn message_attachments(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    message
        .attachments
        .iter()
        .map(|attachment| {
            label_chip(
                format!(
                    "{} ({})",
                    attachment.filename,
                    format_size(attachment.size_bytes)
                ),
                Some("paperclip"),
                colors.button_background,
                colors.info,
            )
            .into_any_element()
        })
        .collect()
}

fn message_status_badges(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    // The render mode (plain text, sanitized HTML, preview) is an internal
    // detail; it stays on `ThreadMessage` for diagnostics but is not shown.
    let mut badges = Vec::new();

    if !message.is_read {
        badges.push(
            label_chip(
                "Unread",
                Some("eye-off"),
                colors.button_background,
                colors.accent,
            )
            .into_any_element(),
        );
    }

    if message.is_flagged {
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

    if message.has_remote_content {
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

/// Renders the inline phishing-warning banner for a message's sender
/// identity (issue #91): lookalike From domains and Reply-To mismatches.
/// Information-first and non-blocking, matching the remote-content
/// stance in docs/phase5-hardening.md.
fn sender_safety_banner(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    let report = zmail_security::sender_safety::analyze_sender_identity(
        &message.sender,
        message.reply_to.as_deref(),
    );
    if report.is_safe() {
        return Vec::new();
    }

    vec![
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(colors.warning))
            .bg(rgb(colors.warning_background))
            .flex()
            .flex_col()
            .gap_1()
            .children(
                report
                    .warnings()
                    .into_iter()
                    .map(|warning| {
                        div()
                            .text_sm()
                            .text_color(rgb(colors.warning))
                            .child(warning)
                    })
                    .collect::<Vec<_>>(),
            )
            .into_any_element(),
    ]
}
