//! Message-list panel render method for [`WorkspaceView`].
//!
//! This module defines a single `impl WorkspaceView` block containing
//! [`WorkspaceView::render_message_list`], which renders the centre panel:
//! threads, search results, drafts, or outbox jobs, depending on the current
//! [`ListMode`].

use std::ops::Range;

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, SharedString, StatefulInteractiveElement, div,
    prelude::*, px, rgb, uniform_list,
};

use zmail_core::{
    DraftState, DraftSummary,
    participants::{participant_summary, sender_display_name},
    text::count_noun,
    time::{format_full_date, format_list_date},
};

use super::{DetailMode, ListMode, WorkspaceView};
use crate::styles::{
    AVATAR_SIZE, ICON_SIZE_SM, LIST_ITEM_HEIGHT, MESSAGE_LIST_WIDTH, ROW_LINE_GAP, ROW_PADDING_Y,
    ROW_PREVIEW_LINE_HEIGHT, ROW_SENDER_LINE_HEIGHT, ROW_TITLE_LINE_HEIGHT, UNREAD_INDICATOR_SIZE,
    UiColors,
};
use crate::tooltip::text_tooltip;
use crate::widgets::{
    EllipsisExt, avatar, badge, empty_state, icon, list_row, message_avatar_background,
    toolbar_button,
};

impl WorkspaceView {
    /// Render the centre message-list panel.
    ///
    /// Dispatches to the correct list variant based on `self.list_mode` and
    /// returns an [`AnyElement`] ready to be placed as a flex child of the
    /// main content row.
    pub(crate) fn render_message_list(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = *colors;
        let now = chrono::Local::now();

        let selected_mailbox = self.selected_mailbox().cloned();
        let thread_count = selected_mailbox
            .as_ref()
            .map(|mailbox| mailbox.total_threads)
            .unwrap_or_default();
        let list_title = match self.list_mode {
            ListMode::Mail => selected_mailbox
                .as_ref()
                .map(|mailbox| mailbox.name.clone())
                .unwrap_or_else(|| "Mail".to_owned()),
            ListMode::Search => "Search".to_owned(),
            ListMode::Drafts => "Drafts".to_owned(),
            ListMode::Outbox => "Outbox".to_owned(),
        };

        let list_subtitle = match self.list_mode {
            ListMode::Mail => selected_mailbox
                .as_ref()
                .map(|mailbox| {
                    if mailbox.unread_threads > 0 {
                        format!(
                            "{} • {} unread",
                            count_noun(mailbox.total_threads, "conversation", "conversations"),
                            mailbox.unread_threads
                        )
                    } else {
                        count_noun(mailbox.total_threads, "conversation", "conversations")
                    }
                })
                .unwrap_or_else(|| "No mailbox selected".to_owned()),
            ListMode::Search => format!(
                "{} for \u{201c}{}\u{201d}",
                count_noun(self.search_results.len(), "result", "results"),
                self.search_results_query
            ),
            ListMode::Drafts => count_noun(self.drafts.len(), "draft", "drafts"),
            ListMode::Outbox => count_noun(
                self.outbox_jobs.len(),
                "message waiting to send",
                "messages waiting to send",
            ),
        };

        div()
            .w(px(MESSAGE_LIST_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(colors.surface))
            .border_r_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(list_title))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(colors.muted))
                            .child(list_subtitle),
                    ),
            )
            .child(match self.list_mode {
                ListMode::Mail => {
                    if self.accounts.is_empty() {
                        empty_state(
                            "No accounts yet",
                            "Use Add Account to configure IMAP or JMAP directly in the app.",
                            "user-plus",
                            Some(
                                toolbar_button(
                                    "Set Up Account",
                                    Some("plus"),
                                    colors.button_background,
                                    colors.border,
                                    colors.foreground,
                                    cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                                        this.open_account_setup(window, cx);
                                    }),
                                )
                                .into_any_element(),
                            ),
                            &colors,
                        )
                    } else if thread_count == 0 {
                        empty_state(
                            "No local mail yet",
                            "Sync the selected account to populate the local mirror.",
                            "inbox",
                            None,
                            &colors,
                        )
                    } else {
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .child(
                                uniform_list(
                                    "threads",
                                    thread_count,
                                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                                        let Some(mailbox_id) = this.selected_mailbox_id else {
                                            return Vec::new();
                                        };
                                        let threads =
                                            match this.store.list_threads(mailbox_id, range) {
                                                Ok(threads) => {
                                                    this.store_error = None;
                                                    threads
                                                }
                                                Err(error) => {
                                                    this.store_error = Some(error.to_string());
                                                    Vec::new()
                                                }
                                            };

                                        let now = chrono::Local::now();
                                        let hour_cycle = this.hour_cycle;
                                        let own_addresses = this.own_addresses();
                                        let own_addresses = own_addresses
                                            .iter()
                                            .map(String::as_str)
                                            .collect::<Vec<_>>();
                                        threads
                                            .into_iter()
                                            .map(|thread| {
                                                let thread_id = thread.id;
                                                let sender_name =
                                                    sender_display_name(&thread.latest_sender);
                                                let sender_line = if thread.message_count > 1
                                                    && !thread.participants.trim().is_empty()
                                                {
                                                    participant_summary(
                                                        &thread.participants,
                                                        thread.message_count,
                                                        &own_addresses,
                                                    )
                                                } else {
                                                    sender_name.clone()
                                                };
                                                let row_id = thread_row_id(thread_id);
                                                let is_selected =
                                                    Some(thread_id) == this.selected_thread_id;
                                                let background_color = rgb(this
                                                    .interactive_background(
                                                        &row_id,
                                                        is_selected,
                                                        colors.surface,
                                                        &colors,
                                                    ));

                                                div()
                                                    .id(row_id.clone())
                                                    .w_full()
                                                    .h(px(LIST_ITEM_HEIGHT))
                                                    .px_3()
                                                    .py(px(ROW_PADDING_Y))
                                                    .cursor_pointer()
                                                    .border_b_1()
                                                    .border_color(rgb(colors.border))
                                                    .bg(background_color)
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.select_thread(
                                                            thread_id, mailbox_id, cx,
                                                        );
                                                    }))
                                                    .on_hover(cx.listener({
                                                        let row_id = row_id.clone();
                                                        move |this, hovered, _, cx| {
                                                            this.set_hovered_element(
                                                                row_id.clone(),
                                                                *hovered,
                                                                cx,
                                                            );
                                                        }
                                                    }))
                                                    .child(conversation_list_row(
                                                        ConversationRow {
                                                            id: row_id.clone(),
                                                            subject: &thread.subject,
                                                            sender_line: &sender_line,
                                                            preview: &thread.preview,
                                                            date: format_list_date(
                                                                &thread.latest_message_at,
                                                                &now,
                                                                hour_cycle,
                                                            ),
                                                            date_tooltip: format_full_date(
                                                                &thread.latest_message_at,
                                                                &now.timezone(),
                                                                hour_cycle,
                                                            ),
                                                            avatar_seed: &sender_name,
                                                            unread_count: thread.unread_count,
                                                            is_flagged: thread.is_flagged,
                                                            has_attachments: thread.has_attachments,
                                                        },
                                                        &colors,
                                                    ))
                                            })
                                            .collect()
                                    }),
                                )
                                .track_scroll(self.thread_scroll.clone())
                                .h_full(),
                            )
                            .into_any_element()
                    }
                }
                ListMode::Search => {
                    if self.search_results.is_empty() {
                        empty_state(
                            "No search results",
                            "Type a query above and run search against the local index.",
                            "search",
                            None,
                            &colors,
                        )
                    } else {
                        div()
                            .id("search-results-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .children(self.search_results.iter().map(|hit| {
                                let sender_name = sender_display_name(&hit.latest_sender);
                                let mailbox_id = hit.mailbox_id;
                                let thread_id = hit.thread_id;
                                let row_id = search_row_id(thread_id);
                                let is_selected = Some(thread_id) == self.selected_thread_id;
                                let background_color = rgb(self.interactive_background(
                                    &row_id,
                                    is_selected,
                                    colors.surface,
                                    &colors,
                                ));

                                div()
                                    .id(row_id.clone())
                                    .h(px(LIST_ITEM_HEIGHT))
                                    .px_3()
                                    .py(px(ROW_PADDING_Y))
                                    .cursor_pointer()
                                    .border_b_1()
                                    .border_color(rgb(colors.border))
                                    .bg(background_color)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_thread(thread_id, mailbox_id, cx);
                                    }))
                                    .on_hover(cx.listener({
                                        let row_id = row_id.clone();
                                        move |this, hovered, _, cx| {
                                            this.set_hovered_element(row_id.clone(), *hovered, cx);
                                        }
                                    }))
                                    .child(conversation_list_row(
                                        ConversationRow {
                                            id: row_id.clone(),
                                            subject: &hit.subject,
                                            sender_line: &format!(
                                                "{} \u{2022} {}",
                                                hit.mailbox_name, sender_name
                                            ),
                                            preview: &hit.preview,
                                            date: format_list_date(
                                                &hit.latest_message_at,
                                                &now,
                                                self.hour_cycle,
                                            ),
                                            date_tooltip: format_full_date(
                                                &hit.latest_message_at,
                                                &now.timezone(),
                                                self.hour_cycle,
                                            ),
                                            avatar_seed: &sender_name,
                                            unread_count: hit.unread_count,
                                            is_flagged: hit.is_flagged,
                                            has_attachments: hit.has_attachments,
                                        },
                                        &colors,
                                    ))
                            }))
                            .into_any_element()
                    }
                }
                ListMode::Drafts => {
                    if self.drafts.is_empty() {
                        empty_state(
                            "No drafts",
                            "Press Compose to start a new message.",
                            "pen-square",
                            None,
                            &colors,
                        )
                    } else {
                        div()
                            .id("drafts-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .children(self.drafts.iter().map(|draft| {
                                let draft_id = draft.id;
                                let row_id = draft_row_id(draft_id);
                                let is_selected = Some(draft_id) == self.selected_draft_id;
                                let background_color = rgb(self.interactive_background(
                                    &row_id,
                                    is_selected,
                                    colors.surface,
                                    &colors,
                                ));

                                div()
                                    .id(row_id.clone())
                                    .px_3()
                                    .py_3()
                                    .cursor_pointer()
                                    .border_b_1()
                                    .border_color(rgb(colors.border))
                                    .bg(background_color)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.load_draft_into_compose(draft_id);
                                        this.list_mode = ListMode::Drafts;
                                        this.detail_mode = DetailMode::Compose;
                                        cx.notify();
                                    }))
                                    .on_hover(cx.listener({
                                        let row_id = row_id.clone();
                                        move |this, hovered, _, cx| {
                                            this.set_hovered_element(row_id.clone(), *hovered, cx);
                                        }
                                    }))
                                    .child(list_row(
                                        display_subject(&draft.subject),
                                        &format_list_date(&draft.updated_at, &now, self.hour_cycle),
                                        &recipient_line(&draft.recipient_summary),
                                        "",
                                        &draft_meta(draft),
                                        &colors,
                                    ))
                            }))
                            .into_any_element()
                    }
                }
                ListMode::Outbox => {
                    if self.outbox_jobs.is_empty() {
                        empty_state(
                            "No outbox jobs",
                            "Queued and retrying deliveries will appear here.",
                            "send",
                            None,
                            &colors,
                        )
                    } else {
                        div()
                            .id("outbox-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .children(self.outbox_jobs.iter().map(|job| {
                                let job_id = job.id;
                                let row_id = outbox_row_id(job_id);
                                let is_selected = Some(job_id) == self.selected_outbox_job_id;
                                let background_color = rgb(self.interactive_background(
                                    &row_id,
                                    is_selected,
                                    colors.surface,
                                    &colors,
                                ));

                                div()
                                    .id(row_id.clone())
                                    .px_3()
                                    .py_3()
                                    .cursor_pointer()
                                    .border_b_1()
                                    .border_color(rgb(colors.border))
                                    .bg(background_color)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected_outbox_job_id = Some(job_id);
                                        cx.notify();
                                    }))
                                    .on_hover(cx.listener({
                                        let row_id = row_id.clone();
                                        move |this, hovered, _, cx| {
                                            this.set_hovered_element(row_id.clone(), *hovered, cx);
                                        }
                                    }))
                                    .child({
                                        let draft = self
                                            .drafts
                                            .iter()
                                            .find(|draft| draft.id == job.draft_id);
                                        list_row(
                                            display_subject(
                                                draft
                                                    .map(|draft| draft.subject.as_str())
                                                    .unwrap_or(""),
                                            ),
                                            &job.next_attempt_at
                                                .as_deref()
                                                .map(|at| {
                                                    format!(
                                                        "Retry {}",
                                                        format_list_date(at, &now, self.hour_cycle)
                                                    )
                                                })
                                                .unwrap_or_default(),
                                            &recipient_line(
                                                draft
                                                    .map(|draft| draft.recipient_summary.as_str())
                                                    .unwrap_or(""),
                                            ),
                                            job.last_error_message.as_deref().unwrap_or(""),
                                            job.state.label(),
                                            &colors,
                                        )
                                    })
                            }))
                            .into_any_element()
                    }
                }
            })
            .into_any_element()
    }
}

/// The subject to show for a draft or queued message.
fn display_subject(subject: &str) -> &str {
    if subject.trim().is_empty() {
        "(no subject)"
    } else {
        subject
    }
}

/// "To: …" line for a draft or queued message.
fn recipient_line(recipients: &str) -> String {
    if recipients.trim().is_empty() {
        "No recipients yet".to_owned()
    } else {
        format!("To: {}", recipients.trim())
    }
}

/// Status line for a draft row: only states that need attention and a
/// non-zero attachment count are shown.
fn draft_meta(draft: &DraftSummary) -> String {
    let mut parts = Vec::new();
    if draft.state != DraftState::Editing {
        parts.push(draft.state.label().to_owned());
    }
    if draft.attachment_count > 0 {
        parts.push(count_noun(
            draft.attachment_count,
            "attachment",
            "attachments",
        ));
    }
    parts.join(" • ")
}

/// Three fixed-height lines beside a `flex_none` avatar:
///
/// ```text
/// [avatar] [dot] [subject...] [flag] [clip]   [date]
///                [sender...]                  [badge]
///                [preview...]
/// ```
///
/// Every line has an explicit height and truncates, so a row never grows
/// past [`LIST_ITEM_HEIGHT`] and long text never pushes the date or badge
/// out of view.
fn conversation_list_row(row: ConversationRow<'_>, colors: &UiColors) -> gpui::Div {
    let avatar_color = message_avatar_background(row.avatar_seed, colors);

    div()
        .flex()
        .items_start()
        .gap_3()
        .h_full()
        .w_full()
        .child(avatar(row.avatar_seed, AVATAR_SIZE, avatar_color, colors))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(ROW_LINE_GAP))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .w_full()
                        .h(px(ROW_TITLE_LINE_HEIGHT))
                        .line_height(px(ROW_TITLE_LINE_HEIGHT))
                        .child(unread_indicator(row.unread_count > 0, colors))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .ellipsis()
                                .when(row.unread_count > 0, |this| {
                                    this.font_weight(FontWeight::SEMIBOLD)
                                })
                                .child(row.subject.to_owned()),
                        )
                        .when(row.is_flagged, |this| {
                            this.child(icon("star", ICON_SIZE_SM, colors.warning).flex_none())
                        })
                        .when(row.has_attachments, |this| {
                            this.child(icon("paperclip", ICON_SIZE_SM, colors.muted).flex_none())
                        })
                        .child(
                            div()
                                .id(SharedString::from(format!("{}-date", row.id)))
                                .flex_none()
                                .whitespace_nowrap()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .tooltip(text_tooltip(row.date_tooltip, *colors))
                                .child(row.date),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .w_full()
                        .h(px(ROW_SENDER_LINE_HEIGHT))
                        .line_height(px(ROW_SENDER_LINE_HEIGHT))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .ellipsis()
                                .child(row.sender_line.to_owned()),
                        )
                        .when(row.unread_count > 1, |this| {
                            this.child(
                                badge(row.unread_count, colors.accent, colors.background)
                                    .flex_none(),
                            )
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(ROW_PREVIEW_LINE_HEIGHT))
                        .line_height(px(ROW_PREVIEW_LINE_HEIGHT))
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .ellipsis()
                        .child(row.preview.to_owned()),
                ),
        )
}

struct ConversationRow<'a> {
    /// Element id of the row; the date's tooltip id derives from it.
    id: SharedString,
    subject: &'a str,
    sender_line: &'a str,
    preview: &'a str,
    /// Short relative date ("10:42", "Yesterday", "12 Oct").
    date: String,
    /// Full timestamp shown when hovering the date.
    date_tooltip: String,
    avatar_seed: &'a str,
    unread_count: usize,
    is_flagged: bool,
    has_attachments: bool,
}

fn unread_indicator(is_unread: bool, colors: &UiColors) -> gpui::Div {
    div()
        .flex_none()
        .w(px(UNREAD_INDICATOR_SIZE))
        .h(px(UNREAD_INDICATOR_SIZE))
        .rounded_full()
        .bg(rgb(if is_unread {
            colors.accent
        } else {
            colors.surface
        }))
}

fn thread_row_id(thread_id: i64) -> SharedString {
    SharedString::from(format!("thread-row-{thread_id}"))
}

fn search_row_id(thread_id: i64) -> SharedString {
    SharedString::from(format!("search-row-{thread_id}"))
}

fn draft_row_id(draft_id: i64) -> SharedString {
    SharedString::from(format!("draft-row-{draft_id}"))
}

fn outbox_row_id(job_id: i64) -> SharedString {
    SharedString::from(format!("outbox-row-{job_id}"))
}
