//! The message view: one message of the conversation, with its header,
//! status, attachments, sender-safety warning and body.

use chrono::{DateTime, Local};
use gpui::{AnyElement, FontWeight, IntoElement, div, prelude::*, rgb};
use zmail_core::{
    ThreadMessage,
    participants::{parse_address_list, sender_identity},
    time::{HourCycle, format_full_date, format_list_date},
};

use crate::styles::{AVATAR_SIZE, UiColors};
use crate::tooltip::text_tooltip;
use crate::widgets::{EllipsisExt, avatar, format_size, label_chip, message_avatar_background};

pub(super) fn render_thread_message(
    index: usize,
    message: &ThreadMessage,
    now: &DateTime<Local>,
    hour_cycle: HourCycle,
    own_addresses: &[String],
    colors: &UiColors,
) -> gpui::Div {
    let (sender_name, sender_email) = sender_identity(&message.sender);
    let avatar_color = message_avatar_background(&sender_name, colors);
    let recipients = participant_names(&message.recipients, own_addresses);
    let date = format_list_date(&message.received_at, now, hour_cycle);
    let full_date = format_full_date(&message.received_at, &now.timezone(), hour_cycle);
    let status_badges = message_status_badges(message, colors);

    div()
        .px_4()
        .py_4()
        .when(index > 0, |this| {
            this.border_t_1().border_color(rgb(colors.border))
        })
        .child(
            div()
                .flex()
                .items_start()
                .gap_3()
                .child(avatar(&sender_name, AVATAR_SIZE, avatar_color, colors))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_start()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .w_full()
                                                .ellipsis()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(sender_name),
                                        )
                                        .when_some(sender_email, |this, email| {
                                            this.child(
                                                div()
                                                    .w_full()
                                                    .ellipsis()
                                                    .text_sm()
                                                    .text_color(rgb(colors.muted))
                                                    .child(email),
                                            )
                                        })
                                        .when(!recipients.is_empty(), |this| {
                                            this.child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(colors.muted))
                                                    .ellipsis()
                                                    .child(format!("To: {recipients}")),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .id(("reader-message-date", message.id as u64))
                                        .flex_none()
                                        .whitespace_nowrap()
                                        .text_sm()
                                        .text_color(rgb(colors.muted))
                                        .tooltip(text_tooltip(full_date, *colors))
                                        .child(date),
                                ),
                        )
                        .when(!status_badges.is_empty(), |this| {
                            this.child(div().flex().flex_wrap().gap_2().children(status_badges))
                        })
                        .when(!message.attachments.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .children(message_attachments(message, colors)),
                            )
                        })
                        .children(sender_safety_banner(message, colors))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .text_sm()
                                .child(message.rendered_body.clone()),
                        ),
                ),
        )
}

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

/// Display names from an address list, with the user's own addresses
/// shown as "me": `Alice Johnson, me`.
pub(super) fn participant_names(addresses: &str, own_addresses: &[String]) -> String {
    parse_address_list(addresses)
        .iter()
        .map(|participant| {
            let is_me = participant.email.as_deref().is_some_and(|email| {
                own_addresses
                    .iter()
                    .any(|own| own.eq_ignore_ascii_case(email.trim()))
            });
            if is_me {
                "me".to_owned()
            } else {
                participant.display_name().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
