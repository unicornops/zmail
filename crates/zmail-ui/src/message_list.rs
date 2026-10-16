//! The message-list pane: threads, search results, drafts or the outbox.
//!
//! [`MessageListView`] is its own entity with its own focus handle and the
//! `MessageList` key context. The shell pushes what to show with
//! [`MessageListView::set_state`]; the list reports what the user picked as
//! [`MessageListEvent`]s.
//!
//! Thread rows are paged out of the local store on demand (the mailbox can
//! be large), and the loaded page is cached: rendering, including the
//! re-render a hover causes, never queries the store. The shell calls
//! [`MessageListView::reload_rows`] after a store refresh, which re-reads
//! the cached page and only re-renders when it changed.

use std::ops::Range;

use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    Render, ScrollStrategy, SharedString, StatefulInteractiveElement, UniformListScrollHandle,
    Window, div, prelude::*, px, rgb, uniform_list,
};
use std::sync::Arc;
use zmail_core::{
    DraftState, DraftSummary, MailboxSummary, OutboxJobSummary, SearchHit, ThreadSummary,
    participants::{participant_summary, sender_display_name},
    text::count_noun,
    time::{HourCycle, format_full_date, format_list_date},
};
use zmail_store::LocalStore;

use crate::ListMode;
use crate::actions::{
    AddAccount, OpenSelected, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
use crate::pane::pane_focus_indicator;
use crate::styles::{
    AVATAR_SIZE, ICON_SIZE_SM, LIST_ITEM_HEIGHT, ROW_LINE_GAP, ROW_PADDING_Y,
    ROW_PREVIEW_LINE_HEIGHT, ROW_SENDER_LINE_HEIGHT, ROW_TITLE_LINE_HEIGHT, UNREAD_INDICATOR_SIZE,
    UiColors,
};
use crate::tooltip::text_tooltip;
use crate::widgets::{
    EllipsisExt, HoverExt, avatar, badge, empty_state, icon, interactive_background, list_row,
    message_avatar_background, toolbar_button,
};

/// The key context the list sets.
pub(crate) const KEY_CONTEXT: &str = "MessageList";

/// Rows loaded beyond the visible range, so small scrolls and arrow keys
/// stay inside the cached page.
const ROW_PAGE_PADDING: usize = 50;

/// What the shell shows in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageListState {
    pub(crate) mode: ListMode,
    pub(crate) has_accounts: bool,
    pub(crate) mailbox: Option<MailboxSummary>,
    /// The user's own addresses, shown as "me".
    pub(crate) own_addresses: Vec<String>,
    pub(crate) selected_thread_id: Option<i64>,
    pub(crate) search_results: Vec<SearchHit>,
    /// The query that produced `search_results`.
    pub(crate) search_query: String,
    pub(crate) drafts: Vec<DraftSummary>,
    pub(crate) selected_draft_id: Option<i64>,
    pub(crate) outbox_jobs: Vec<OutboxJobSummary>,
    pub(crate) selected_outbox_job_id: Option<i64>,
}

impl Default for MessageListState {
    fn default() -> Self {
        Self {
            mode: ListMode::Mail,
            has_accounts: false,
            mailbox: None,
            own_addresses: Vec::new(),
            selected_thread_id: None,
            search_results: Vec::new(),
            search_query: String::new(),
            drafts: Vec::new(),
            selected_draft_id: None,
            outbox_jobs: Vec::new(),
            selected_outbox_job_id: None,
        }
    }
}

/// What the user did in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MessageListEvent {
    /// A thread or search result was clicked or reached with the arrows.
    OpenThread { thread_id: i64, mailbox_id: i64 },
    /// A draft was clicked or reached with the arrows.
    OpenDraft(i64),
    /// An outbox message was clicked or reached with the arrows.
    SelectOutboxJob(i64),
    /// Enter: move on to the detail pane.
    Activate,
    /// Reading the rows failed.
    Error(String),
}

/// The cached page of thread rows.
#[derive(Debug, Clone)]
struct RowPage {
    mailbox_id: i64,
    start: usize,
    rows: Vec<ThreadSummary>,
}

impl RowPage {
    fn covers(&self, mailbox_id: i64, range: &Range<usize>) -> bool {
        self.mailbox_id == mailbox_id
            && range.start >= self.start
            && range.end <= self.start + self.rows.len()
    }
}

pub(crate) struct MessageListView {
    focus_handle: FocusHandle,
    store: Arc<LocalStore>,
    hour_cycle: HourCycle,
    state: MessageListState,
    page: Option<RowPage>,
    /// Where the selected thread was last seen, so the arrow keys do not
    /// have to search the mailbox for it.
    selected_index_hint: Option<(i64, usize)>,
    thread_scroll: UniformListScrollHandle,
    /// Test instrumentation: renders and store queries for rows.
    #[cfg(test)]
    pub(crate) counters: crate::pane::RenderCounters,
}

impl EventEmitter<MessageListEvent> for MessageListView {}

impl Focusable for MessageListView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl MessageListView {
    pub(crate) fn new(
        store: Arc<LocalStore>,
        hour_cycle: HourCycle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            store,
            hour_cycle,
            state: MessageListState::default(),
            page: None,
            selected_index_hint: None,
            thread_scroll: UniformListScrollHandle::new(),
            #[cfg(test)]
            counters: Default::default(),
        }
    }

    /// Replaces what the list shows; re-renders only when it changed.
    pub(crate) fn set_state(&mut self, state: MessageListState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        let mailbox_changed = self.state.mailbox.as_ref().map(|mailbox| mailbox.id)
            != state.mailbox.as_ref().map(|mailbox| mailbox.id);
        let counts_changed = self.state.mailbox != state.mailbox;
        self.state = state;
        if mailbox_changed || counts_changed {
            self.page = None;
        }
        if mailbox_changed {
            self.selected_index_hint = None;
            self.thread_scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Re-reads the cached page of thread rows after a store refresh and
    /// re-renders only if a row changed.
    pub(crate) fn reload_rows(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.page.as_ref() else {
            return;
        };
        let range = page.start..page.start + page.rows.len().max(1);
        match self.store.list_threads(page.mailbox_id, range) {
            Ok(rows) if rows != page.rows => {
                if let Some(page) = self.page.as_mut() {
                    page.rows = rows;
                }
                cx.notify();
            }
            Ok(_) => {}
            Err(error) => cx.emit(MessageListEvent::Error(error.to_string())),
        }
    }

    /// Rows in `range` of the selected mailbox, from the cached page when
    /// it covers them.
    fn thread_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<ThreadSummary> {
        let Some(mailbox_id) = self.state.mailbox.as_ref().map(|mailbox| mailbox.id) else {
            return Vec::new();
        };
        if !self
            .page
            .as_ref()
            .is_some_and(|page| page.covers(mailbox_id, &range))
        {
            let total = self
                .state
                .mailbox
                .as_ref()
                .map(|mailbox| mailbox.total_threads)
                .unwrap_or_default();
            let start = range.start.saturating_sub(ROW_PAGE_PADDING);
            let end = (range.end + ROW_PAGE_PADDING).min(total.max(range.end));
            #[cfg(test)]
            {
                self.counters.queries += 1;
            }
            match self.store.list_threads(mailbox_id, start..end) {
                Ok(rows) => {
                    self.page = Some(RowPage {
                        mailbox_id,
                        start,
                        rows,
                    })
                }
                Err(error) => {
                    // Rendering never clears an error; only the user
                    // dismisses it.
                    cx.emit(MessageListEvent::Error(error.to_string()));
                    return Vec::new();
                }
            }
        }
        let Some(page) = self.page.as_ref() else {
            return Vec::new();
        };
        let from = range.start.saturating_sub(page.start).min(page.rows.len());
        let to = range.end.saturating_sub(page.start).min(page.rows.len());
        page.rows[from..to].to_vec()
    }

    // -----------------------------------------------------------------------
    // Keyboard selection
    // -----------------------------------------------------------------------

    fn row_count(&self) -> usize {
        match self.state.mode {
            ListMode::Mail => self
                .state
                .mailbox
                .as_ref()
                .map(|mailbox| mailbox.total_threads)
                .unwrap_or_default(),
            ListMode::Search => self.state.search_results.len(),
            ListMode::Drafts => self.state.drafts.len(),
            ListMode::Outbox => self.state.outbox_jobs.len(),
        }
    }

    fn current_index(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        match self.state.mode {
            ListMode::Mail => {
                let selected = self.state.selected_thread_id?;
                if let Some((id, index)) = self.selected_index_hint
                    && id == selected
                {
                    return Some(index);
                }
                if let Some(page) = self.page.as_ref()
                    && let Some(position) = page.rows.iter().position(|row| row.id == selected)
                {
                    return Some(page.start + position);
                }
                // Not in the cached page: look through the start of the
                // mailbox, which is where the selection usually is.
                let count = self.row_count().min(1000);
                self.thread_rows(0..count, cx)
                    .iter()
                    .position(|row| row.id == selected)
            }
            ListMode::Search => {
                let selected = self.state.selected_thread_id?;
                self.state
                    .search_results
                    .iter()
                    .position(|hit| hit.thread_id == selected)
            }
            ListMode::Drafts => {
                let selected = self.state.selected_draft_id?;
                self.state
                    .drafts
                    .iter()
                    .position(|draft| draft.id == selected)
            }
            ListMode::Outbox => {
                let selected = self.state.selected_outbox_job_id?;
                self.state
                    .outbox_jobs
                    .iter()
                    .position(|job| job.id == selected)
            }
        }
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.row_count() {
            return;
        }
        match self.state.mode {
            ListMode::Mail => {
                let Some(mailbox_id) = self.state.mailbox.as_ref().map(|mailbox| mailbox.id) else {
                    return;
                };
                let Some(thread) = self.thread_rows(index..index + 1, cx).into_iter().next() else {
                    return;
                };
                self.selected_index_hint = Some((thread.id, index));
                self.thread_scroll
                    .scroll_to_item(index, ScrollStrategy::Nearest);
                self.state.selected_thread_id = Some(thread.id);
                cx.emit(MessageListEvent::OpenThread {
                    thread_id: thread.id,
                    mailbox_id,
                });
            }
            ListMode::Search => {
                let Some(hit) = self.state.search_results.get(index) else {
                    return;
                };
                let (thread_id, mailbox_id) = (hit.thread_id, hit.mailbox_id);
                self.state.selected_thread_id = Some(thread_id);
                cx.emit(MessageListEvent::OpenThread {
                    thread_id,
                    mailbox_id,
                });
            }
            ListMode::Drafts => {
                let Some(draft_id) = self.state.drafts.get(index).map(|draft| draft.id) else {
                    return;
                };
                self.state.selected_draft_id = Some(draft_id);
                cx.emit(MessageListEvent::OpenDraft(draft_id));
            }
            ListMode::Outbox => {
                let Some(job_id) = self.state.outbox_jobs.get(index).map(|job| job.id) else {
                    return;
                };
                self.state.selected_outbox_job_id = Some(job_id);
                cx.emit(MessageListEvent::SelectOutboxJob(job_id));
            }
        }
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.current_index(cx).map(|index| index + 1).unwrap_or(0);
        self.select_index(next, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let previous = self
            .current_index(cx)
            .map(|index| index.saturating_sub(1))
            .unwrap_or(0);
        self.select_index(previous, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.select_index(0, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(last) = self.row_count().checked_sub(1) {
            self.select_index(last, cx);
        }
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(MessageListEvent::Activate);
    }
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

impl Render for MessageListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.counters.renders += 1;
        }
        let colors = *cx.global::<UiColors>();
        let focused = self.focus_handle.contains_focused(window, cx);
        let (title, subtitle) = self.header_text();

        div()
            .id("message-list")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::open_selected))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.surface))
            .border_r_1()
            .border_color(rgb(colors.border))
            .child(
                pane_focus_indicator(
                    div()
                        .px_3()
                        .py_3()
                        .border_b_1()
                        .border_color(rgb(colors.border)),
                    focused,
                    &colors,
                )
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .child(subtitle),
                ),
            )
            .child(match self.state.mode {
                ListMode::Mail => self.render_threads(&colors, cx),
                ListMode::Search => self.render_search_results(&colors, cx),
                ListMode::Drafts => self.render_drafts(&colors, cx),
                ListMode::Outbox => self.render_outbox(&colors, cx),
            })
    }
}

impl MessageListView {
    fn header_text(&self) -> (String, String) {
        let mailbox = self.state.mailbox.as_ref();
        let title = match self.state.mode {
            ListMode::Mail => mailbox
                .map(|mailbox| mailbox.name.clone())
                .unwrap_or_else(|| "Mail".to_owned()),
            ListMode::Search => "Search".to_owned(),
            ListMode::Drafts => "Drafts".to_owned(),
            ListMode::Outbox => "Outbox".to_owned(),
        };
        let subtitle = match self.state.mode {
            ListMode::Mail => mailbox
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
                count_noun(self.state.search_results.len(), "result", "results"),
                self.state.search_query
            ),
            ListMode::Drafts => count_noun(self.state.drafts.len(), "draft", "drafts"),
            ListMode::Outbox => count_noun(
                self.state.outbox_jobs.len(),
                "message waiting to send",
                "messages waiting to send",
            ),
        };
        (title, subtitle)
    }

    fn render_threads(&mut self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let colors = *colors;
        let thread_count = self
            .state
            .mailbox
            .as_ref()
            .map(|mailbox| mailbox.total_threads)
            .unwrap_or_default();
        if !self.state.has_accounts {
            let focus_handle = self.focus_handle.clone();
            return empty_state(
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
                        move |_, window, cx| focus_handle.dispatch_action(&AddAccount, window, cx),
                    )
                    .into_any_element(),
                ),
                &colors,
            );
        }
        if thread_count == 0 {
            return empty_state(
                "No local mail yet",
                "Sync the selected account to populate the local mirror.",
                "inbox",
                None,
                &colors,
            );
        }

        div()
            .flex_1()
            .overflow_hidden()
            .child(
                uniform_list(
                    "threads",
                    thread_count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        this.render_thread_rows(range, &colors, cx)
                    }),
                )
                .track_scroll(&self.thread_scroll)
                .h_full(),
            )
            .into_any_element()
    }

    fn render_thread_rows(
        &mut self,
        range: Range<usize>,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Stateful<gpui::Div>> {
        let Some(mailbox_id) = self.state.mailbox.as_ref().map(|mailbox| mailbox.id) else {
            return Vec::new();
        };
        let start = range.start;
        let threads = self.thread_rows(range, cx);
        let now = chrono::Local::now();
        let hour_cycle = self.hour_cycle;
        let own_addresses = self
            .state
            .own_addresses
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();

        threads
            .into_iter()
            .enumerate()
            .map(|(offset, thread)| {
                let index = start + offset;
                let thread_id = thread.id;
                let sender_name = sender_display_name(&thread.latest_sender);
                let sender_line = if thread.message_count > 1
                    && !thread.participants.trim().is_empty()
                {
                    participant_summary(&thread.participants, thread.message_count, &own_addresses)
                } else {
                    sender_name.clone()
                };
                let row_id = thread_row_id(thread_id);
                let is_selected = Some(thread_id) == self.state.selected_thread_id;
                let background_color =
                    rgb(interactive_background(is_selected, colors.surface, colors));

                div()
                    .id(row_id.clone())
                    .debug_selector(move || format!("thread-row-{index}"))
                    .w_full()
                    .h(px(LIST_ITEM_HEIGHT))
                    .px_3()
                    .py(px(ROW_PADDING_Y))
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .bg(background_color)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_index_hint = Some((thread_id, index));
                        this.state.selected_thread_id = Some(thread_id);
                        cx.emit(MessageListEvent::OpenThread {
                            thread_id,
                            mailbox_id,
                        });
                        cx.notify();
                    }))
                    .hover_bg(is_selected, colors)
                    .child(conversation_list_row(
                        ConversationRow {
                            id: row_id.clone(),
                            subject: &thread.subject,
                            sender_line: &sender_line,
                            preview: &thread.preview,
                            date: format_list_date(&thread.latest_message_at, &now, hour_cycle),
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
                        colors,
                    ))
            })
            .collect()
    }

    fn render_search_results(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        if self.state.search_results.is_empty() {
            return empty_state(
                "No search results",
                "Type a query above and run search against the local index.",
                "search",
                None,
                colors,
            );
        }
        let now = chrono::Local::now();
        div()
            .id("search-results-list")
            .flex_1()
            .overflow_y_scroll()
            .children(self.state.search_results.iter().map(|hit| {
                let sender_name = sender_display_name(&hit.latest_sender);
                let mailbox_id = hit.mailbox_id;
                let thread_id = hit.thread_id;
                let row_id = search_row_id(thread_id);
                let is_selected = Some(thread_id) == self.state.selected_thread_id;
                let background_color =
                    rgb(interactive_background(is_selected, colors.surface, colors));

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
                        this.state.selected_thread_id = Some(thread_id);
                        cx.emit(MessageListEvent::OpenThread {
                            thread_id,
                            mailbox_id,
                        });
                        cx.notify();
                    }))
                    .hover_bg(is_selected, colors)
                    .child(conversation_list_row(
                        ConversationRow {
                            id: row_id.clone(),
                            subject: &hit.subject,
                            sender_line: &format!("{} \u{2022} {}", hit.mailbox_name, sender_name),
                            preview: &hit.preview,
                            date: format_list_date(&hit.latest_message_at, &now, self.hour_cycle),
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
                        colors,
                    ))
            }))
            .into_any_element()
    }

    fn render_drafts(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        if self.state.drafts.is_empty() {
            return empty_state(
                "No drafts",
                "Press Compose to start a new message.",
                "pen-square",
                None,
                colors,
            );
        }
        let now = chrono::Local::now();
        div()
            .id("drafts-list")
            .flex_1()
            .overflow_y_scroll()
            .children(self.state.drafts.iter().map(|draft| {
                let draft_id = draft.id;
                let is_selected = Some(draft_id) == self.state.selected_draft_id;
                let background_color =
                    rgb(interactive_background(is_selected, colors.surface, colors));

                div()
                    .id(draft_row_id(draft_id))
                    .px_3()
                    .py_3()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .bg(background_color)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.selected_draft_id = Some(draft_id);
                        cx.emit(MessageListEvent::OpenDraft(draft_id));
                        cx.notify();
                    }))
                    .hover_bg(is_selected, colors)
                    .child(list_row(
                        display_subject(&draft.subject),
                        &format_list_date(&draft.updated_at, &now, self.hour_cycle),
                        &recipient_line(&draft.recipient_summary),
                        "",
                        &draft_meta(draft),
                        colors,
                    ))
            }))
            .into_any_element()
    }

    fn render_outbox(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        if self.state.outbox_jobs.is_empty() {
            return empty_state(
                "No outbox jobs",
                "Queued and retrying deliveries will appear here.",
                "send",
                None,
                colors,
            );
        }
        let now = chrono::Local::now();
        div()
            .id("outbox-list")
            .flex_1()
            .overflow_y_scroll()
            .children(self.state.outbox_jobs.iter().map(|job| {
                let job_id = job.id;
                let is_selected = Some(job_id) == self.state.selected_outbox_job_id;
                let background_color =
                    rgb(interactive_background(is_selected, colors.surface, colors));
                let draft = self
                    .state
                    .drafts
                    .iter()
                    .find(|draft| draft.id == job.draft_id);

                div()
                    .id(outbox_row_id(job_id))
                    .px_3()
                    .py_3()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .bg(background_color)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.selected_outbox_job_id = Some(job_id);
                        cx.emit(MessageListEvent::SelectOutboxJob(job_id));
                        cx.notify();
                    }))
                    .hover_bg(is_selected, colors)
                    .child(list_row(
                        display_subject(draft.map(|draft| draft.subject.as_str()).unwrap_or("")),
                        &job.next_attempt_at
                            .as_deref()
                            .map(|at| {
                                format!("Retry {}", format_list_date(at, &now, self.hour_cycle))
                            })
                            .unwrap_or_default(),
                        &recipient_line(
                            draft
                                .map(|draft| draft.recipient_summary.as_str())
                                .unwrap_or(""),
                        ),
                        job.last_error_message.as_deref().unwrap_or(""),
                        job.state.label(),
                        colors,
                    ))
            }))
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
        // Read rows keep the space but draw nothing, so the dot does not
        // show up against selected or hovered backgrounds.
        .when(is_unread, |this| this.bg(rgb(colors.accent)))
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
