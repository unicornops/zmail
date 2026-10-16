//! The reader pane: the open conversation and its actions.
//!
//! [`ReaderView`] is its own entity with its own focus handle and the
//! `Reader` key context. The shell pushes the selected thread (loaded once
//! per selection or store refresh, never per frame) with
//! [`ReaderView::set_state`]. Its buttons dispatch the same actions as the
//! key bindings; the shell handles them, because replying and moving need
//! the selection and the account.

use chrono::{DateTime, Local};
use gpui::{
    Action, AnyElement, App, Context, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    ScrollHandle, Window, div, point, prelude::*, px, rgb,
};
use zmail_core::{
    MailboxSummary, ThreadMessage, ThreadView,
    participants::{parse_address_list, sender_identity},
    text::count_noun,
    time::{HourCycle, format_full_date, format_list_date},
};

use crate::actions::{
    Archive, Delete, Forward, MoveToMailbox, PageDown, PageUp, Reply, ReplyAll, SaveAttachments,
    ScrollDown, ScrollUp, ToggleFlag, ToggleRead,
};
use crate::pane::pane_focus_indicator;
use crate::styles::{AVATAR_SIZE, UiColors};
use crate::tooltip::text_tooltip;
use crate::widgets::{
    EllipsisExt, action_divider, avatar, empty_state, format_size, label_chip,
    message_avatar_background, toolbar_button,
};

/// The key context the reader sets.
pub(crate) const KEY_CONTEXT: &str = "Reader";

/// How far one arrow key scrolls the conversation.
const SCROLL_STEP: f32 = 48.;

/// What the shell shows in the reader.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ReaderState {
    pub(crate) thread: Option<ThreadView>,
    /// The account's mailboxes, for the move buttons.
    pub(crate) mailboxes: Vec<MailboxSummary>,
    /// The user's own addresses, shown as "me".
    pub(crate) own_addresses: Vec<String>,
}

pub(crate) struct ReaderView {
    focus_handle: FocusHandle,
    hour_cycle: HourCycle,
    state: ReaderState,
    scroll_handle: ScrollHandle,
    /// Test instrumentation: renders.
    #[cfg(test)]
    pub(crate) counters: crate::pane::RenderCounters,
}

impl Focusable for ReaderView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ReaderView {
    pub(crate) fn new(hour_cycle: HourCycle, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            hour_cycle,
            state: ReaderState::default(),
            scroll_handle: ScrollHandle::new(),
            #[cfg(test)]
            counters: Default::default(),
        }
    }

    /// Replaces what the reader shows; re-renders only when it changed. A
    /// different thread starts at the top.
    pub(crate) fn set_state(&mut self, state: ReaderState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        let thread_changed = self.state.thread.as_ref().map(|thread| thread.id)
            != state.thread.as_ref().map(|thread| thread.id);
        self.state = state;
        if thread_changed {
            self.scroll_handle.set_offset(point(px(0.), px(0.)));
        }
        cx.notify();
    }

    fn scroll_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        let offset = self.scroll_handle.offset();
        let max = self.scroll_handle.max_offset().y;
        // Offsets grow negative as the content scrolls up.
        let y = (offset.y - px(delta)).clamp(-max, px(0.));
        self.scroll_handle.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn page_height(&self) -> f32 {
        let height: f32 = self.scroll_handle.bounds().size.height.into();
        (height - SCROLL_STEP).max(SCROLL_STEP)
    }

    fn scroll_up(&mut self, _: &ScrollUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-SCROLL_STEP, cx);
    }

    fn scroll_down(&mut self, _: &ScrollDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(SCROLL_STEP, cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-self.page_height(), cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.page_height(), cx);
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.counters.renders += 1;
        }
        let colors = *cx.global::<UiColors>();
        let focused = self.focus_handle.contains_focused(window, cx);
        let thread = self.state.thread.as_ref();
        let primary_actions = reader_primary_action_buttons(thread, &self.focus_handle, &colors);
        let move_actions =
            reader_move_buttons(thread, &self.state.mailboxes, &self.focus_handle, &colors);

        div()
            .id("reader")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            .child(
                pane_focus_indicator(
                    div()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(rgb(colors.border)),
                    focused,
                    &colors,
                )
                .child("Reader"),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .children(primary_actions),
            )
            .child(
                div()
                    .id("reader-scroll")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .px_4()
                    .py_4()
                    .child(render_reader_detail(
                        thread,
                        self.hour_cycle,
                        &self.state.own_addresses,
                        &colors,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .children(move_actions),
            )
    }
}

/// Render the full thread reader: subject, metadata, and conversation
/// cards. Returns an [`AnyElement`] ready to be placed
/// inside a scrollable container.
fn render_reader_detail(
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

/// The main reader actions shown above the conversation. Each button
/// dispatches its action from the reader, so it does exactly what the key
/// binding does.
///
/// Returns an empty [`Vec`] when no thread is selected.
fn reader_primary_action_buttons(
    thread: Option<&ThreadView>,
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> Vec<AnyElement> {
    let Some(thread) = thread else {
        return Vec::new();
    };

    let button = |label: &'static str, icon: &'static str, action: Box<dyn Action>| {
        let focus_handle = focus_handle.clone();
        toolbar_button(
            label,
            Some(icon),
            colors.button_background,
            colors.border,
            colors.foreground,
            move |_, window, cx| focus_handle.dispatch_action(action.as_ref(), window, cx),
        )
        .into_any_element()
    };

    let mut buttons = vec![
        button("Reply", "reply", Box::new(Reply)),
        button("Reply All", "reply-all", Box::new(ReplyAll)),
        button("Forward", "forward", Box::new(Forward)),
        action_divider(colors).into_any_element(),
        button("Archive", "archive", Box::new(Archive)),
        button("Delete", "trash-2", Box::new(Delete)),
        action_divider(colors).into_any_element(),
        button(
            if thread.is_flagged { "Unflag" } else { "Flag" },
            "flag",
            Box::new(ToggleFlag),
        ),
        if thread.unread_count > 0 {
            button("Mark Read", "eye", Box::new(ToggleRead))
        } else {
            button("Mark Unread", "eye-off", Box::new(ToggleRead))
        },
    ];

    let thread_has_attachments = thread
        .messages
        .iter()
        .any(|message| !message.attachments.is_empty());
    if thread_has_attachments {
        buttons.push(button(
            "Save attachments",
            "save",
            Box::new(SaveAttachments),
        ));
    }

    buttons
}

/// The move-to-mailbox buttons shown at the bottom of the reader.
///
/// Returns an empty [`Vec`] when no thread is selected.
fn reader_move_buttons(
    thread: Option<&ThreadView>,
    mailboxes: &[MailboxSummary],
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> Vec<AnyElement> {
    let Some(thread) = thread else {
        return Vec::new();
    };

    mailboxes
        .iter()
        .filter(|mailbox| mailbox.id != thread.mailbox_id)
        .map(|mailbox| {
            let action = MoveToMailbox {
                mailbox_id: mailbox.id,
            };
            let focus_handle = focus_handle.clone();
            toolbar_button(
                format!("Move: {}", mailbox.name),
                Some("folder"),
                colors.button_background,
                colors.border,
                colors.foreground,
                move |_, window, cx| focus_handle.dispatch_action(&action, window, cx),
            )
            .into_any_element()
        })
        .collect()
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
