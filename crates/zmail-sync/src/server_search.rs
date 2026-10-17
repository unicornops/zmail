//! Server-side search fallback.
//!
//! The local index answers every search first. When a query needs message
//! bodies and some of the account's bodies are not indexed yet (see
//! [`LocalStore::search_needs_server`]), the server is asked too: IMAP
//! `UID SEARCH` per mailbox or JMAP `Email/query`. Its hits are recorded in
//! the store ([`LocalStore::record_server_search_hits`]), so the UI keeps
//! reading local state and shows them as server results.
//!
//! Headers are always fully mirrored (IMAP syncs every UID of every
//! selectable mailbox and JMAP queries every email; only bodies are lazy,
//! and eviction only drops bodies), so a hit the store does not know is
//! mail that arrived after the last sync. One account sync, through the
//! normal sync path, brings it in; a search never triggers more than one.
//! Should a sync window or partial folder sync ever be added, unknown hits
//! would need a targeted header fetch (`UID FETCH` / `Email/get` on the
//! ids) fed through the same snapshot merge instead.
//!
//! [`LocalStore::search_needs_server`]: zmail_store::LocalStore::search_needs_server
//! [`LocalStore::record_server_search_hits`]: zmail_store::LocalStore::record_server_search_hits

use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::time::sleep;
use zmail_core::{
    ProtocolKind,
    search::{SearchQuery, SearchTerm, mailbox_matches},
};

use crate::SyncService;

/// Most hits taken from the server: per mailbox for IMAP, per account for
/// JMAP. The newest are kept.
pub const SERVER_SEARCH_MAX_HITS: usize = 200;

/// How often a server search waiting for a running sync re-checks it.
const SYNC_WAIT_INTERVAL: Duration = Duration::from_millis(250);

/// Outcome of one server search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerSearchReport {
    /// Whether the server was asked at all (the local index may already
    /// cover the query).
    pub searched: bool,
    /// Hits the server returned.
    pub hits: usize,
    /// Hits that were not in the store before this search ran a sync.
    pub fetched_by_sync: usize,
    /// Hits still unknown afterwards (for example deleted meanwhile).
    pub unresolved: usize,
}

impl SyncService {
    /// Runs the server search fallback for `query` and records its hits in
    /// the store, so the next [`LocalStore::search_threads`] for the same
    /// query text includes them.
    ///
    /// The protocol work runs on the background runtime, so this can be
    /// awaited from any executor (such as the GUI's). Run it once per
    /// submitted query, not per keystroke: a hit for mail newer than the
    /// last sync costs one account sync.
    ///
    /// [`LocalStore::search_threads`]: zmail_store::LocalStore::search_threads
    pub async fn search_server_and_wait(
        &self,
        account_id: i64,
        query: &str,
    ) -> Result<ServerSearchReport> {
        let service = self.clone();
        let query = query.to_owned();
        self.inner
            .handle
            .spawn(async move { service.search_server_job(account_id, &query).await })
            .await
            .context("the server search worker exited before returning a result")?
    }

    async fn search_server_job(&self, account_id: i64, query: &str) -> Result<ServerSearchReport> {
        let store = &self.inner.store;
        let mut report = ServerSearchReport::default();
        if !store.search_needs_server(account_id, query)? {
            return Ok(report);
        }
        let Some(target) = store.sync_target(account_id)? else {
            bail!("account {account_id} no longer exists");
        };
        let parsed = SearchQuery::parse(query);
        let secret = self.connect_secret(&target).await?;
        let hits = match target.protocol {
            ProtocolKind::Imap => {
                let mailboxes = store
                    .mailbox_names_and_roles(account_id)?
                    .into_iter()
                    .filter(|(name, role)| mailbox_in_scope(&parsed, name, role.as_deref()))
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>();
                zmail_imap::search_messages(
                    &target,
                    &secret,
                    &parsed,
                    &mailboxes,
                    SERVER_SEARCH_MAX_HITS,
                )
                .await?
            }
            ProtocolKind::Jmap => {
                zmail_jmap::search_emails(
                    &target,
                    &secret,
                    &parsed,
                    &chrono::Local::now(),
                    SERVER_SEARCH_MAX_HITS,
                )
                .await?
            }
        };
        report.searched = true;
        report.hits = hits.len();

        let unknown = store.record_server_search_hits(account_id, query, &hits)?;
        if unknown.is_empty() {
            return Ok(report);
        }
        // Mail newer than the last sync: let a running sync finish, or run
        // one, then resolve again. At most one sync per search.
        if self.is_syncing(account_id) {
            while self.is_syncing(account_id) {
                sleep(SYNC_WAIT_INTERVAL).await;
            }
        } else if self.mark_account_active(account_id) {
            self.clone().run_sync_job(account_id).await?;
        } else {
            while self.is_syncing(account_id) {
                sleep(SYNC_WAIT_INTERVAL).await;
            }
        }
        let still_unknown = store.record_server_search_hits(account_id, query, &hits)?;
        report.fetched_by_sync = unknown.len() - still_unknown.len().min(unknown.len());
        report.unresolved = still_unknown.len();
        Ok(report)
    }
}

/// Whether a mailbox is covered by the query's `in:` clauses: it must match
/// every positive one and no negated one.
fn mailbox_in_scope(query: &SearchQuery, name: &str, role: Option<&str>) -> bool {
    query.clauses.iter().all(|clause| match &clause.term {
        SearchTerm::Mailbox(value) => mailbox_matches(value, name, role) != clause.negated,
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_clauses_choose_the_mailboxes() {
        let scope = |query: &str, name: &str, role: Option<&str>| {
            mailbox_in_scope(&SearchQuery::parse(query), name, role)
        };
        assert!(scope("report", "Archive", Some("archive")));
        assert!(scope("report in:inbox", "INBOX", Some("inbox")));
        assert!(!scope("report in:inbox", "Archive", Some("archive")));
        assert!(!scope("report -in:spam", "Bulk", Some("junk")));
        assert!(scope("report -in:spam", "INBOX", None));
    }
}
