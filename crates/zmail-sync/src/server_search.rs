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
//! normal sync path, brings it in: the search asks for it with
//! [`SyncService::request_sync`], so it queues behind a sync already
//! running (which then runs once more, picking up mail that arrived after
//! it started) instead of starting its own. Searches ask at most once per
//! account every [`SEARCH_SYNC_COOLDOWN`], so repeated searches never chain
//! full-account syncs.
//!
//! A search scoped to a folder ([`SyncService::search_server_in_and_wait`])
//! asks IMAP about that folder's mailboxes only; an `in:` clause in the
//! query names the mailboxes instead. JMAP's `Email/query` covers the
//! account and the store keeps the folder's hits. A search across every
//! account runs this once per account; each account's failure is its own.
//! Should a sync window or partial folder sync ever be added, unknown hits
//! would need a targeted header fetch (`UID FETCH` / `Email/get` on the
//! ids) fed through the same snapshot merge instead.
//!
//! [`LocalStore::search_needs_server`]: zmail_store::LocalStore::search_needs_server
//! [`LocalStore::record_server_search_hits`]: zmail_store::LocalStore::record_server_search_hits

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::time::sleep;
use zmail_core::{
    MailboxSelection, MailboxSummary, ProtocolKind, VirtualMailbox,
    search::{SearchQuery, SearchTerm, mailbox_matches},
};

use crate::SyncService;

/// Most hits taken from the server: per mailbox for IMAP, per account for
/// JMAP. The newest are kept.
pub const SERVER_SEARCH_MAX_HITS: usize = 200;

/// Shortest interval between two syncs started by server searches for the
/// same account. Searches inside it resolve against the store as it is.
pub const SEARCH_SYNC_COOLDOWN: Duration = Duration::from_secs(60);

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
        self.search_server_in_and_wait(account_id, None, query)
            .await
    }

    /// Like [`Self::search_server_and_wait`], for a search scoped to
    /// `folder` (one of the account's mailboxes, or a virtual mailbox
    /// across accounts): IMAP searches only the folder's mailboxes. With
    /// no folder it covers the whole account.
    pub async fn search_server_in_and_wait(
        &self,
        account_id: i64,
        folder: Option<MailboxSelection>,
        query: &str,
    ) -> Result<ServerSearchReport> {
        let service = self.clone();
        let query = query.to_owned();
        self.inner
            .handle
            .spawn(async move {
                service
                    .search_server_job(account_id, folder.as_ref(), &query)
                    .await
            })
            .await
            .context("the server search worker exited before returning a result")?
    }

    async fn search_server_job(
        &self,
        account_id: i64,
        folder: Option<&MailboxSelection>,
        query: &str,
    ) -> Result<ServerSearchReport> {
        let store = &self.inner.store;
        let mut report = ServerSearchReport::default();
        let parsed = SearchQuery::parse(query);
        // An `in:` clause names the mailboxes itself.
        let folder = folder.filter(|_| parsed.mailbox_clause().is_none());
        if !store.search_needs_server_in(account_id, folder, query)? {
            return Ok(report);
        }
        let Some(target) = store.sync_target(account_id)? else {
            bail!("account {account_id} no longer exists");
        };
        let secret = self.connect_secret(&target).await?;
        let hits = match target.protocol {
            ProtocolKind::Imap => {
                let mailboxes = store
                    .list_mailboxes(account_id)?
                    .into_iter()
                    .filter(|mailbox| {
                        mailbox_in_folder(folder, mailbox)
                            && mailbox_in_scope(
                                &parsed,
                                &mailbox.name,
                                mailbox.role.map(|role| role.as_str()),
                            )
                    })
                    .map(|mailbox| mailbox.name)
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
        // Mail newer than the last sync: sync, then resolve again.
        self.sync_for_unknown_hits(account_id).await;
        let still_unknown = store.record_server_search_hits(account_id, query, &hits)?;
        report.fetched_by_sync = unknown.len() - still_unknown.len().min(unknown.len());
        report.unresolved = still_unknown.len();
        Ok(report)
    }
}

impl SyncService {
    /// Brings in mail a server search found but the store does not have
    /// yet. Asks for a sync with [`SyncService::request_sync`], at most
    /// once per account every [`SEARCH_SYNC_COOLDOWN`]: with a sync already
    /// running, the request queues behind it and that sync runs once more,
    /// so mail that arrived after it started is fetched too. Then waits
    /// until the account has no sync running or queued. A failed sync is
    /// the sync's own to report; the search resolves what it can.
    async fn sync_for_unknown_hits(&self, account_id: i64) {
        if self.claim_search_sync(account_id) {
            self.request_sync(account_id);
        }
        while self.is_syncing(account_id) {
            sleep(SYNC_WAIT_INTERVAL).await;
        }
    }

    /// Records a search-triggered sync for the account, returning `false`
    /// when one already ran within [`SEARCH_SYNC_COOLDOWN`].
    fn claim_search_sync(&self, account_id: i64) -> bool {
        let mut syncs = self
            .inner
            .search_syncs
            .lock()
            .expect("search sync mutex should not be poisoned");
        let now = Instant::now();
        if !search_sync_allowed(syncs.get(&account_id).copied(), now) {
            return false;
        }
        syncs.insert(account_id, now);
        true
    }
}

/// Whether a search may start a sync, given when the last one started.
fn search_sync_allowed(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= SEARCH_SYNC_COOLDOWN)
}

/// Whether a mailbox belongs to the folder a search is scoped to. A
/// virtual folder's mailboxes are each account's mailbox with its role;
/// Flagged spans every mailbox (the store keeps the flagged hits).
fn mailbox_in_folder(folder: Option<&MailboxSelection>, mailbox: &MailboxSummary) -> bool {
    match folder {
        None | Some(MailboxSelection::Virtual(VirtualMailbox::Flagged)) => true,
        Some(MailboxSelection::Mailbox { mailbox_id, .. }) => mailbox.id == *mailbox_id,
        Some(MailboxSelection::Virtual(VirtualMailbox::Unified(role))) => {
            mailbox.role == Some(*role)
        }
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
    fn search_syncs_are_rate_limited_per_account() {
        let start = Instant::now();
        assert!(search_sync_allowed(None, start));
        assert!(!search_sync_allowed(Some(start), start));
        assert!(!search_sync_allowed(
            Some(start),
            start + SEARCH_SYNC_COOLDOWN - Duration::from_millis(1)
        ));
        assert!(search_sync_allowed(
            Some(start),
            start + SEARCH_SYNC_COOLDOWN
        ));
    }

    #[test]
    fn folder_scopes_choose_the_mailboxes() {
        use zmail_core::MailboxRole;

        let mailbox = |id: i64, name: &str, role: Option<MailboxRole>| MailboxSummary {
            id,
            account_id: 1,
            name: name.to_owned(),
            role,
            delimiter: None,
            parent: None,
            total_threads: 0,
            unread_threads: 0,
        };
        let inbox = mailbox(1, "INBOX", Some(MailboxRole::Inbox));
        let archive = mailbox(2, "Archive", Some(MailboxRole::Archive));
        let one = MailboxSelection::Mailbox {
            account_id: 1,
            mailbox_id: 2,
        };
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        let flagged = MailboxSelection::Virtual(VirtualMailbox::Flagged);

        assert!(mailbox_in_folder(None, &inbox) && mailbox_in_folder(None, &archive));
        assert!(!mailbox_in_folder(Some(&one), &inbox));
        assert!(mailbox_in_folder(Some(&one), &archive));
        assert!(mailbox_in_folder(Some(&unified), &inbox));
        assert!(!mailbox_in_folder(Some(&unified), &archive));
        assert!(mailbox_in_folder(Some(&flagged), &archive));
    }

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_hits_queue_a_rerun_behind_a_running_sync() {
        use std::sync::{Arc, atomic::Ordering};

        use zmail_core::AuthKind;
        use zmail_store::LocalStore;

        use crate::tests::{CountingSecretStore, insert_account};

        let store = Arc::new(LocalStore::open_ephemeral().expect("store should open"));
        let account_id = insert_account(&store, AuthKind::Password);
        let secrets = Arc::new(CountingSecretStore::default());
        let service = SyncService::new(
            tokio::runtime::Handle::current().clone(),
            store,
            secrets.clone(),
        );
        let lookups = || secrets.lookups.load(Ordering::SeqCst);

        // A sync is running: two failing attempts two seconds apart, one
        // credential lookup each. The search's sync queues behind it, so
        // it runs once more, and the search waits for both.
        service.request_sync(account_id);
        service.sync_for_unknown_hits(account_id).await;
        assert!(!service.is_syncing(account_id));
        assert_eq!(lookups(), 4, "the running sync ran once more");

        // Within the cooldown, another search asks for nothing.
        service.sync_for_unknown_hits(account_id).await;
        sleep(Duration::from_millis(300)).await;
        assert_eq!(lookups(), 4);
        assert!(!service.is_syncing(account_id));
    }
}
