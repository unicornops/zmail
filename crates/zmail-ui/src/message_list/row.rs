//! One message-list row, in either density (UI plan §2, "Message rows").
//!
//! [`MessageRow`] is what a row shows, whatever it comes from: a mailbox
//! thread, a search hit and, later, a draft or an outbox message. The
//! functions here only draw; the list view wraps the result in
//! [`row_shell`] and adds the click, hover and selection behaviour, so
//! those hooks stay in one place in `message_list.rs`.
//!
//! ```text
//! Comfortable:
//! • [avatar] Sender (bold if unread)  [3]        09:42
//!              Subject                   [label] [⚑] [📎]
//!              Snippet…
//!
//! Compact:
//! [•] Sender    [3]  Subject — snippet…   [⚑] [📎] 09:42
//! ```
//!
//! On hover the list passes quick actions, which take the date's place.

use gpui::{
    AnyElement, HighlightStyle, IntoElement, SharedString, Stateful, StyledText, div, prelude::*,
    px, relative, rgb,
};
use zmail_core::{
    DraftState, DraftSummary, OutboxJobState, OutboxJobSummary, SearchHit, ThreadSummary,
    participants::{parse_address_list, participant_names, sender_identity},
    text::count_noun,
    time::{HourCycle, format_full_date, format_list_date, parse_timestamp},
};

use super::selection::{row_background, row_colors};
use crate::components::tooltip::TooltipExt;
use crate::components::{Avatar, Badge, BadgeTone};
use crate::styles::{
    DensityTokens, ICON_SIZE_SM, ROW_PREVIEW_LINE_HEIGHT, ROW_SENDER_LINE_HEIGHT,
    ROW_TITLE_LINE_HEIGHT, TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TypeStyleExt,
    UNREAD_INDICATOR_SIZE, UiColors,
};
use crate::widgets::{EllipsisExt, HoverExt, account_dot, icon};

/// Share of a compact row the sender column takes.
const COMPACT_SENDER_WIDTH: f32 = 0.3;

/// Separates the subject from the snippet in a compact row.
const COMPACT_SNIPPET_SEPARATOR: &str = " \u{2014} ";

/// What one message-list row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MessageRow {
    /// Element id of the row; the ids and test selectors of its parts
    /// derive from it.
    pub(super) id: SharedString,
    /// The sender, or the participants of a longer thread.
    pub(super) sender: String,
    /// Name and address the avatar's initials and colour come from.
    pub(super) avatar_name: String,
    pub(super) avatar_address: Option<String>,
    pub(super) subject: String,
    /// One line of the latest message, or a draft's or queued message's
    /// status.
    pub(super) snippet: String,
    /// How the snippet reads: muted text, or a status that needs
    /// attention.
    pub(super) snippet_tone: SnippetTone,
    /// Short relative date ("10:42", "Yesterday", "Mon", "12 Oct").
    pub(super) date: String,
    /// Full timestamp shown when hovering the date.
    pub(super) date_tooltip: String,
    pub(super) unread: bool,
    /// Messages in the thread; a pill shows it when above one.
    pub(super) message_count: usize,
    pub(super) is_flagged: bool,
    pub(super) has_attachments: bool,
    /// A short tag beside the subject, such as the mailbox a search hit
    /// is in.
    pub(super) label: Option<String>,
    /// The account the thread belongs to, in lists that span accounts
    /// (the Unified Inbox); `None` in a single account's mailbox.
    pub(super) account: Option<RowAccount>,
}

/// The colour of a row's snippet line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SnippetTone {
    /// Secondary text.
    #[default]
    Muted,
    /// A problem, such as a message that failed to send.
    Danger,
}

/// The account chip of a row in a cross-account list: the account's
/// colour dot (the same as its sidebar header's) and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RowAccount {
    pub(super) id: i64,
    pub(super) name: String,
}

impl MessageRow {
    /// The row for a mailbox thread.
    pub(super) fn thread(
        id: SharedString,
        thread: &ThreadSummary,
        own_addresses: &[&str],
        now: &chrono::DateTime<chrono::Local>,
        hour_cycle: HourCycle,
    ) -> Self {
        let (name, address) = sender_identity(&thread.latest_sender);
        let sender = if thread.message_count > 1 && !thread.participants.trim().is_empty() {
            participant_names(&thread.participants, own_addresses)
        } else {
            name.clone()
        };
        Self {
            id,
            sender,
            avatar_name: name,
            avatar_address: address,
            subject: display_subject(&thread.subject).to_owned(),
            snippet: thread.preview.clone(),
            snippet_tone: SnippetTone::Muted,
            date: format_list_date(&thread.latest_message_at, now, hour_cycle),
            date_tooltip: format_full_date(&thread.latest_message_at, &now.timezone(), hour_cycle),
            unread: thread.unread_count > 0,
            message_count: thread.message_count,
            is_flagged: thread.is_flagged,
            has_attachments: thread.has_attachments,
            label: None,
            account: None,
        }
    }

    /// The row for a search result, labelled with its mailbox.
    pub(super) fn search_hit(
        id: SharedString,
        hit: &SearchHit,
        now: &chrono::DateTime<chrono::Local>,
        hour_cycle: HourCycle,
    ) -> Self {
        let (name, address) = sender_identity(&hit.latest_sender);
        Self {
            id,
            sender: name.clone(),
            avatar_name: name,
            avatar_address: address,
            subject: display_subject(&hit.subject).to_owned(),
            snippet: hit.preview.clone(),
            snippet_tone: SnippetTone::Muted,
            date: format_list_date(&hit.latest_message_at, now, hour_cycle),
            date_tooltip: format_full_date(&hit.latest_message_at, &now.timezone(), hour_cycle),
            unread: hit.unread_count > 0,
            // Search hits do not carry a message count.
            message_count: 0,
            is_flagged: hit.is_flagged,
            has_attachments: hit.has_attachments,
            // Server-only hits matched text the local index does not hold
            // yet.
            label: Some(if hit.from_server {
                format!("{} \u{2022} on server", hit.mailbox_name)
            } else {
                hit.mailbox_name.clone()
            }),
            account: None,
        }
    }

    /// The row for a draft: who it is to, its subject, and its state.
    pub(super) fn draft(
        id: SharedString,
        draft: &DraftSummary,
        now: &chrono::DateTime<chrono::Local>,
        hour_cycle: HourCycle,
    ) -> Self {
        let (sender, avatar_name, avatar_address) =
            recipient_line(&draft.recipient_summary, &draft.identity_name);
        let mut status = Vec::new();
        if draft.state != DraftState::Editing {
            status.push(draft.state.label().to_owned());
        }
        if draft.attachment_count > 0 {
            status.push(count_noun(
                draft.attachment_count,
                "attachment",
                "attachments",
            ));
        }
        Self {
            id,
            sender,
            avatar_name,
            avatar_address,
            subject: display_subject(&draft.subject).to_owned(),
            snippet: if status.is_empty() {
                "Draft".to_owned()
            } else {
                status.join(" \u{2022} ")
            },
            snippet_tone: if draft.state == DraftState::Failed {
                SnippetTone::Danger
            } else {
                SnippetTone::Muted
            },
            date: format_list_date(&draft.updated_at, now, hour_cycle),
            date_tooltip: format_full_date(&draft.updated_at, &now.timezone(), hour_cycle),
            unread: false,
            message_count: 0,
            is_flagged: false,
            has_attachments: draft.attachment_count > 0,
            label: None,
            account: None,
        }
    }

    /// The row for a message in the outbox: who it is to, its subject,
    /// and how sending is going, in plain words.
    pub(super) fn outbox_job(
        id: SharedString,
        job: &OutboxJobSummary,
        draft: Option<&DraftSummary>,
        now: &chrono::DateTime<chrono::Local>,
        hour_cycle: HourCycle,
    ) -> Self {
        let (sender, avatar_name, avatar_address) = recipient_line(
            draft
                .map(|draft| draft.recipient_summary.as_str())
                .unwrap_or(""),
            draft
                .map(|draft| draft.identity_name.as_str())
                .unwrap_or(""),
        );
        let status = outbox_status(job, now);
        let updated_at = draft.map(|draft| draft.updated_at.as_str()).unwrap_or("");
        Self {
            id,
            sender,
            avatar_name,
            avatar_address,
            subject: display_subject(draft.map(|draft| draft.subject.as_str()).unwrap_or(""))
                .to_owned(),
            snippet: status.text,
            snippet_tone: if status.failed {
                SnippetTone::Danger
            } else {
                SnippetTone::Muted
            },
            date: format_list_date(updated_at, now, hour_cycle),
            // The server's own words, for whoever wants them.
            date_tooltip: job
                .last_error_message
                .clone()
                .unwrap_or_else(|| format_full_date(updated_at, &now.timezone(), hour_cycle)),
            unread: false,
            message_count: 0,
            is_flagged: false,
            has_attachments: draft.is_some_and(|draft| draft.attachment_count > 0),
            label: None,
            account: None,
        }
    }

    /// What a screen reader announces for the row.
    pub(super) fn accessible_label(&self) -> String {
        let mut parts = Vec::new();
        if self.unread {
            parts.push("Unread".to_owned());
        }
        if self.is_flagged {
            parts.push("Flagged".to_owned());
        }
        parts.push(self.sender.clone());
        parts.push(self.subject.clone());
        if let Some(account) = &self.account {
            parts.push(account.name.clone());
        }
        if self.message_count > 1 {
            parts.push(format!("{} messages", self.message_count));
        }
        if self.has_attachments {
            parts.push("Has attachments".to_owned());
        }
        parts.push(self.date_tooltip.clone());
        parts.join(", ")
    }

    /// A test selector for one part of the row: `{id}-{part}`.
    fn part(&self, part: &str) -> String {
        format!("{}-{part}", self.id)
    }
}

/// The sender line of an outgoing message ("To: Priya Raman +2"), and
/// the name and address its avatar uses: the first recipient's, or the
/// sending identity's while there is none.
///
/// `summary` is the store's recipient summary: the first recipient, then
/// " +N" for the others, or "(no recipients)".
fn recipient_line(summary: &str, identity_name: &str) -> (String, String, Option<String>) {
    let summary = summary.trim();
    let (first, others) = match summary.rsplit_once(" +") {
        Some((first, others)) if others.parse::<usize>().is_ok() => (first, format!(" +{others}")),
        _ => (summary, String::new()),
    };
    match parse_address_list(first).into_iter().next() {
        Some(first) if summary != "(no recipients)" => {
            let name = first.display_name().to_owned();
            (format!("To: {name}{others}"), name, first.email)
        }
        _ => (
            "No recipients yet".to_owned(),
            identity_name.to_owned(),
            None,
        ),
    }
}

/// How sending a queued message is going, in plain words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutboxStatus {
    pub(super) text: String,
    /// The last attempt failed.
    pub(super) failed: bool,
    /// Offer "Retry now": the message is waiting for a retry, or gave up.
    pub(super) can_retry: bool,
}

/// "Sending…", "Failed — retrying in 5 min", "Couldn't send" and so on,
/// instead of the job's state name and attempt count.
pub(super) fn outbox_status(
    job: &OutboxJobSummary,
    now: &chrono::DateTime<chrono::Local>,
) -> OutboxStatus {
    let status = |text: &str, failed, can_retry| OutboxStatus {
        text: text.to_owned(),
        failed,
        can_retry,
    };
    match job.state {
        OutboxJobState::Queued => status("Waiting to send", false, false),
        OutboxJobState::Sending => status("Sending\u{2026}", false, false),
        OutboxJobState::Sent => status("Sent", false, false),
        OutboxJobState::Failed => status("Couldn\u{2019}t send", true, true),
        OutboxJobState::Retrying => {
            let wait = job
                .next_attempt_at
                .as_deref()
                .and_then(parse_timestamp)
                .map(|at| at.with_timezone(&chrono::Utc) - now.with_timezone(&chrono::Utc));
            let text = match wait {
                Some(wait) if wait.num_seconds() > 0 => {
                    format!("Failed \u{2014} retrying in {}", short_duration(wait))
                }
                _ => "Failed \u{2014} retrying now".to_owned(),
            };
            OutboxStatus {
                text,
                failed: true,
                can_retry: true,
            }
        }
    }
}

/// "30 sec", "5 min", "2 h", "3 days": rounded up, so a wait never
/// reads shorter than it is.
fn short_duration(duration: chrono::TimeDelta) -> String {
    let seconds = duration.num_seconds().max(1);
    let ceil = |unit: i64| (seconds + unit - 1) / unit;
    if seconds < 60 {
        format!("{seconds} sec")
    } else if seconds < 60 * 60 {
        format!("{} min", ceil(60))
    } else if seconds < 24 * 60 * 60 {
        format!("{} h", ceil(60 * 60))
    } else {
        count_noun(ceil(24 * 60 * 60) as usize, "day", "days")
    }
}

/// The subject to show; an empty one reads "(no subject)".
pub(super) fn display_subject(subject: &str) -> &str {
    if subject.trim().is_empty() {
        "(no subject)"
    } else {
        subject
    }
}

/// The clickable box around a row's content: the density's height and
/// padding, the divider and the resting, selected and hover backgrounds.
/// The list adds the click, hover and selection handlers.
pub(super) fn row_shell(
    id: SharedString,
    selected: bool,
    list_focused: bool,
    density: DensityTokens,
    colors: &UiColors,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .w_full()
        .h(px(density.message_row_height))
        .px(px(density.message_row_padding_x))
        .py(px(density.message_row_padding_y))
        .cursor_pointer()
        .border_b_1()
        .border_color(rgb(colors.border))
        .bg(rgb(row_background(selected, list_focused, colors)))
        .text_color(rgb(row_colors(selected, list_focused, colors).foreground))
        .hover_bg(selected, colors)
}

/// A row's content. `quick_actions`, when given, takes the date's place.
pub(super) fn row_content(
    row: MessageRow,
    quick_actions: Option<AnyElement>,
    density: DensityTokens,
    colors: &UiColors,
) -> AnyElement {
    if density.message_row_lines > 1 {
        comfortable_row(row, quick_actions, density, colors).into_any_element()
    } else {
        compact_row(row, quick_actions, density, colors).into_any_element()
    }
}

/// Three fixed-height lines beside the avatar. Every line has an explicit
/// height and truncates, so a row never grows past the density's height
/// and long text never pushes the date out of view.
fn comfortable_row(
    row: MessageRow,
    quick_actions: Option<AnyElement>,
    density: DensityTokens,
    colors: &UiColors,
) -> gpui::Div {
    let avatar = Avatar::new(row.avatar_name.clone())
        .when_some(row.avatar_address.clone(), |avatar, address| {
            avatar.address(address)
        })
        .size(density.avatar_size);
    let line = |height: f32| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(height))
            .line_height(px(height))
    };

    // The unread dot sits in the row's leading padding, level with the
    // sender, so the sender, subject and snippet line up.
    let dot = unread_indicator(&row, colors)
        .absolute()
        .left(px(
            -(density.message_row_padding_x + UNREAD_INDICATOR_SIZE) / 2.0
        ))
        .top(px((ROW_SENDER_LINE_HEIGHT - UNREAD_INDICATOR_SIZE) / 2.0));

    div()
        .relative()
        .flex()
        .items_start()
        .gap(px(density.row_gap))
        .size_full()
        .child(dot)
        .when(density.show_avatar, |this| {
            this.child(
                div()
                    .flex_none()
                    .debug_selector({
                        let part = row.part("avatar");
                        move || part
                    })
                    .child(avatar),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(density.message_row_line_gap))
                .child(
                    line(ROW_SENDER_LINE_HEIGHT)
                        .child(sender(&row, colors).flex_1())
                        .children(thread_count(&row))
                        .child(trailing(&row, quick_actions, colors)),
                )
                .child(
                    line(ROW_TITLE_LINE_HEIGHT)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .type_style(TYPE_BODY)
                                .line_height(px(ROW_TITLE_LINE_HEIGHT))
                                .ellipsis()
                                .child(row.subject.clone()),
                        )
                        .children(status_icons(&row, colors)),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(ROW_PREVIEW_LINE_HEIGHT))
                        .type_style(TYPE_BODY)
                        .line_height(px(ROW_PREVIEW_LINE_HEIGHT))
                        .text_color(rgb(snippet_color(&row, colors)))
                        .ellipsis()
                        .child(row.snippet.clone()),
                ),
        )
}

/// One line: sender, subject and snippet, then the icons and the date.
fn compact_row(
    row: MessageRow,
    quick_actions: Option<AnyElement>,
    density: DensityTokens,
    colors: &UiColors,
) -> gpui::Div {
    let line_height = density.message_row_line_height;
    div()
        .flex()
        .items_center()
        .gap(px(density.row_gap))
        .size_full()
        .line_height(px(line_height))
        .child(unread_indicator(&row, colors))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .w(relative(COMPACT_SENDER_WIDTH))
                .min_w_0()
                .child(sender(&row, colors).flex_1())
                .children(thread_count(&row)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .type_style(TYPE_BODY)
                .line_height(px(line_height))
                .ellipsis()
                .child(subject_and_snippet(&row, colors)),
        )
        .children(status_icons(&row, colors))
        .child(trailing(&row, quick_actions, colors))
}

/// "Subject — snippet" as one text run, the snippet muted, so the line
/// truncates as a whole.
fn subject_and_snippet(row: &MessageRow, colors: &UiColors) -> StyledText {
    let snippet = row.snippet.trim();
    if snippet.is_empty() {
        return StyledText::new(row.subject.clone());
    }
    let text = format!("{}{COMPACT_SNIPPET_SEPARATOR}{snippet}", row.subject);
    let muted = HighlightStyle {
        color: Some(rgb(snippet_color(row, colors)).into()),
        ..HighlightStyle::default()
    };
    let start = row.subject.len();
    let end = text.len();
    StyledText::new(text).with_highlights([(start..end, muted)])
}

fn snippet_color(row: &MessageRow, colors: &UiColors) -> u32 {
    match row.snippet_tone {
        SnippetTone::Muted => colors.muted,
        SnippetTone::Danger => colors.danger,
    }
}

fn sender(row: &MessageRow, colors: &UiColors) -> gpui::Div {
    div()
        .min_w_0()
        .type_style(if row.unread {
            TYPE_BODY_STRONG
        } else {
            TYPE_BODY
        })
        .line_height(px(ROW_SENDER_LINE_HEIGHT))
        .text_color(rgb(colors.foreground))
        .ellipsis()
        .child(row.sender.clone())
}

/// The unread dot. In compact rows read rows keep its space, so senders
/// line up.
fn unread_indicator(row: &MessageRow, colors: &UiColors) -> gpui::Div {
    div()
        .flex_none()
        .size(px(UNREAD_INDICATOR_SIZE))
        .rounded_full()
        .when(row.unread, |this| {
            this.bg(rgb(colors.accent)).debug_selector({
                let part = row.part("unread");
                move || part
            })
        })
}

/// The thread-count pill, for threads of more than one message.
fn thread_count(row: &MessageRow) -> Option<gpui::Div> {
    (row.message_count > 1).then(|| {
        div()
            .flex_none()
            .debug_selector({
                let part = row.part("count");
                move || part
            })
            .child(Badge::count(row.message_count).tone(BadgeTone::Neutral))
    })
}

/// The account chip and label tag, then the flag and attachment icons.
fn status_icons(row: &MessageRow, colors: &UiColors) -> Vec<gpui::Div> {
    let mut icons = Vec::new();
    if let Some(account) = row.account.clone() {
        icons.push(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .max_w(relative(0.4))
                .debug_selector({
                    let part = row.part("account");
                    move || part
                })
                .child(account_dot(account.id))
                .child(
                    div()
                        .min_w_0()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .ellipsis()
                        .child(account.name),
                ),
        );
    }
    if let Some(label) = row.label.clone() {
        icons.push(
            div()
                .flex_none()
                .max_w(relative(0.4))
                .type_style(TYPE_CAPTION)
                .text_color(rgb(colors.muted))
                .ellipsis()
                .child(label),
        );
    }
    let glyph = |part: &str, name: &'static str, color: u32| {
        div()
            .flex_none()
            .debug_selector({
                let part = row.part(part);
                move || part
            })
            .child(icon(name, ICON_SIZE_SM, color))
    };
    if row.is_flagged {
        icons.push(glyph("flag", "flag", colors.warning));
    }
    if row.has_attachments {
        icons.push(glyph("attachment", "paperclip", colors.muted));
    }
    icons
}

/// The date, or the quick actions in its place.
fn trailing(row: &MessageRow, quick_actions: Option<AnyElement>, colors: &UiColors) -> AnyElement {
    match quick_actions {
        Some(actions) => div()
            .flex()
            .flex_none()
            .items_center()
            .debug_selector({
                let part = row.part("actions");
                move || part
            })
            .child(actions)
            .into_any_element(),
        None => div()
            .id(SharedString::from(row.part("date")))
            .debug_selector({
                let part = row.part("date");
                move || part
            })
            .flex_none()
            .whitespace_nowrap()
            .type_style(TYPE_CAPTION)
            .text_color(rgb(colors.muted))
            .with_tooltip(row.date_tooltip.clone(), None)
            .child(row.date.clone())
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(message_count: usize, participants: &str) -> ThreadSummary {
        ThreadSummary {
            id: 7,
            mailbox_id: 1,
            account_id: 1,
            subject: "Q4 roadmap review".to_owned(),
            latest_sender: "Priya Raman <priya.raman@example.org>".to_owned(),
            participants: participants.to_owned(),
            preview: "Thanks Marcus".to_owned(),
            message_count,
            unread_count: 1,
            is_flagged: true,
            has_attachments: true,
            latest_message_at: "2026-03-20T09:15:00Z".to_owned(),
        }
    }

    fn row(thread: &ThreadSummary) -> MessageRow {
        let now = chrono::Local::now();
        MessageRow::thread(
            "thread-row-7".into(),
            thread,
            &["me@example.org"],
            &now,
            HourCycle::H24,
        )
    }

    #[test]
    fn a_single_message_shows_its_sender() {
        let row = row(&thread(1, "Priya Raman <priya.raman@example.org>"));
        assert_eq!(row.sender, "Priya Raman");
        assert_eq!(row.avatar_name, "Priya Raman");
        assert_eq!(
            row.avatar_address.as_deref(),
            Some("priya.raman@example.org")
        );
        assert!(row.unread);
    }

    #[test]
    fn a_thread_lists_its_participants_and_counts_in_the_pill() {
        let row = row(&thread(
            3,
            "Priya Raman <priya.raman@example.org>, Me <me@example.org>",
        ));
        // The count is the pill's job, not the sender line's.
        assert_eq!(row.sender, "Priya, me");
        assert_eq!(row.message_count, 3);
        // The avatar is the latest sender's.
        assert_eq!(row.avatar_name, "Priya Raman");
    }

    #[test]
    fn empty_subjects_read_no_subject() {
        let mut summary = thread(1, "");
        summary.subject = "  ".to_owned();
        assert_eq!(row(&summary).subject, "(no subject)");
    }

    #[test]
    fn the_accessible_label_reads_the_row() {
        let row = row(&thread(3, "Priya Raman <priya.raman@example.org>"));
        let label = row.accessible_label();
        assert!(
            label.starts_with(
                "Unread, Flagged, Priya Raman, Q4 roadmap review, 3 messages, Has attachments, "
            ),
            "{label}"
        );
    }

    #[test]
    fn search_hits_are_labelled_with_their_mailbox() {
        let now = chrono::Local::now();
        let hit = SearchHit {
            account_id: 1,
            mailbox_id: 1,
            mailbox_name: "Archive".to_owned(),
            thread_id: 2,
            subject: String::new(),
            latest_sender: "kai@example.org".to_owned(),
            participants: String::new(),
            preview: String::new(),
            unread_count: 0,
            is_flagged: false,
            has_attachments: false,
            latest_message_at: "2026-03-20T09:15:00Z".to_owned(),
            from_server: true,
        };
        let row = MessageRow::search_hit("search-row-2".into(), &hit, &now, HourCycle::H24);
        assert_eq!(row.sender, "kai@example.org");
        assert_eq!(row.subject, "(no subject)");
        assert_eq!(row.label.as_deref(), Some("Archive \u{2022} on server"));
        assert!(!row.unread);
    }

    fn job(state: OutboxJobState, next_attempt_at: Option<&str>) -> OutboxJobSummary {
        OutboxJobSummary {
            id: 3,
            draft_id: 4,
            account_id: 1,
            state,
            attempt_count: 3,
            next_attempt_at: next_attempt_at.map(str::to_owned),
            last_error_message: Some("421 4.7.0 try again later".to_owned()),
        }
    }

    #[test]
    fn outbox_status_reads_in_plain_words() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-18T10:00:00Z")
            .expect("a timestamp")
            .with_timezone(&chrono::Local);
        let text = |state, next: Option<&str>| outbox_status(&job(state, next), &now).text;
        assert_eq!(text(OutboxJobState::Queued, None), "Waiting to send");
        assert_eq!(text(OutboxJobState::Sending, None), "Sending\u{2026}");
        assert_eq!(
            text(OutboxJobState::Retrying, Some("2026-10-18T10:05:00Z")),
            "Failed \u{2014} retrying in 5 min"
        );
        assert_eq!(
            text(OutboxJobState::Retrying, Some("2026-10-18T10:04:01Z")),
            "Failed \u{2014} retrying in 5 min",
            "waits round up"
        );
        assert_eq!(
            text(OutboxJobState::Retrying, Some("2026-10-18T10:00:30Z")),
            "Failed \u{2014} retrying in 30 sec"
        );
        assert_eq!(
            text(OutboxJobState::Retrying, Some("2026-10-18T12:00:00Z")),
            "Failed \u{2014} retrying in 2 h"
        );
        assert_eq!(
            text(OutboxJobState::Retrying, Some("2026-10-18T09:00:00Z")),
            "Failed \u{2014} retrying now"
        );
        assert_eq!(text(OutboxJobState::Failed, None), "Couldn\u{2019}t send");
        // No state names, attempt counts or job ids.
        for state in [
            OutboxJobState::Queued,
            OutboxJobState::Sending,
            OutboxJobState::Retrying,
            OutboxJobState::Failed,
        ] {
            let status = text(state, Some("2026-10-18T10:05:00Z"));
            assert!(
                !status.contains("attempt") && !status.contains('3'),
                "{status}"
            );
        }
        let failed = outbox_status(&job(OutboxJobState::Failed, None), &now);
        assert!(failed.failed && failed.can_retry);
        let sending = outbox_status(&job(OutboxJobState::Sending, None), &now);
        assert!(!sending.failed && !sending.can_retry);
    }

    #[test]
    fn outgoing_rows_name_their_recipients() {
        assert_eq!(
            recipient_line("Priya Raman <priya@example.org> +2", "Sam"),
            (
                "To: Priya Raman +2".to_owned(),
                "Priya Raman".to_owned(),
                Some("priya@example.org".to_owned())
            )
        );
        assert_eq!(
            recipient_line("kai@example.org", "Sam").0,
            "To: kai@example.org"
        );
        assert_eq!(
            recipient_line("(no recipients)", "Sam"),
            ("No recipients yet".to_owned(), "Sam".to_owned(), None)
        );
    }

    #[test]
    fn a_failed_outbox_row_says_so_in_the_danger_colour() {
        let now = chrono::Local::now();
        let draft = DraftSummary {
            id: 4,
            account_id: 1,
            identity_id: 1,
            identity_name: "Sam".to_owned(),
            recipient_summary: "Priya Raman <priya@example.org>".to_owned(),
            subject: "Q4 estimates".to_owned(),
            attachment_count: 1,
            state: DraftState::Failed,
            updated_at: "2026-10-18T09:00:00Z".to_owned(),
        };
        let row = MessageRow::outbox_job(
            "outbox-row-3".into(),
            &job(OutboxJobState::Failed, None),
            Some(&draft),
            &now,
            HourCycle::H24,
        );
        assert_eq!(row.sender, "To: Priya Raman");
        assert_eq!(row.subject, "Q4 estimates");
        assert_eq!(row.snippet, "Couldn\u{2019}t send");
        assert_eq!(row.snippet_tone, SnippetTone::Danger);
        assert!(row.has_attachments);
        assert_eq!(row.date_tooltip, "421 4.7.0 try again later");

        let row = MessageRow::draft("draft-row-4".into(), &draft, &now, HourCycle::H24);
        assert_eq!(row.snippet, "Failed \u{2022} 1 attachment");
        assert_eq!(row.snippet_tone, SnippetTone::Danger);
    }
}
