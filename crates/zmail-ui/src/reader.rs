//! Thread reader pane and reader action buttons.
//!
//! [`render_reader_detail`] produces the scroll content for the reader detail
//! pane. [`reader_primary_action_buttons`] produces the main action-bar button
//! list shown above the scroll area, while [`reader_move_buttons`] keeps the
//! move-to-mailbox actions at the bottom of the detail pane.

use chrono::{DateTime, Local};
use gpui::{AnyElement, Context, FontWeight, IntoElement, div, prelude::*, rgb};
use zmail_core::{
    MailboxSummary, ThreadMessage, ThreadView,
    participants::{parse_address_list, sender_identity},
    text::count_noun,
    time::{HourCycle, format_full_date, format_list_date},
};

use super::WorkspaceView;
use crate::styles::{AVATAR_SIZE, UiColors};
use crate::tooltip::text_tooltip;
use crate::widgets::{
    EllipsisExt, action_divider, avatar, empty_state, format_size, label_chip,
    message_avatar_background, toolbar_button,
};

/// Render the full thread reader: subject, metadata, and conversation
/// cards. Returns an [`AnyElement`] ready to be placed
/// inside a scrollable container.
pub(crate) fn render_reader_detail(
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

/// Build the main reader actions rendered above the scrollable detail pane.
///
/// Returns an empty [`Vec`] when no thread is selected.
pub(crate) fn reader_primary_action_buttons(
    selected_thread: &Option<ThreadView>,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> Vec<AnyElement> {
    let Some(thread) = selected_thread.as_ref() else {
        return Vec::new();
    };

    let thread_has_unread = thread.unread_count > 0;
    let thread_is_flagged = thread.is_flagged;

    let mut buttons = vec![
        toolbar_button(
            "Reply",
            Some("reply"),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                this.reply_to_selected_thread(false, window, cx);
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Reply All",
            Some("reply-all"),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                this.reply_to_selected_thread(true, window, cx);
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Forward",
            Some("forward"),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                this.forward_selected_thread(window, cx);
            }),
        )
        .into_any_element(),
        action_divider(colors).into_any_element(),
        toolbar_button(
            "Archive",
            Some("archive"),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                let result = this
                    .selected_thread_id
                    .map(|thread_id| this.store.archive_thread(thread_id))
                    .unwrap_or_else(|| Ok(()));
                if let Err(error) = result {
                    this.store_error = Some(error.to_string());
                } else {
                    this.refresh_from_store();
                }
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Delete",
            Some("trash-2"),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                let result = this
                    .selected_thread_id
                    .map(|thread_id| this.store.delete_thread(thread_id))
                    .unwrap_or_else(|| Ok(()));
                if let Err(error) = result {
                    this.store_error = Some(error.to_string());
                } else {
                    this.refresh_from_store();
                }
                cx.notify();
            }),
        )
        .into_any_element(),
        action_divider(colors).into_any_element(),
        toolbar_button(
            if thread_is_flagged { "Unflag" } else { "Flag" },
            Some("flag"),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                let result = this
                    .selected_thread_id
                    .map(|thread_id| this.store.set_thread_flagged(thread_id, !thread_is_flagged))
                    .unwrap_or_else(|| Ok(()));
                if let Err(error) = result {
                    this.store_error = Some(error.to_string());
                } else {
                    this.refresh_from_store();
                }
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            if thread_has_unread {
                "Mark Read"
            } else {
                "Mark Unread"
            },
            Some(if thread_has_unread { "eye" } else { "eye-off" }),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                let result = this
                    .selected_thread_id
                    .map(|thread_id| this.store.set_thread_read(thread_id, thread_has_unread))
                    .unwrap_or_else(|| Ok(()));
                if let Err(error) = result {
                    this.store_error = Some(error.to_string());
                } else {
                    this.refresh_from_store();
                }
                cx.notify();
            }),
        )
        .into_any_element(),
    ];

    let thread_has_attachments = thread
        .messages
        .iter()
        .any(|message| !message.attachments.is_empty());
    if thread_has_attachments {
        buttons.push(
            toolbar_button(
                "Save attachments",
                Some("save"),
                colors.button_background,
                colors.border,
                colors.foreground,
                cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                    this.save_selected_thread_attachments(cx);
                }),
            )
            .into_any_element(),
        );
    }

    buttons
}

/// Build the move-to-mailbox buttons rendered at the bottom of the reader pane.
///
/// Returns an empty [`Vec`] when no thread is selected.
pub(crate) fn reader_move_buttons(
    selected_thread: &Option<ThreadView>,
    mailboxes: &[MailboxSummary],
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> Vec<AnyElement> {
    let Some(thread) = selected_thread.as_ref() else {
        return Vec::new();
    };

    let mut buttons = Vec::new();

    for mailbox in mailboxes
        .iter()
        .filter(|mailbox| mailbox.id != thread.mailbox_id)
    {
        let mailbox_id = mailbox.id;
        let mailbox_name = mailbox.name.clone();
        buttons.push(
            toolbar_button(
                format!("Move: {mailbox_name}"),
                Some("folder"),
                colors.button_background,
                colors.border,
                colors.foreground,
                cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                    let result = this
                        .selected_thread_id
                        .map(|thread_id| this.store.move_thread(thread_id, mailbox_id))
                        .unwrap_or_else(|| Ok(()));
                    if let Err(error) = result {
                        this.store_error = Some(error.to_string());
                    } else {
                        this.selected_mailbox_id = Some(mailbox_id);
                        this.refresh_from_store();
                    }
                    cx.notify();
                }),
            )
            .into_any_element(),
        );
    }

    buttons
}

fn render_thread_message(
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

/// Display names from an address list, with the user's own addresses
/// shown as "me": `Alice Johnson, me`.
fn participant_names(addresses: &str, own_addresses: &[String]) -> String {
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
