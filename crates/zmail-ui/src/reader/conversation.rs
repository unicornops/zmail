//! The conversation view: the subject, the thread's metadata and its
//! messages, or what the reader shows instead of one.

use gpui::{AnyElement, FontWeight, IntoElement, div, prelude::*, rgb};
use zmail_core::{
    ThreadView,
    text::count_noun,
    time::{HourCycle, format_full_date},
};

use super::message::{participant_names, render_thread_message};
use crate::styles::UiColors;
use crate::widgets::{EllipsisExt, empty_state, label_chip};

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

/// Render the full thread reader: subject, metadata, and conversation
/// cards. Returns an [`AnyElement`] ready to be placed
/// inside a scrollable container.
pub(super) fn render_reader_detail(
    selected_thread: Option<&ThreadView>,
    hour_cycle: HourCycle,
    own_addresses: &[String],
    colors: &UiColors,
) -> AnyElement {
    match selected_thread {
        Some(thread) => {
            let now = chrono::Local::now();
            let latest_message_at = thread
                .messages
                .last()
                .map(|message| format_full_date(&message.received_at, &now.timezone(), hour_cycle))
                .unwrap_or_else(|| "No local messages".to_owned());
            let participants = participant_names(&thread.participants, own_addresses);

            div()
                .flex()
                .flex_col()
                .gap_5()
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .w_full()
                        .min_w_0()
                        // The subject gets the full pane width so it wraps
                        // at the pane edge.
                        .child(
                            div()
                                .w_full()
                                .text_xl()
                                .font_weight(FontWeight::BOLD)
                                .child(thread.subject.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .children(thread_header_badges(thread, colors)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .w_full()
                                .min_w_0()
                                .child(metadata_pair("Participants", &participants, colors))
                                .child(metadata_pair("Latest", &latest_message_at, colors))
                                .child(metadata_pair(
                                    "Mailbox",
                                    &format!(
                                        "{} \u{2022} {}",
                                        thread.mailbox_name,
                                        count_noun(thread.message_count, "message", "messages")
                                    ),
                                    colors,
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Conversation"),
                        )
                        .child(
                            div()
                                .rounded_lg()
                                .bg(rgb(colors.surface))
                                .border_1()
                                .border_color(rgb(colors.border))
                                .children(thread.messages.iter().enumerate().map(
                                    |(index, message)| {
                                        render_thread_message(
                                            index,
                                            message,
                                            &now,
                                            hour_cycle,
                                            own_addresses,
                                            colors,
                                        )
                                        .into_any_element()
                                    },
                                )),
                        ),
                )
                .into_any_element()
        }
        None => empty_state(
            "No message selected",
            "Select a conversation to read it here.",
            "mail-open",
            None,
            colors,
        ),
    }
}

fn thread_header_badges(thread: &ThreadView, colors: &UiColors) -> Vec<AnyElement> {
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

/// One "Label: value" line; the value ends in an ellipsis when it does not
/// fit the pane.
fn metadata_pair(label: &str, value: &str, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(rgb(colors.muted))
                .child(format!("{label}:")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .ellipsis()
                .child(value.to_owned()),
        )
}
