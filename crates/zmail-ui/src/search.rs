//! Running searches in a scope: the folder the list shows (the default),
//! its account, or every account (UI plan Phase 3 item 5, Phase 7).
//!
//! Search is live: each edit of the search field waits [`SEARCH_DEBOUNCE`]
//! for the next one, then reads the local index on a background thread,
//! so typing never waits for the store. Every search bumps
//! [`SearchContext::generation`], and results only show while their
//! generation is current, so a slow read for an earlier keystroke never
//! replaces newer results. Clearing the field, Escape, or showing a
//! mailbox ends the search and empties the field.
//!
//! A search session starts in the folder the list shows and can widen
//! from the list header's scope control, its empty state or the Edit
//! menu. The folder and account are taken when the session starts, so
//! refining the query keeps the scope, and opening a result from another
//! account does not change what is searched. An `in:` clause in the query
//! names the folder itself and takes precedence over the folder scope;
//! the scope control shows it.
//!
//! The local store answers first. When a query needs message bodies that
//! are not indexed yet, the server fallback runs for each account in scope
//! (only the folder's mailboxes in folder scope), and the results are
//! re-read from the store as each finishes. One account's failure leaves
//! the others' results in place. The fallback never runs per keystroke:
//! only once typing has settled for [`SERVER_SEARCH_SETTLE`], or at once
//! on Enter, Down or a scope change. A server hit for mail newer than the
//! last sync still syncs the account at most once per
//! `zmail_sync::SEARCH_SYNC_COOLDOWN`.

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use gpui::{AppContext as _, Context, Task, Window};
use zmail_core::{MailboxSelection, SearchHit, SearchScope, search::SearchQuery};
use zmail_store::LocalStore;

use super::{DetailMode, ListMode, WorkspaceView};
use crate::a11y::Politeness;
use crate::actions::{SearchAllAccounts, SearchInAccount, SearchInFolder};
use crate::pane::Pane;

mod filters;
mod highlight;

pub(crate) use filters::{DateRange, SearchFilter, SearchFilters};
pub(crate) use highlight::{RowMatches, SearchHighlighter};

/// Results a search reads at a time. Scrolling near the end of the list
/// reads the next page.
const SEARCH_PAGE: usize = 100;

/// How long typing must pause before the local search runs.
pub(crate) const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

/// How long typing must pause before the servers are asked, counted from
/// the last edit. Long enough that a word typed at a normal pace asks
/// once, at its end.
pub(crate) const SERVER_SEARCH_SETTLE: Duration = Duration::from_millis(1000);

/// How far a search reaches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SearchLevel {
    /// The folder the list showed when the search started.
    #[default]
    Folder,
    /// Every mailbox of that folder's account.
    Account,
    /// Every account.
    AllAccounts,
}

/// The scope of the search the list shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SearchContext {
    pub(crate) level: SearchLevel,
    /// The folder the search started in, and its name.
    pub(crate) folder: Option<(MailboxSelection, String)>,
    /// The account the search started in, and its name.
    pub(crate) account: Option<(i64, String)>,
    /// Bumped by every search, scope change and clear, so a late local or
    /// server result for an earlier one is ignored.
    pub(crate) generation: u64,
    /// Server searches still running for this one.
    pub(crate) servers_pending: usize,
    /// Whether the servers were asked for this generation already.
    pub(crate) servers_asked: bool,
    /// How many results the search reads: one page, plus one more for
    /// each time the list scrolled near the end.
    pub(crate) limit: usize,
    /// Whether the last read filled `limit`, so there may be more.
    pub(crate) has_more: bool,
}

/// What the list shows about the search scope (see the list header).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SearchScopeState {
    pub(crate) level: SearchLevel,
    /// The folder's name, when the search started in one.
    pub(crate) folder: Option<String>,
    /// Whether that folder spans accounts (the Unified Inbox, Flagged).
    pub(crate) folder_spans_accounts: bool,
    /// The account's name.
    pub(crate) account: Option<String>,
    /// Whether there is more than one account to widen to.
    pub(crate) many_accounts: bool,
    /// The query's `in:` value, which chooses the folder instead.
    pub(crate) in_clause: Option<String>,
}

impl SearchContext {
    /// The store scope for the search's level. Without a folder (no
    /// mailbox was shown) the folder level searches the account.
    fn scope(&self) -> Option<SearchScope> {
        match (self.level, &self.folder, &self.account) {
            (SearchLevel::Folder, Some((folder, _)), _) => Some(SearchScope::Folder(*folder)),
            (SearchLevel::Folder | SearchLevel::Account, _, Some((account_id, _))) => {
                Some(SearchScope::Account(*account_id))
            }
            (SearchLevel::AllAccounts, _, _) => Some(SearchScope::AllAccounts),
            _ => None,
        }
    }

    /// The folder the server fallback is scoped to, if any.
    fn server_folder(&self) -> Option<MailboxSelection> {
        match (self.level, &self.folder) {
            (SearchLevel::Folder, Some((folder, _))) => Some(*folder),
            _ => None,
        }
    }

    /// Starts a new generation: anything still running for the last one
    /// is ignored when it finishes.
    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.servers_pending = 0;
        self.servers_asked = false;
        self.limit = SEARCH_PAGE;
        self.has_more = false;
        self.generation
    }
}

/// When a search asks the servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerSearch {
    /// Straight away (Enter, Down, a scope change).
    Now,
    /// Not yet: typing may go on ([`SERVER_SEARCH_SETTLE`]).
    Later,
}

/// Where a search reads the local index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// On a background thread; the results show when it finishes.
    Background,
    /// Here and now, for a search the next step depends on (Down into
    /// the results, the window opening on a search).
    Now,
}

impl WorkspaceView {
    /// The search field changed: search again once typing pauses, or end
    /// the search when the field is empty.
    pub(crate) fn search_edited(&mut self, cx: &mut Context<Self>) {
        if self.search_query.read(cx).text().trim().is_empty() {
            self.end_search(cx);
            return;
        }
        // Replacing the task cancels the one for the previous edit.
        self.search_typing = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let Ok(Some(generation)) = this.update(cx, |view, cx| {
                view.start_search(ServerSearch::Later, Read::Background, cx)
            }) else {
                return;
            };
            cx.background_executor()
                .timer(SERVER_SEARCH_SETTLE.saturating_sub(SEARCH_DEBOUNCE))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.search_context.generation == generation {
                    view.search_servers_in_scope(cx);
                }
            });
        });
    }

    /// Enter in the search field: search now, servers included, without
    /// waiting for the debounce.
    pub(crate) fn submit_search(&mut self, cx: &mut Context<Self>) {
        self.search_now(Read::Background, cx);
    }

    /// Runs the search the window opens on, with its results in place
    /// before the first frame.
    pub(crate) fn search_on_open(&mut self, cx: &mut Context<Self>) {
        self.search_now(Read::Now, cx);
    }

    /// Searches for the field's text straight away, servers included. A
    /// search already showing for the same text only asks the servers if
    /// it has not yet.
    fn search_now(&mut self, read: Read, cx: &mut Context<Self>) {
        self.search_typing = Task::ready(());
        let query = self.search_query.read(cx).text().trim().to_owned();
        if query.is_empty() {
            self.end_search(cx);
            return;
        }
        // The list shows this query's results, and no read for another
        // query is still running.
        let showing = matches!(self.list_mode, ListMode::Search)
            && self.search_results_query == query
            && self.search_pending_query == query;
        if showing {
            self.search_servers_in_scope(cx);
        } else {
            self.start_search(ServerSearch::Now, read, cx);
        }
    }

    /// Runs the query in the search field. A new search starts in the
    /// folder the list shows; refining the query of the search the list
    /// shows keeps its scope. Returns the search's generation.
    fn start_search(
        &mut self,
        servers: ServerSearch,
        read: Read,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let Some(account_id) = self.selected_account_id else {
            self.show_error("Select or add an account before searching.", cx);
            return None;
        };
        let query = self.search_query.read(cx).text().trim().to_owned();
        if query.is_empty() {
            self.end_search(cx);
            return None;
        }
        if !matches!(self.list_mode, ListMode::Search) {
            let account_name = self
                .accounts
                .iter()
                .find(|account| account.id == account_id)
                .map(|account| account.display_name.clone())
                .unwrap_or_default();
            let folder = match (self.mailbox_selection(), self.list_mailbox()) {
                (Some(selection), Some(mailbox)) => Some((selection, mailbox.name)),
                _ => None,
            };
            self.search_context = SearchContext {
                level: SearchLevel::Folder,
                folder,
                account: Some((account_id, account_name)),
                generation: self.search_context.generation,
                ..SearchContext::default()
            };
        }
        let generation = self.search_context.next_generation();
        self.search_pending_query = query;
        self.search_in_scope(servers, read, cx);
        Some(generation)
    }

    /// Runs the field's query straight away and moves focus into the
    /// results, selecting the first: Down in the search field.
    pub(crate) fn search_into_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_now(Read::Now, cx);
        self.focus_pane(Pane::MessageList, window, cx);
        if matches!(self.list_mode, ListMode::Search)
            && let Some(hit) = self.search_results.first()
        {
            let (thread_id, mailbox_id) = (hit.thread_id, hit.mailbox_id);
            self.select_thread(thread_id, mailbox_id, cx);
        }
    }

    /// A filter chip edits the query in the search field and runs it. A
    /// chip that leaves nothing to search yet (From, which waits for the
    /// sender) hands the field to the user instead.
    pub(crate) fn apply_search_filter(
        &mut self,
        filter: SearchFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.search_query.read(cx).text().to_owned();
        let edited = filters::apply_filter(&current, filter);
        self.search_query
            .update(cx, |input, cx| input.set_text(edited.clone(), cx));
        if SearchQuery::parse(&edited) == SearchQuery::parse(&current) {
            self.search_query
                .update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        self.search_now(Read::Background, cx);
    }

    /// Ends the search and goes back to the mailbox it started in.
    pub(crate) fn end_search(&mut self, cx: &mut Context<Self>) {
        if matches!(self.list_mode, ListMode::Search) {
            // Leaving search mode resets the search.
            self.set_list_mode(ListMode::Mail, cx);
        } else {
            self.reset_search(cx);
        }
    }

    /// Drops the search's results and anything still running for it, and
    /// empties the search field. Runs whenever the list leaves search
    /// mode, so the field always shows what the list does.
    pub(crate) fn reset_search(&mut self, cx: &mut Context<Self>) {
        self.search_typing = Task::ready(());
        self.search_read = Task::ready(());
        self.search_context.next_generation();
        self.search_results.clear();
        self.search_results_query.clear();
        self.search_pending_query.clear();
        if !self.search_query.read(cx).text().is_empty() {
            self.search_query
                .update(cx, |input, cx| input.set_text("", cx));
        }
        cx.notify();
    }

    /// Re-runs the shown search at `level`, servers included.
    pub(crate) fn set_search_level(&mut self, level: SearchLevel, cx: &mut Context<Self>) {
        if !matches!(self.list_mode, ListMode::Search)
            || self.search_results_query.is_empty()
            || self.search_context.level == level
        {
            return;
        }
        self.search_typing = Task::ready(());
        self.search_context.level = level;
        self.search_context.next_generation();
        self.search_pending_query = self.search_results_query.clone();
        self.search_in_scope(ServerSearch::Now, Read::Background, cx);
    }

    /// What the list header and empty state show about the scope.
    pub(crate) fn search_scope_state(&self) -> SearchScopeState {
        let context = &self.search_context;
        SearchScopeState {
            level: context.level,
            folder: context.folder.as_ref().map(|(_, name)| name.clone()),
            folder_spans_accounts: matches!(
                context.folder,
                Some((MailboxSelection::Virtual(_), _))
            ),
            account: context.account.as_ref().map(|(_, name)| name.clone()),
            many_accounts: self.accounts.len() > 1,
            in_clause: SearchQuery::parse(&self.search_results_query)
                .mailbox_clause()
                .map(str::to_owned),
        }
    }

    /// Re-reads the shown search's local results, so read state, flags
    /// and moves made since it ran show up.
    pub(crate) fn refresh_search_results(&mut self, cx: &mut Context<Self>) {
        if matches!(self.list_mode, ListMode::Search) && !self.search_pending_query.is_empty() {
            self.read_search_results(Read::Background, cx);
        }
    }

    /// Reads the next page of the shown search's results.
    pub(crate) fn load_more_search_results(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.list_mode, ListMode::Search) || !self.search_context.has_more {
            return;
        }
        self.search_context.has_more = false;
        self.search_context.limit += SEARCH_PAGE;
        self.read_search_results(Read::Background, cx);
    }

    /// Searches the store in the current scope for the pending query, then
    /// asks the servers in scope when the local index cannot fully answer.
    fn search_in_scope(&mut self, servers: ServerSearch, read: Read, cx: &mut Context<Self>) {
        self.read_search_results(read, cx);
        if servers == ServerSearch::Now {
            self.search_servers_in_scope(cx);
        }
    }

    /// Reads the pending query's results in the current scope, up to the
    /// search's limit. A later read replaces (and so cancels) one still
    /// running; the generation check covers reads that already finished.
    fn read_search_results(&mut self, read: Read, cx: &mut Context<Self>) {
        let Some(scope) = self.search_context.scope() else {
            return;
        };
        let generation = self.search_context.generation;
        let query = self.search_pending_query.clone();
        let limit = self.search_context.limit.max(SEARCH_PAGE);
        match read {
            Read::Now => {
                let results = self
                    .store
                    .search_threads_in(&scope, &query, limit)
                    .map(|hits| with_body_excerpts(&self.store, hits, &query));
                self.search_read = Task::ready(());
                self.show_search_results(generation, query, results, cx);
            }
            Read::Background => {
                let store = self.store.clone();
                self.search_read = cx.spawn(async move |this, cx| {
                    let (query, results) = cx
                        .background_spawn(async move {
                            let results = store
                                .search_threads_in(&scope, &query, limit)
                                .map(|hits| with_body_excerpts(&store, hits, &query));
                            (query, results)
                        })
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        view.show_search_results(generation, query, results, cx);
                    });
                });
            }
        }
    }

    /// Shows a local read's results, unless a newer search has started
    /// since it began.
    pub(crate) fn show_search_results(
        &mut self,
        generation: u64,
        query: String,
        results: Result<Vec<SearchHit>>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.search_context.generation {
            return;
        }
        match results {
            Ok(results) => {
                self.search_context.has_more =
                    results.len() >= self.search_context.limit.max(SEARCH_PAGE);
                // A new query's count is read out; more pages of the same
                // one are not.
                if query != self.search_results_query {
                    self.announcer.announce(
                        result_count_announcement(results.len(), self.search_context.has_more),
                        Politeness::Polite,
                    );
                }
                self.search_results = results;
                self.search_results_query = query;
                if !matches!(self.list_mode, ListMode::Search) {
                    self.set_list_mode(ListMode::Search, cx);
                    self.detail_mode = DetailMode::Reader;
                }
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Asks the servers in scope about the search, once per generation.
    fn search_servers_in_scope(&mut self, cx: &mut Context<Self>) {
        if self.search_context.servers_asked || self.search_pending_query.is_empty() {
            return;
        }
        let Some(scope) = self.search_context.scope() else {
            return;
        };
        self.search_context.servers_asked = true;
        let query = self.search_pending_query.clone();
        self.search_servers(&scope, query, cx);
    }

    /// Runs the server fallback for every account in `scope` that has
    /// unindexed mail the query could match. Each account's search is
    /// independent: a failure (offline, bad credentials) only leaves that
    /// account's local results.
    fn search_servers(&mut self, scope: &SearchScope, query: String, cx: &mut Context<Self>) {
        let folder = self.search_context.server_folder();
        let accounts: Vec<i64> = match scope {
            SearchScope::Account(account_id) => vec![*account_id],
            SearchScope::Folder(MailboxSelection::Mailbox { .. }) => self
                .search_context
                .account
                .iter()
                .map(|(id, _)| *id)
                .collect(),
            SearchScope::Folder(MailboxSelection::Virtual(_)) | SearchScope::AllAccounts => {
                self.accounts.iter().map(|account| account.id).collect()
            }
        };
        let generation = self.search_context.generation;
        for account_id in accounts {
            let needed = self
                .store
                .search_needs_server_in(account_id, folder.as_ref(), &query)
                .unwrap_or(false);
            if !needed {
                continue;
            }
            self.search_context.servers_pending += 1;
            let service = self.sync_service.clone();
            let query = query.clone();
            cx.spawn(async move |this, cx| {
                // Offline or failing servers leave the local results as
                // they are.
                let _ = service
                    .search_server_in_and_wait(account_id, folder, &query)
                    .await;
                let _ = this.update(cx, |view, cx| {
                    if view.search_context.generation != generation {
                        return;
                    }
                    view.search_context.servers_pending =
                        view.search_context.servers_pending.saturating_sub(1);
                    // Re-reads the results too.
                    view.refresh_from_store(cx);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn search_in_folder(
        &mut self,
        _: &SearchInFolder,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_search_level(SearchLevel::Folder, cx);
    }

    pub(crate) fn search_in_account(
        &mut self,
        _: &SearchInAccount,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_search_level(SearchLevel::Account, cx);
    }

    pub(crate) fn search_all_accounts(
        &mut self,
        _: &SearchAllAccounts,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.accounts.len() > 1 {
            self.set_search_level(SearchLevel::AllAccounts, cx);
        }
    }
}

/// `hits` with each snippet that does not show why the hit matched
/// replaced by an excerpt of the thread's body around the match, when the
/// body is stored. Free text matches in bodies the stored snippet (the
/// start of the latest message) often leaves out.
fn with_body_excerpts(store: &LocalStore, mut hits: Vec<SearchHit>, query: &str) -> Vec<SearchHit> {
    let highlighter = SearchHighlighter::new(query);
    if !highlighter.marks_text() {
        return hits;
    }
    let unexplained = hits
        .iter()
        .filter(|hit| highlighter.text(&hit.preview).is_empty())
        .map(|hit| hit.thread_id)
        .collect::<Vec<_>>();
    if unexplained.is_empty() {
        return hits;
    }
    // A failed read leaves the stored snippets.
    let Ok(bodies) = store.thread_body_texts(&unexplained) else {
        return hits;
    };
    let mut excerpts = HashMap::new();
    for (thread_id, body) in &bodies {
        if !excerpts.contains_key(thread_id)
            && let Some(excerpt) = highlighter.body_excerpt(body)
        {
            excerpts.insert(*thread_id, excerpt);
        }
    }
    for hit in &mut hits {
        if let Some(excerpt) = excerpts.remove(&hit.thread_id) {
            hit.preview = excerpt;
        }
    }
    hits
}

/// What a screen reader hears when a search's results arrive. With more
/// to load, `count` is only the first page.
pub(crate) fn result_count_announcement(count: usize, has_more: bool) -> String {
    match (count, has_more) {
        (0, _) => "No results".to_owned(),
        (count, true) => format!("At least {count} results"),
        (1, false) => "1 result".to_owned(),
        (count, false) => format!("{count} results"),
    }
}

#[cfg(test)]
mod tests {
    use zmail_core::{MailboxRole, VirtualMailbox};

    use super::*;

    #[test]
    fn result_counts_read_in_words() {
        assert_eq!(result_count_announcement(0, false), "No results");
        assert_eq!(result_count_announcement(1, false), "1 result");
        assert_eq!(result_count_announcement(7, false), "7 results");
        assert_eq!(result_count_announcement(50, true), "At least 50 results");
    }

    #[test]
    fn levels_map_to_store_scopes() {
        let inbox = MailboxSelection::Mailbox {
            account_id: 1,
            mailbox_id: 5,
        };
        let mut context = SearchContext {
            folder: Some((inbox, "Inbox".to_owned())),
            account: Some((1, "Work".to_owned())),
            ..SearchContext::default()
        };
        assert_eq!(context.scope(), Some(SearchScope::Folder(inbox)));
        assert_eq!(context.server_folder(), Some(inbox));
        context.level = SearchLevel::Account;
        assert_eq!(context.scope(), Some(SearchScope::Account(1)));
        assert_eq!(context.server_folder(), None);
        context.level = SearchLevel::AllAccounts;
        assert_eq!(context.scope(), Some(SearchScope::AllAccounts));

        // The Unified Inbox is a folder too.
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        context.level = SearchLevel::Folder;
        context.folder = Some((unified, "Unified Inbox".to_owned()));
        assert_eq!(context.scope(), Some(SearchScope::Folder(unified)));

        // Without a folder, the folder level searches the account.
        context.folder = None;
        assert_eq!(context.scope(), Some(SearchScope::Account(1)));
    }
}
