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

mod context_menu;
mod row;
mod selection;

use std::ops::Range;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, MouseButton, MouseDownEvent, Pixels, Point, Render, Role, ScrollStrategy,
    SharedString, Subscription, UniformListScrollHandle, Window, div, prelude::*, rgb,
    uniform_list,
};
use std::sync::Arc;
use zmail_core::{
    DraftState, DraftSummary, MailboxSelection, MailboxSummary, OutboxJobSummary, SearchHit,
    ThreadSummary,
    preferences::Density,
    text::count_noun,
    time::{HourCycle, format_list_date},
};
use zmail_store::LocalStore;

pub(crate) use self::selection::ListSelection;
use self::selection::{OpenMovePicker, row_colors};
use crate::ListMode;
use crate::actions::{
    AddAccount, OpenSelected, RetryOutbox, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
use crate::components::{Button, ButtonSize, Menu};
use crate::pane::{dispatch_from, pane_focus_indicator};
use crate::styles::{DensityTokens, UiColors};
use crate::widgets::{HoverExt, empty_state, interactive_background, list_row, toolbar_button};

use self::row::{MessageRow, RowAccount, display_subject, row_content, row_shell};

/// The key context the list sets.
pub(crate) const KEY_CONTEXT: &str = "MessageList";

/// Rows loaded beyond the visible range, so small scrolls and arrow keys
/// stay inside the cached page.
const ROW_PAGE_PADDING: usize = 50;

/// The thread list Mail mode shows: one mailbox, or a virtual mailbox
/// across accounts (the Unified Inbox, Flagged, Sent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListMailbox {
    /// Which threads to list; rows are read with
    /// `LocalStore::list_selection_threads`.
    pub(crate) selection: MailboxSelection,
    /// The mailbox's (or virtual mailbox's) name.
    pub(crate) name: String,
    pub(crate) total_threads: usize,
    pub(crate) unread_threads: usize,
}

/// An account as list rows name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountLabel {
    pub(crate) id: i64,
    pub(crate) name: String,
}

/// What the shell shows in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageListState {
    pub(crate) mode: ListMode,
    pub(crate) has_accounts: bool,
    pub(crate) mailbox: Option<ListMailbox>,
    /// Every account, to name the account each row of a cross-account
    /// list comes from (see [`MessageListState::row_account_name`]).
    pub(crate) accounts: Vec<AccountLabel>,
    /// The user's own addresses, shown as "me".
    pub(crate) own_addresses: Vec<String>,
    pub(crate) selected_thread_id: Option<i64>,
    pub(crate) search_results: Vec<SearchHit>,
    /// The query that produced `search_results`.
    pub(crate) search_query: String,
    /// Whether the server search fallback for the query is still running.
    pub(crate) searching_server: bool,
    pub(crate) drafts: Vec<DraftSummary>,
    pub(crate) selected_draft_id: Option<i64>,
    pub(crate) outbox_jobs: Vec<OutboxJobSummary>,
    pub(crate) selected_outbox_job_id: Option<i64>,
    /// Comfortable (three-line) or compact (one-line) rows.
    pub(crate) density: Density,
    /// The multi-row selection (see `selection.rs`).
    pub(crate) selection: ListSelection,
    /// Every account's mailboxes, for Move to….
    pub(crate) mailboxes: Vec<MailboxSummary>,
}

impl MessageListState {
    /// The account a thread row comes from, named only in lists that span
    /// accounts (the Unified Inbox, Flagged, Sent).
    pub(crate) fn row_account_name(&self, account_id: i64) -> Option<&str> {
        let spans_accounts = matches!(
            self.mailbox.as_ref().map(|mailbox| mailbox.selection),
            Some(MailboxSelection::Virtual(_))
        );
        self.accounts
            .iter()
            .find(|account| spans_accounts && account.id == account_id)
            .map(|account| account.name.as_str())
    }
}

impl Default for MessageListState {
    fn default() -> Self {
        Self {
            mode: ListMode::Mail,
            has_accounts: false,
            mailbox: None,
            accounts: Vec::new(),
            own_addresses: Vec::new(),
            selected_thread_id: None,
            search_results: Vec::new(),
            search_query: String::new(),
            searching_server: false,
            drafts: Vec::new(),
            selected_draft_id: None,
            outbox_jobs: Vec::new(),
            selected_outbox_job_id: None,
            density: Density::default(),
            selection: ListSelection::default(),
            mailboxes: Vec::new(),
        }
    }
}

/// What the user did in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MessageListEvent {
    /// A thread or search result was clicked or reached with the arrows.
    OpenThread { thread_id: i64, mailbox_id: i64 },
    /// More than one thread or search result is now selected, with the
    /// cursor on `thread_id`.
    SelectThreads {
        thread_ids: Vec<i64>,
        thread_id: i64,
        mailbox_id: i64,
    },
    /// A draft was clicked or reached with the arrows.
    OpenDraft(i64),
    /// An outbox message was clicked or reached with the arrows.
    SelectOutboxJob(i64),
    /// Enter: move on to the detail pane.
    Activate,
    /// A thread, search result or draft row was clicked (after its open
    /// event). The single-pane layout shows the detail pane on it.
    RowClicked,
    /// Reading the rows failed.
    Error(String),
}

/// The cached page of thread rows.
#[derive(Debug, Clone)]
struct RowPage {
    selection: MailboxSelection,
    start: usize,
    rows: Vec<ThreadSummary>,
}

impl RowPage {
    fn covers(&self, selection: &MailboxSelection, range: &Range<usize>) -> bool {
        self.selection == *selection
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
    /// Where Shift-click and Shift+arrows extend the selection from.
    selection_anchor: Option<usize>,
    /// The open Move to… picker, where the context menu opened it (or
    /// `None` below the bulk bar's button), and its subscriptions.
    move_picker: Option<OpenMovePicker>,
    /// The open row context menu, where it opened, and its subscription.
    context_menu: Option<(Entity<Menu>, Point<Pixels>, Subscription)>,
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
            selection_anchor: None,
            move_picker: None,
            context_menu: None,
            #[cfg(test)]
            counters: Default::default(),
        }
    }

    /// What the list shows now.
    #[cfg(test)]
    pub(crate) fn state(&self) -> &MessageListState {
        &self.state
    }

    /// Replaces what the list shows; re-renders only when it changed.
    pub(crate) fn set_state(&mut self, state: MessageListState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        let mailbox_changed = self.state.mailbox.as_ref().map(|mailbox| mailbox.selection)
            != state.mailbox.as_ref().map(|mailbox| mailbox.selection);
        let counts_changed = self.state.mailbox != state.mailbox;
        // The shell cleared the selection (Escape, or a change took the
        // rows away): Shift extends from the selected row again.
        if self.state.selection.is_multiple() && !state.selection.is_multiple() {
            self.selection_anchor = None;
        }
        self.state = state;
        if mailbox_changed || counts_changed {
            self.page = None;
        }
        if mailbox_changed {
            self.selected_index_hint = None;
            self.selection_anchor = None;
            self.move_picker = None;
            self.context_menu = None;
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
        match self.store.list_selection_threads(&page.selection, range) {
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
        let Some(selection) = self.state.mailbox.as_ref().map(|mailbox| mailbox.selection) else {
            return Vec::new();
        };
        if !self
            .page
            .as_ref()
            .is_some_and(|page| page.covers(&selection, &range))
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
            match self.store.list_selection_threads(&selection, start..end) {
                Ok(rows) => {
                    self.page = Some(RowPage {
                        selection,
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

    /// Row geometry for the chosen density.
    fn density_tokens(&self) -> DensityTokens {
        DensityTokens::for_density(self.state.density)
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

    /// The selected thread's row in the mailbox, so the shell can select
    /// the row that takes its place when it leaves the list.
    pub(crate) fn selected_row_index(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        if self.state.mode != ListMode::Mail {
            return None;
        }
        if self.state.selection.is_multiple() {
            return self
                .first_selected_index()
                .or_else(|| self.current_index(cx));
        }
        self.current_index(cx)
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.row_count() {
            return;
        }
        // Moving without Shift leaves a single selected row.
        self.state.selection.thread_ids.clear();
        self.selection_anchor = Some(index);
        match self.state.mode {
            ListMode::Mail => {
                let Some(thread) = self.thread_rows(index..index + 1, cx).into_iter().next() else {
                    return;
                };
                self.selected_index_hint = Some((thread.id, index));
                self.thread_scroll
                    .scroll_to_item(index, ScrollStrategy::Nearest);
                self.state.selected_thread_id = Some(thread.id);
                cx.emit(MessageListEvent::OpenThread {
                    thread_id: thread.id,
                    mailbox_id: thread.mailbox_id,
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
        let can_retry = self.state.mode == ListMode::Outbox && !self.state.outbox_jobs.is_empty();

        div()
            .id("message-list")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::extend_selection_down))
            .on_action(cx.listener(Self::extend_selection_up))
            .on_action(cx.listener(Self::select_all))
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
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .child(subtitle),
                        ),
                )
                // Retry lives with the outbox it acts on.
                .when(can_retry, |this| {
                    this.child(
                        Button::new("outbox-retry", "Retry")
                            .icon("refresh-cw")
                            .size(ButtonSize::Small)
                            .shortcut("secondary-shift-o")
                            .on_click(dispatch_from(&self.focus_handle, RetryOutbox)),
                    )
                }),
            )
            .children(self.render_bulk_bar(&colors, cx))
            .children(self.render_row_overlays())
            .child(match self.state.mode {
                ListMode::Mail => self.render_threads(&colors, cx),
                ListMode::Search => self.render_search_results(&colors, focused, cx),
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
                "{} for \u{201c}{}\u{201d}{}",
                count_noun(self.state.search_results.len(), "result", "results"),
                self.state.search_query,
                if self.state.searching_server {
                    " \u{2022} searching the server\u{2026}"
                } else {
                    ""
                }
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
                    cx.processor(move |this, range: Range<usize>, window, cx| {
                        let focused = this.focus_handle.contains_focused(window, cx);
                        this.render_thread_rows(range, &colors, focused, cx)
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
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Stateful<gpui::Div>> {
        let start = range.start;
        let threads = self.thread_rows(range, cx);
        let now = chrono::Local::now();
        let density = self.density_tokens();
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
                let row_id = thread_row_id(thread_id);
                let is_selected = self.is_thread_selected(thread_id);
                let mut row = MessageRow::thread(
                    row_id.clone(),
                    &thread,
                    &own_addresses,
                    &now,
                    self.hour_cycle,
                );
                row.account =
                    self.state
                        .row_account_name(thread.account_id)
                        .map(|name| RowAccount {
                            name: name.to_owned(),
                            color: None,
                        });

                row_shell(row_id, is_selected, focused, density, colors)
                    .debug_selector(move || format!("thread-row-{index}"))
                    .role(Role::ListItem)
                    .aria_label(row.accessible_label())
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.click_thread_row(index, event.modifiers(), cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_context_menu(index, event.position, window, cx);
                        }),
                    )
                    .child(row_content(
                        row,
                        None,
                        density,
                        &row_colors(is_selected, focused, colors),
                    ))
            })
            .collect()
    }

    fn render_search_results(
        &self,
        colors: &UiColors,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        let density = self.density_tokens();
        div()
            .id("search-results-list")
            .flex_1()
            .overflow_y_scroll()
            .children(
                self.state
                    .search_results
                    .iter()
                    .enumerate()
                    .map(|(index, hit)| {
                        let thread_id = hit.thread_id;
                        let row_id = search_row_id(thread_id);
                        let is_selected = self.is_thread_selected(thread_id);
                        let row =
                            MessageRow::search_hit(row_id.clone(), hit, &now, self.hour_cycle);

                        row_shell(row_id, is_selected, focused, density, colors)
                            .role(Role::ListItem)
                            .aria_label(row.accessible_label())
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                this.click_thread_row(index, event.modifiers(), cx);
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    this.open_context_menu(index, event.position, window, cx);
                                }),
                            )
                            .child(row_content(
                                row,
                                None,
                                density,
                                &row_colors(is_selected, focused, colors),
                            ))
                    }),
            )
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
                        cx.emit(MessageListEvent::RowClicked);
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
