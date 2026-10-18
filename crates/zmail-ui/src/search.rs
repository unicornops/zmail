//! Running searches in a scope: the folder the list shows (the default),
//! its account, or every account (UI plan Phase 3 item 5, Phase 7).
//!
//! Each search typed into the search field starts in the folder and can
//! widen from the list header's scope control, its empty state or the
//! Edit menu. The folder and account are taken when the search starts, so
//! opening a result from another account does not change what is
//! searched. An `in:` clause in the query names the folder itself and
//! takes precedence over the folder scope; the scope control shows it.
//!
//! The local store answers first. When a query needs message bodies that
//! are not indexed yet, the server fallback runs for each account in scope
//! (only the folder's mailboxes in folder scope), and the results are
//! re-read from the store as each finishes. One account's failure leaves
//! the others' results in place.

use gpui::{Context, Window};
use zmail_core::{MailboxSelection, SearchScope, search::SearchQuery};

use super::{DetailMode, ListMode, WorkspaceView};
use crate::actions::{SearchAllAccounts, SearchInAccount, SearchInFolder};

/// Most results a search lists.
const SEARCH_LIMIT: usize = 100;

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
    /// Bumped by every search, so a late server result for an earlier one
    /// is ignored.
    pub(crate) generation: u64,
    /// Server searches still running for this one.
    pub(crate) servers_pending: usize,
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
}

impl WorkspaceView {
    /// Runs the query in the search field, starting in the folder the
    /// list shows.
    pub(crate) fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(account_id) = self.selected_account_id else {
            self.show_error("Select or add an account before searching.", cx);
            return;
        };
        let query = self.search_query.read(cx).text().trim().to_owned();
        if query.is_empty() {
            self.search_results.clear();
            self.list_mode = ListMode::Mail;
            cx.notify();
            return;
        }
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
            generation: self.search_context.generation.wrapping_add(1),
            servers_pending: 0,
        };
        self.search_results_query = query;
        self.search_in_scope(cx);
    }

    /// Re-runs the shown search at `level`.
    pub(crate) fn set_search_level(&mut self, level: SearchLevel, cx: &mut Context<Self>) {
        if !matches!(self.list_mode, ListMode::Search)
            || self.search_results_query.is_empty()
            || self.search_context.level == level
        {
            return;
        }
        self.search_context.level = level;
        self.search_context.generation = self.search_context.generation.wrapping_add(1);
        self.search_context.servers_pending = 0;
        self.search_in_scope(cx);
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
    pub(crate) fn refresh_search_results(&mut self) {
        if !matches!(self.list_mode, ListMode::Search) || self.search_results_query.is_empty() {
            return;
        }
        if let Some(scope) = self.search_context.scope()
            && let Ok(results) =
                self.store
                    .search_threads_in(&scope, &self.search_results_query, SEARCH_LIMIT)
        {
            self.search_results = results;
        }
    }

    /// Searches the store in the current scope, then asks the servers in
    /// scope when the local index cannot fully answer.
    fn search_in_scope(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.search_context.scope() else {
            return;
        };
        let query = self.search_results_query.clone();
        match self.store.search_threads_in(&scope, &query, SEARCH_LIMIT) {
            Ok(results) => {
                self.search_results = results;
                self.list_mode = ListMode::Search;
                self.detail_mode = DetailMode::Reader;
                self.search_servers(&scope, query, cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
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
                    view.refresh_search_results();
                    view.refresh_from_store(cx);
                    cx.notify();
                });
            })
            .detach();
        }
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

#[cfg(test)]
mod tests {
    use zmail_core::{MailboxRole, VirtualMailbox};

    use super::*;

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
