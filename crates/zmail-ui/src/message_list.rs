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
mod header;
mod quick_actions;
mod row;
mod selection;
mod states;

use std::ops::Range;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, MouseButton, MouseDownEvent, Pixels, Point, Render, Role, ScrollStrategy,
    SharedString, Subscription, UniformListScrollHandle, Window, div, prelude::*, rgb,
    uniform_list,
};
use std::sync::Arc;
use zmail_core::{
    AccountColor, DraftSummary, MailboxRole, MailboxSelection, MailboxSummary, OutboxJobSummary,
    SearchHit, ThreadFilter, ThreadSummary, preferences::Density, time::HourCycle,
};
use zmail_store::LocalStore;

pub(crate) use self::selection::ListSelection;
use self::selection::{OpenMovePicker, row_colors};
use crate::ListMode;
use crate::a11y::{A11yFlags, AccessibleExt};
use crate::actions::{
    AddAccount, OpenSelected, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
use crate::components::{Button, ButtonSize, Menu};
use crate::drag::DraggedThreads;
use crate::mail_actions::MailActions;
use crate::search::{SearchFilter, SearchHighlighter, SearchScopeState};
use crate::styles::{DensityTokens, UiColors};
use crate::widgets::{empty_state, toolbar_button};

use self::row::{MessageRow, RowAccount, outbox_status, row_content, row_shell};

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
    /// A single mailbox's role (Inbox, Sent, …), for its empty state.
    pub(crate) role: Option<MailboxRole>,
    pub(crate) total_threads: usize,
    pub(crate) unread_threads: usize,
}

/// An account as list rows name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountLabel {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) color: AccountColor,
}

/// What the shell shows in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageListState {
    pub(crate) mode: ListMode,
    pub(crate) has_accounts: bool,
    pub(crate) mailbox: Option<ListMailbox>,
    /// Every account, to name the account each row of a cross-account
    /// list comes from (see [`MessageListState::row_account`]).
    pub(crate) accounts: Vec<AccountLabel>,
    /// The user's own addresses, shown as "me".
    pub(crate) own_addresses: Vec<String>,
    pub(crate) selected_thread_id: Option<i64>,
    pub(crate) search_results: Vec<SearchHit>,
    /// The query that produced `search_results`.
    pub(crate) search_query: String,
    /// Whether the store has more results than `search_results` holds:
    /// scrolling near the end asks for the next page.
    pub(crate) search_has_more: bool,
    /// Whether the server search fallback for the query is still running.
    pub(crate) searching_server: bool,
    /// Where the search looks, for the header's scope control.
    pub(crate) search_scope: SearchScopeState,
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
    /// An account the list shows is syncing for the first time, so an
    /// empty list is still filling: it shows skeleton rows.
    pub(crate) first_sync: bool,
}

impl MessageListState {
    /// The account a thread row comes from, shown only in lists that span
    /// accounts (the Unified Inbox, Flagged, Sent).
    pub(crate) fn row_account(&self, account_id: i64) -> Option<&AccountLabel> {
        let spans_accounts = matches!(
            self.mailbox.as_ref().map(|mailbox| mailbox.selection),
            Some(MailboxSelection::Virtual(_))
        );
        self.accounts
            .iter()
            .find(|account| spans_accounts && account.id == account_id)
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
            search_has_more: false,
            searching_server: false,
            search_scope: SearchScopeState::default(),
            drafts: Vec::new(),
            selected_draft_id: None,
            outbox_jobs: Vec::new(),
            selected_outbox_job_id: None,
            density: Density::default(),
            selection: ListSelection::default(),
            mailboxes: Vec::new(),
            first_sync: false,
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
    /// An outbox row's Retry now was clicked.
    RetryOutboxJob(i64),
    /// Enter: move on to the detail pane.
    Activate,
    /// A thread, search result or draft row was clicked (after its open
    /// event). The single-pane layout shows the detail pane on it.
    RowClicked,
    /// A search filter chip was chosen: the shell edits the query.
    FilterSearch(SearchFilter),
    /// The search results are scrolled near their end and the store has
    /// more: the shell reads the next page.
    LoadMoreResults,
    /// Reading the rows failed.
    Error(String),
}

/// The cached page of thread rows.
#[derive(Debug, Clone)]
struct RowPage {
    selection: MailboxSelection,
    filter: ThreadFilter,
    start: usize,
    rows: Vec<ThreadSummary>,
}

impl RowPage {
    fn covers(
        &self,
        selection: &MailboxSelection,
        filter: ThreadFilter,
        range: &Range<usize>,
    ) -> bool {
        self.selection == *selection
            && self.filter == filter
            && range.start >= self.start
            && range.end <= self.start + self.rows.len()
    }
}

pub(crate) struct MessageListView {
    focus_handle: FocusHandle,
    store: Arc<LocalStore>,
    /// Quick actions change mail through the same service (and undo
    /// stack) as every other command.
    mail_actions: Entity<MailActions>,
    hour_cycle: HourCycle,
    state: MessageListState,
    /// The header's filter (All, Unread, Flagged, Has attachments). It
    /// resets to All when another mailbox is shown.
    filter: ThreadFilter,
    /// How many threads the filter leaves, when it is not All.
    filtered_total: Option<usize>,
    /// The open filter menu and the subscription that closes it.
    filter_menu: Option<(Entity<Menu>, Subscription)>,
    /// Set when a press on the filter button closed the open menu, so the
    /// click that follows does not reopen it.
    swallow_filter_click: bool,
    /// How many search results the list had when it last asked for more,
    /// so it asks once per page.
    asked_for_more_at: Option<usize>,
    /// The open search date menu, like `filter_menu`.
    date_menu: Option<(Entity<Menu>, Subscription)>,
    /// Like `swallow_filter_click`, for the date chip.
    swallow_date_click: bool,
    /// The thread row under the pointer: it shows quick actions in place
    /// of its date.
    hovered_thread: Option<i64>,
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
        mail_actions: Entity<MailActions>,
        hour_cycle: HourCycle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            store,
            mail_actions,
            hour_cycle,
            state: MessageListState::default(),
            filter: ThreadFilter::All,
            filtered_total: None,
            filter_menu: None,
            swallow_filter_click: false,
            asked_for_more_at: None,
            date_menu: None,
            swallow_date_click: false,
            hovered_thread: None,
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

    /// The thread rows' scroll handle (the performance runs scroll it).
    #[cfg(test)]
    pub(crate) fn thread_scroll(&self) -> &UniformListScrollHandle {
        &self.thread_scroll
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
        // Opening a search result from another account moves the shell to
        // that account's mailbox, but the list still shows the same
        // results: its menus and scroll position stay.
        let rows_changed = mailbox_changed
            && !(self.state.mode == ListMode::Search && state.mode == ListMode::Search);
        // The shell cleared the selection (Escape, or a change took the
        // rows away): Shift extends from the selected row again.
        if self.state.selection.is_multiple() && !state.selection.is_multiple() {
            self.selection_anchor = None;
        }
        // Another search's results start at the top; the next page of the
        // same search keeps the scroll position.
        let new_search = state.mode == ListMode::Search
            && (self.state.mode != ListMode::Search
                || self.state.search_query != state.search_query);
        if new_search {
            self.asked_for_more_at = None;
            self.thread_scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        if state.mode != ListMode::Search {
            self.date_menu = None;
        }
        self.state = state;
        if mailbox_changed {
            self.filter = ThreadFilter::All;
            self.filter_menu = None;
        }
        if mailbox_changed || counts_changed {
            self.page = None;
            self.count_filtered(cx);
        }
        if rows_changed {
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
        // A change can move a thread in or out of the filter without
        // changing the mailbox's counts.
        if self.filter != ThreadFilter::All {
            let before = self.filtered_total;
            self.count_filtered(cx);
            if self.filtered_total != before {
                cx.notify();
            }
        }
        let Some(page) = self.page.as_ref() else {
            return;
        };
        let range = page.start..page.start + page.rows.len().max(1);
        match self
            .store
            .list_filtered_threads(&page.selection, page.filter, range)
        {
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
            .is_some_and(|page| page.covers(&selection, self.filter, &range))
        {
            let total = self.thread_count();
            let start = range.start.saturating_sub(ROW_PAGE_PADDING);
            let end = (range.end + ROW_PAGE_PADDING).min(total.max(range.end));
            #[cfg(test)]
            {
                self.counters.queries += 1;
            }
            match self
                .store
                .list_filtered_threads(&selection, self.filter, start..end)
            {
                Ok(rows) => {
                    self.page = Some(RowPage {
                        selection,
                        filter: self.filter,
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
            ListMode::Mail => self.thread_count(),
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
                self.thread_scroll
                    .scroll_to_item(index, ScrollStrategy::Nearest);
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
        self.select_adjacent(true, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.select_adjacent(false, cx);
    }

    /// Selects and opens the next (or previous) row, as Down and Up do.
    /// The workspace calls it for the Gmail keymap's j and k in the
    /// reader, which move through the list behind the conversation.
    pub(crate) fn select_adjacent(&mut self, forward: bool, cx: &mut Context<Self>) {
        let current = self.current_index(cx);
        let index = match (current, forward) {
            (Some(index), true) => index + 1,
            (Some(index), false) => index.saturating_sub(1),
            (None, _) => 0,
        };
        self.select_index(index, cx);
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
        div()
            .id("message-list")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .accessible(Role::Pane, "Message list")
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
            .child(self.render_header(focused, &colors, cx))
            .children(self.render_bulk_bar(&colors, cx))
            .children(self.render_row_overlays())
            .child(match self.state.mode {
                ListMode::Mail => self.render_threads(&colors, cx),
                ListMode::Search => self.render_search_results(&colors, cx),
                ListMode::Drafts => self.render_drafts(&colors, focused, cx),
                ListMode::Outbox => self.render_outbox(&colors, focused, cx),
            })
    }
}

impl MessageListView {
    fn render_threads(&mut self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let colors = *colors;
        let thread_count = self.thread_count();
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
        if thread_count == 0 && self.filter != ThreadFilter::All {
            return self.render_filtered_empty(&colors, cx);
        }
        if thread_count == 0 && self.state.first_sync {
            return self.render_skeleton();
        }
        if thread_count == 0 {
            return self.render_empty_mailbox(&colors);
        }

        let (title, _) = self.header_text();
        div()
            .id("thread-list")
            .accessible(Role::ListBox, title)
            .a11y_flags(A11yFlags {
                multiselectable: true,
                ..A11yFlags::default()
            })
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
        let count = self.thread_count();
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
                let dragged = self.dragged_threads(thread.id, thread.account_id, thread.mailbox_id);
                let mut row = MessageRow::thread(
                    row_id.clone(),
                    &thread,
                    &own_addresses,
                    &now,
                    self.hour_cycle,
                );
                row.account = self
                    .state
                    .row_account(thread.account_id)
                    .map(|account| RowAccount {
                        id: account.id,
                        name: account.name.clone(),
                        color: account.color,
                    });

                // The hovered row shows quick actions for that row; the
                // selection hooks act on the selection.
                let quick_actions = (self.hovered_thread == Some(thread_id))
                    .then(|| self.quick_actions(thread_id, &row, cx));

                let cursor = focused && self.state.selected_thread_id == Some(thread_id);
                row_shell(row_id, is_selected, focused, density, colors)
                    .debug_selector(move || format!("thread-row-{index}"))
                    .map(|this| row.accessible(this, is_selected, cursor, index, count))
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        this.set_hovered_thread(thread_id, *hovered, cx);
                    }))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.click_thread_row(index, event.modifiers(), cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_context_menu(index, event.position, window, cx);
                        }),
                    )
                    .on_drag(dragged, |dragged: &DraggedThreads, _, _, cx| {
                        cx.new(|_| dragged.clone())
                    })
                    .child(row_content(
                        row,
                        quick_actions,
                        density,
                        &row_colors(is_selected, focused, colors),
                    ))
            })
            .collect()
    }

    fn render_search_results(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        if self.state.search_results.is_empty() {
            return self.render_no_results(colors);
        }
        let colors = *colors;
        // Rows are paged like a mailbox's: the list draws only what is in
        // view, and asks for the next page as the end comes near.
        div()
            .id("search-results-list")
            .accessible(Role::ListBox, "Search results")
            .a11y_flags(A11yFlags {
                multiselectable: true,
                ..A11yFlags::default()
            })
            .flex_1()
            .overflow_hidden()
            .child(
                uniform_list(
                    "search-results",
                    self.state.search_results.len(),
                    cx.processor(move |this, range: Range<usize>, window, cx| {
                        let focused = this.focus_handle.contains_focused(window, cx);
                        this.render_search_rows(range, &colors, focused, cx)
                    }),
                )
                .track_scroll(&self.thread_scroll)
                .h_full(),
            )
            .into_any_element()
    }

    fn render_search_rows(
        &mut self,
        range: Range<usize>,
        colors: &UiColors,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Stateful<gpui::Div>> {
        let loaded = self.state.search_results.len();
        if self.state.search_has_more
            && range.end + ROW_PAGE_PADDING >= loaded
            && self.asked_for_more_at != Some(loaded)
        {
            self.asked_for_more_at = Some(loaded);
            cx.emit(MessageListEvent::LoadMoreResults);
        }
        let now = chrono::Local::now();
        let density = self.density_tokens();
        let highlighter = SearchHighlighter::new(&self.state.search_query);
        // Results from several accounts name their account, as rows of
        // the Unified Inbox do.
        let first_account = self.state.search_results.first().map(|hit| hit.account_id);
        let spans_accounts = self
            .state
            .search_results
            .iter()
            .any(|hit| Some(hit.account_id) != first_account);
        let start = range.start;
        let end = range.end.min(loaded);
        self.state.search_results[start.min(end)..end]
            .iter()
            .enumerate()
            .map(|(offset, hit)| {
                let index = start + offset;
                let thread_id = hit.thread_id;
                let row_id = search_row_id(thread_id);
                let is_selected = self.is_thread_selected(thread_id);
                let mut row = MessageRow::search_hit(
                    row_id.clone(),
                    hit,
                    &highlighter,
                    &now,
                    self.hour_cycle,
                );
                if spans_accounts {
                    row.account = self
                        .state
                        .accounts
                        .iter()
                        .find(|account| account.id == hit.account_id)
                        .map(|account| RowAccount {
                            id: account.id,
                            name: account.name.clone(),
                            color: account.color,
                        });
                }
                let quick_actions = (self.hovered_thread == Some(thread_id))
                    .then(|| self.quick_actions(thread_id, &row, cx));
                // A hit says where it is, so it can be dragged to one of
                // its account's folders.
                let dragged = self.dragged_threads(thread_id, hit.account_id, hit.mailbox_id);

                let cursor = focused && self.state.selected_thread_id == Some(thread_id);
                row_shell(row_id, is_selected, focused, density, colors)
                    .debug_selector(move || format!("search-row-{thread_id}"))
                    .map(|this| row.accessible(this, is_selected, cursor, index, loaded))
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        this.set_hovered_thread(thread_id, *hovered, cx);
                    }))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.click_thread_row(index, event.modifiers(), cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_context_menu(index, event.position, window, cx);
                        }),
                    )
                    .on_drag(dragged, |dragged: &DraggedThreads, _, _, cx| {
                        cx.new(|_| dragged.clone())
                    })
                    .child(row_content(
                        row,
                        quick_actions,
                        density,
                        &row_colors(is_selected, focused, colors),
                    ))
            })
            .collect()
    }

    fn render_drafts(
        &self,
        colors: &UiColors,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.state.drafts.is_empty() {
            return self.render_no_drafts(colors);
        }
        let now = chrono::Local::now();
        let density = self.density_tokens();
        let count = self.state.drafts.len();
        div()
            .id("drafts-list")
            .accessible(Role::ListBox, "Drafts")
            .flex_1()
            .overflow_y_scroll()
            .children(self.state.drafts.iter().enumerate().map(|(index, draft)| {
                let draft_id = draft.id;
                let row_id = draft_row_id(draft_id);
                let is_selected = Some(draft_id) == self.state.selected_draft_id;
                let row = MessageRow::draft(row_id.clone(), draft, &now, self.hour_cycle);

                row_shell(row_id, is_selected, focused, density, colors)
                    .map(|this| {
                        row.accessible(this, is_selected, focused && is_selected, index, count)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.selected_draft_id = Some(draft_id);
                        cx.emit(MessageListEvent::OpenDraft(draft_id));
                        cx.emit(MessageListEvent::RowClicked);
                        cx.notify();
                    }))
                    .child(row_content(
                        row,
                        None,
                        density,
                        &row_colors(is_selected, focused, colors),
                    ))
            }))
            .into_any_element()
    }

    fn render_outbox(
        &self,
        colors: &UiColors,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.state.outbox_jobs.is_empty() {
            return self.render_empty_outbox(colors);
        }
        let now = chrono::Local::now();
        let density = self.density_tokens();
        let count = self.state.outbox_jobs.len();
        div()
            .id("outbox-list")
            .accessible(Role::ListBox, "Outbox")
            .flex_1()
            .overflow_y_scroll()
            .children(
                self.state
                    .outbox_jobs
                    .iter()
                    .enumerate()
                    .map(|(index, job)| {
                        let job_id = job.id;
                        let row_id = outbox_row_id(job_id);
                        let is_selected = Some(job_id) == self.state.selected_outbox_job_id;
                        let draft = self
                            .state
                            .drafts
                            .iter()
                            .find(|draft| draft.id == job.draft_id);
                        let row = MessageRow::outbox_job(
                            row_id.clone(),
                            job,
                            draft,
                            &now,
                            self.hour_cycle,
                        );
                        // A message waiting for a retry, or one that gave up,
                        // offers Retry now where the date goes.
                        let retry = outbox_status(job, &now)
                            .can_retry
                            .then(|| self.retry_now_button(job_id, &row_id, cx));

                        row_shell(row_id, is_selected, focused, density, colors)
                            .map(|this| {
                                row.accessible(
                                    this,
                                    is_selected,
                                    focused && is_selected,
                                    index,
                                    count,
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.selected_outbox_job_id = Some(job_id);
                                cx.emit(MessageListEvent::SelectOutboxJob(job_id));
                                cx.notify();
                            }))
                            .child(row_content(
                                row,
                                retry,
                                density,
                                &row_colors(is_selected, focused, colors),
                            ))
                    }),
            )
            .into_any_element()
    }

    /// The "Retry now" button on an outbox row.
    fn retry_now_button(
        &self,
        job_id: i64,
        row_id: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = format!("{row_id}-retry");
        div()
            .debug_selector({
                let id = id.clone();
                move || id
            })
            .child(
                Button::new(SharedString::from(id), "Retry now")
                    .size(ButtonSize::Small)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        // Not the row's click as well.
                        cx.stop_propagation();
                        cx.emit(MessageListEvent::RetryOutboxJob(job_id));
                    })),
            )
            .into_any_element()
    }
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
