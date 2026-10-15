//! Thread reader pane and reader action buttons.
//!
//! [`render_reader_detail`] produces the scroll content for the reader detail
//! pane. [`reader_primary_action_buttons`] produces the main action-bar button
//! list shown above the scroll area, while [`reader_move_buttons`] keeps the
//! move-to-mailbox actions at the bottom of the detail pane.

use gpui::{AnyElement, Context, FontWeight, IntoElement, div, prelude::*, rgb};
use zmail_core::{
    AccountSummary, IdentitySummary, MailboxSummary, MessageRenderMode, ThreadMessage, ThreadView,
};

use super::WorkspaceView;
use crate::styles::{AVATAR_SIZE, UiColors};
use crate::widgets::{
    action_divider, avatar, empty_state, label_chip, message_avatar_background, toolbar_button,
};

/// Render the full thread reader: subject, metadata, conversation cards, sync
/// status, and identity list. Returns an [`AnyElement`] ready to be placed
/// inside a scrollable container.
pub(crate) fn render_reader_detail(
    selected_thread: Option<&ThreadView>,
    selected_account: Option<&AccountSummary>,
    _mailboxes: &[MailboxSummary],
    identities: &[IdentitySummary],
    colors: &UiColors,
    _cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    match selected_thread {
        Some(thread) => {
            let sync_detail = selected_account
                .map(|account| {
                    format!(
                        "{} • {}",
                        account.sync_status.phase, account.sync_status.detail
                    )
                })
                .unwrap_or_else(|| "No sync state".to_owned());
            let latest_message_at = thread
                .messages
                .last()
                .map(|message| message.received_at.as_str())
                .unwrap_or("No local messages");

            div()
                .flex()
                .flex_col()
                .gap_5()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_start()
                                .gap_4()
                                .child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .gap_3()
                                        .child(
                                            div()
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
                                        ),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(colors.muted))
                                        .child(format!("{} messages", thread.message_count)),
                                ),
                        )
                        .child(
                            div().flex().flex_wrap().gap_4().children([
                                metadata_pair("Participants", &thread.participants, colors)
                                    .into_any_element(),
                                metadata_pair("Latest", latest_message_at, colors)
                                    .into_any_element(),
                                metadata_pair("Mailbox", &thread.mailbox_name, colors)
                                    .into_any_element(),
                            ]),
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
                                        render_thread_message(index, message, colors)
                                            .into_any_element()
                                    },
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Sync Status"))
                        .child(
                            div()
                                .rounded_md()
                                .bg(rgb(colors.surface))
                                .border_1()
                                .border_color(rgb(colors.border))
                                .px_4()
                                .py_3()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(colors.muted))
                                        .child(sync_detail),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Identities"))
                        .children(identities.iter().map(|identity| {
                            let primary = if identity.is_primary {
                                Some(
                                    label_chip(
                                        "Primary",
                                        Some("check-circle"),
                                        colors.button_background,
                                        colors.success,
                                    )
                                    .into_any_element(),
                                )
                            } else {
                                None
                            };

                            div()
                                .rounded_md()
                                .bg(rgb(colors.surface))
                                .border_1()
                                .border_color(rgb(colors.border))
                                .px_4()
                                .py_3()
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(identity.display_name.clone())
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(rgb(colors.muted))
                                                        .child(identity.email_address.clone()),
                                                ),
                                        )
                                        .when_some(primary, |this, badge| this.child(badge)),
                                )
                        })),
                )
                .into_any_element()
        }
        None => empty_state(
            "No message selected",
            "Select a thread, search result, or draft-related item to inspect it here.",
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

fn render_thread_message(index: usize, message: &ThreadMessage, colors: &UiColors) -> gpui::Div {
    let avatar_color = message_avatar_background(&message.sender, colors);
    let (sender_name, sender_email) = sender_identity(&message.sender);

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
                .child(avatar(&message.sender, AVATAR_SIZE, avatar_color, colors))
                .child(
                    div()
                        .flex_1()
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
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(sender_name),
                                                )
                                                .when_some(sender_email, |this, email| {
                                                    this.child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(rgb(colors.muted))
                                                            .child(email),
                                                    )
                                                }),
                                        )
                                        .when(!message.recipients.trim().is_empty(), |this| {
                                            this.child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(colors.muted))
                                                    .truncate()
                                                    .child(message.recipients.clone()),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(colors.muted))
                                        .child(message.received_at.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .children(message_status_badges(message, colors)),
                        )
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
                        .child(div().text_sm().child(message.rendered_body.clone())),
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

/// Formats a byte count with one fractional digit and a binary unit.
fn format_size(size_bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size_bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size_bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn message_status_badges(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    let mut badges = vec![
        label_chip(
            match message.render_mode {
                MessageRenderMode::PlainText => "Plain text",
                MessageRenderMode::SanitizedHtml => "Sanitized HTML",
                MessageRenderMode::PreviewOnly => "Preview only",
            },
            Some("file-text"),
            colors.button_background,
            colors.foreground,
        )
        .into_any_element(),
    ];

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

fn metadata_pair(label: &str, value: &str, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(rgb(colors.muted))
                .child(format!("{label}:")),
        )
        .child(div().text_sm().child(value.to_owned()))
}

fn sender_identity(sender: &str) -> (String, Option<String>) {
    let trimmed = sender.trim();

    let Some(start) = trimmed.rfind('<') else {
        return (trimmed.to_owned(), None);
    };
    let Some(end) = trimmed.rfind('>') else {
        return (trimmed.to_owned(), None);
    };
    if start >= end {
        return (trimmed.to_owned(), None);
    }

    let name = trimmed[..start].trim().trim_matches('"');
    let email = trimmed[start + 1..end].trim();

    if name.is_empty() {
        (email.to_owned(), Some(email.to_owned()))
    } else {
        (name.to_owned(), Some(email.to_owned()))
    }
}
