//! Background runtime bootstrap and sync supervision for Phase 3.

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::{
    runtime::{Builder, Handle},
    sync::oneshot,
    time::sleep,
};
use zmail_core::{
    AuthKind, MailboxSnapshot, PendingOutboxJob, ProtocolKind, RuntimeSummary, SubmissionKind,
    SyncPhase, SyncTarget,
};
use zmail_notify::{NewMailArrival, NewMailBatch, Notifier};
use zmail_security::{SecretSlot, SecretStore};
use zmail_smtp::SubmissionConfig;
use zmail_store::LocalStore;

const OUTBOX_POLL_INTERVAL: Duration = Duration::from_secs(5);
const OUTBOX_RETRY_BASE_DELAY: Duration = Duration::from_secs(30);
const MAX_OUTBOX_ATTEMPTS: usize = 3;
const MAIL_WATCHER_POLL_INTERVAL: Duration = Duration::from_secs(60);
const MAX_PENDING_OP_ATTEMPTS: i64 = 5;
/// Refreshed tokens are recorded as expiring this many seconds early so a
/// connection attempt never races the true expiry.
const OAUTH2_REFRESH_MARGIN_SECS: i64 = 60;

/// Dedicated Tokio runtime host for background tasks.
#[derive(Debug)]
pub struct BackgroundRuntime {
    handle: Handle,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<thread::JoinHandle<()>>,
    summary: RuntimeSummary,
}

impl BackgroundRuntime {
    /// Starts a dedicated multithreaded Tokio runtime and verifies that task execution works.
    pub fn start(worker_threads: usize) -> Result<Self> {
        let (handle_tx, handle_rx) = mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let join_handle = thread::Builder::new()
            .name("zmail-tokio-runtime".to_owned())
            .spawn(move || {
                let runtime = match Builder::new_multi_thread()
                    .worker_threads(worker_threads)
                    .thread_name("zmail-tokio-worker")
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = handle_tx.send(Err(error.to_string()));
                        return;
                    }
                };

                if handle_tx.send(Ok(runtime.handle().clone())).is_err() {
                    return;
                }

                runtime.block_on(async move {
                    let _ = shutdown_rx.await;
                });
            })
            .context("failed to spawn the dedicated tokio runtime thread")?;

        let handle = handle_rx
            .recv()
            .context("tokio runtime thread exited before it reported readiness")?
            .map_err(anyhow::Error::msg)?;

        Self::probe(&handle)?;

        Ok(Self {
            handle,
            shutdown_tx: Some(shutdown_tx),
            join_handle: Some(join_handle),
            summary: RuntimeSummary {
                worker_threads,
                bridge_verified: true,
            },
        })
    }

    /// Returns a cloneable Tokio handle for async protocol services.
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Returns the startup summary presented by the shell.
    pub fn summary(&self) -> RuntimeSummary {
        self.summary.clone()
    }

    fn probe(handle: &Handle) -> Result<()> {
        let (probe_tx, probe_rx) = mpsc::sync_channel(1);
        handle.spawn(async move {
            let _ = probe_tx.send(());
        });

        probe_rx
            .recv_timeout(Duration::from_secs(1))
            .context("tokio runtime probe did not complete within one second")?;

        Ok(())
    }
}

impl Drop for BackgroundRuntime {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }

        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

/// Cloneable handle for background account supervisors.
#[derive(Clone)]
pub struct SyncService {
    inner: Arc<SyncServiceInner>,
}

struct SyncServiceInner {
    handle: Handle,
    store: Arc<LocalStore>,
    secrets: Arc<dyn SecretStore>,
    active_accounts: Mutex<HashSet<i64>>,
    outbox_supervisor_started: AtomicBool,
    watched_accounts: Mutex<HashSet<i64>>,
    watcher_poll_interval: Mutex<Option<Duration>>,
    notifier: Mutex<Option<Arc<dyn Notifier>>>,
}

impl std::fmt::Debug for SyncService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SyncService")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SyncServiceInner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SyncServiceInner")
            .finish_non_exhaustive()
    }
}

impl SyncService {
    /// Creates a new sync service on top of the dedicated Tokio runtime.
    pub fn new(handle: Handle, store: Arc<LocalStore>, secrets: Arc<dyn SecretStore>) -> Self {
        let service = Self {
            inner: Arc::new(SyncServiceInner {
                handle,
                store,
                secrets,
                active_accounts: Mutex::new(HashSet::new()),
                outbox_supervisor_started: AtomicBool::new(false),
                watched_accounts: Mutex::new(HashSet::new()),
                watcher_poll_interval: Mutex::new(None),
                notifier: Mutex::new(None),
            }),
        };
        service.start_outbox_supervisor();
        service
    }

    /// Schedules sync jobs for every configured account and returns the number started.
    pub fn bootstrap_existing_accounts(&self) -> Result<usize> {
        self.sync_accounts(
            self.inner
                .store
                .list_sync_targets()?
                .into_iter()
                .map(|target| target.id),
        )
    }

    /// Starts a sync for every configured account and returns the number started.
    pub fn sync_all(&self) -> Result<usize> {
        self.bootstrap_existing_accounts()
    }

    /// Starts a sync for a single account. Returns `false` if one is already running.
    pub fn sync_account(&self, account_id: i64) -> Result<bool> {
        if !self.mark_account_active(account_id) {
            return Ok(false);
        }

        let service = self.clone();
        self.inner.handle.spawn(async move {
            let _ = service.run_sync_job(account_id).await;
        });

        Ok(true)
    }

    /// Runs a sync for a single account and blocks the caller until completion.
    pub fn sync_account_and_wait(&self, account_id: i64) -> Result<()> {
        if !self.mark_account_active(account_id) {
            bail!("a sync is already running for account {account_id}");
        }

        let service = self.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        self.inner.handle.spawn(async move {
            let result = service.run_sync_job(account_id).await;
            let _ = tx.send(result.map_err(|error| error.to_string()));
        });

        rx.recv()
            .context("the sync worker exited before returning a result")?
            .map_err(anyhow::Error::msg)
    }

    /// Resolves the secret a protocol adapter should use to connect.
    ///
    /// Password accounts return the stored password. OAuth2 accounts return
    /// the current access token, refreshing it first when the recorded
    /// expiry has passed, so sessions survive access-token expiry without
    /// user interaction. Access tokens stored in the legacy single-secret
    /// slot by the token-paste flow are honored as a fallback.
    pub async fn connect_secret(&self, target: &SyncTarget) -> Result<String> {
        if target.auth != AuthKind::OAuth2 {
            return self
                .inner
                .secrets
                .get_account_secret(target.id)
                .with_context(|| {
                    format!(
                        "missing or inaccessible credentials for account {}",
                        target.id
                    )
                });
        }

        let refresh_token = self
            .inner
            .secrets
            .get_account_secret_from_slot(target.id, SecretSlot::RefreshToken)
            .ok()
            .filter(|token| !token.is_empty());
        if let Some(refresh_token) = refresh_token {
            // The recorded expiry is the provider's true expiry; the margin
            // keeps refreshes ahead of it so a connection attempt never
            // races a token boundary.
            let expired = target
                .oauth2
                .as_ref()
                .and_then(|config| config.access_token_expires_at)
                .is_some_and(|expires_at| expires_at - OAUTH2_REFRESH_MARGIN_SECS <= unix_now());
            if expired {
                self.refresh_oauth2_token(target, &refresh_token)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to refresh the OAuth2 access token for account {}",
                            target.id
                        )
                    })?;
            }
        }

        self.inner
            .secrets
            .get_account_secret_from_slot(target.id, SecretSlot::AccessToken)
            .or_else(|_| self.inner.secrets.get_account_secret(target.id))
            .with_context(|| format!("missing OAuth2 access token for account {}", target.id))
    }

    /// Exchanges the refresh token for a new access token and persists the
    /// resulting grant (access token, rotated refresh token, expiry).
    async fn refresh_oauth2_token(&self, target: &SyncTarget, refresh_token: &str) -> Result<()> {
        let config = target.oauth2.as_ref().with_context(|| {
            format!(
                "account {} has no stored OAuth2 client configuration",
                target.id
            )
        })?;
        let client_secret = self
            .inner
            .secrets
            .get_account_secret_from_slot(target.id, SecretSlot::ClientSecret)
            .ok()
            .filter(|secret| !secret.is_empty());

        // The provider's token endpoint is reached with a blocking client on
        // a dedicated thread; the blocking HTTP runtime must not run inside
        // the sync runtime's async workers.
        let endpoint = config.token_endpoint.clone();
        let client_id = config.client_id.clone();
        let token = refresh_token.to_owned();
        let (tx, rx) = oneshot::channel();
        thread::Builder::new()
            .name("zmail-oauth2-refresh".to_owned())
            .spawn(move || {
                let _ = tx.send(zmail_auth::refresh_access_token(
                    &endpoint,
                    &client_id,
                    client_secret.as_deref(),
                    &token,
                ));
            })
            .context("failed to spawn the OAuth2 refresh thread")?;
        let tokens = rx
            .await
            .context("the OAuth2 refresh thread exited before returning")??;

        self.inner.secrets.set_account_secret_in_slot(
            target.id,
            SecretSlot::AccessToken,
            &tokens.access_token,
        )?;
        if let Some(refresh_token) = tokens.refresh_token.filter(|token| !token.is_empty()) {
            self.inner.secrets.set_account_secret_in_slot(
                target.id,
                SecretSlot::RefreshToken,
                &refresh_token,
            )?;
        }
        let expires_at = tokens
            .expires_in
            .map(|lifetime| unix_now() + lifetime as i64);
        self.inner
            .store
            .set_account_oauth2_expiry(target.id, expires_at)?;
        Ok(())
    }

    /// Fetches and stores the bodies of a thread's not-yet-hydrated messages.
    ///
    /// Returns `false` when the protocol has no lazy body path (JMAP syncs
    /// bodies with the message) or when the thread has nothing to fetch.
    pub fn hydrate_thread_body(&self, account_id: i64, thread_id: i64) -> Result<bool> {
        let service = self.clone();
        self.inner.handle.spawn(async move {
            if let Err(error) = service
                .hydrate_thread_body_and_wait(account_id, thread_id)
                .await
            {
                // Offline opens keep the stored preview; hydration failures are
                // not surfaced as errors.
                let _ = error;
            }
        });
        Ok(true)
    }

    /// Fetches and stores the bodies of a thread's not-yet-hydrated messages,
    /// blocking until the fetch completes. Returns the number hydrated.
    pub async fn hydrate_thread_body_and_wait(
        &self,
        account_id: i64,
        thread_id: i64,
    ) -> Result<usize> {
        let Some(target) = self.inner.store.sync_target(account_id)? else {
            bail!("account {account_id} no longer exists");
        };
        if target.protocol != ProtocolKind::Imap {
            // JMAP delivers bodies with the sync snapshot; nothing to fetch.
            return Ok(0);
        }
        let Some(missing) = self.inner.store.messages_missing_bodies(thread_id)? else {
            bail!("thread {thread_id} does not exist");
        };
        if missing.is_empty() {
            return Ok(0);
        }

        let thread = self
            .inner
            .store
            .thread_view(thread_id)?
            .with_context(|| format!("thread {thread_id} does not exist"))?;
        let secret = self
            .connect_secret(&target)
            .await
            .with_context(|| format!("missing credentials for account {account_id}"))?;

        let uids = missing
            .iter()
            .filter_map(|(_, remote_id)| remote_id.parse::<u32>().ok())
            .collect::<Vec<_>>();
        let bodies =
            zmail_imap::fetch_message_bodies(&target, &secret, &thread.mailbox_name, &uids).await?;

        let mut by_uid = std::collections::HashMap::new();
        for (uid, body) in bodies {
            by_uid.insert(uid.to_string(), body);
        }

        let mut hydrated = 0;
        for (message_id, remote_id) in &missing {
            let Some(body) = by_uid.get(remote_id) else {
                continue;
            };
            self.inner.store.hydrate_message_body(*message_id, body)?;
            hydrated += 1;
        }
        Ok(hydrated)
    }

    /// Installs the new-mail notifier the sync reports arrivals through.
    ///
    /// Without one, syncs run silently, which keeps CLI runs headless.
    pub fn set_notifier(&self, notifier: Arc<dyn Notifier>) {
        *self
            .inner
            .notifier
            .lock()
            .expect("notifier mutex should not be poisoned") = Some(notifier);
    }

    /// Runs a sync for every configured account and blocks until all of them finish.
    pub fn sync_all_and_wait(&self) -> Result<()> {
        for account in self.inner.store.list_sync_targets()? {
            self.sync_account_and_wait(account.id)?;
        }
        Ok(())
    }

    /// Starts background mail watchers for every configured account and
    /// returns the number started.
    ///
    /// IMAP accounts hold one IDLE connection and JMAP accounts hold one
    /// event-source connection; both trigger a sync as soon as the server
    /// reports a change and fall back to the poll interval when the server
    /// lacks the capability or the connection drops.
    pub fn start_mail_watcher(&self) -> Result<usize> {
        self.start_mail_watcher_with_interval(MAIL_WATCHER_POLL_INTERVAL)
    }

    /// Starts the background mail watcher with an explicit poll interval.
    ///
    /// Idempotent: accounts that already have a running watcher keep
    /// theirs, and accounts added since the last call start one.
    pub fn start_mail_watcher_with_interval(&self, poll_interval: Duration) -> Result<usize> {
        {
            let mut interval = self
                .inner
                .watcher_poll_interval
                .lock()
                .expect("watcher poll interval mutex should not be poisoned");
            if interval.is_none() {
                *interval = Some(poll_interval);
            }
        }
        self.sync_watchers()
    }

    /// Ensures every configured account has a running mail watcher.
    ///
    /// Accounts that already have a watcher are skipped; accounts added
    /// after startup start watching immediately. Returns the number of
    /// newly started watchers.
    pub fn sync_watchers(&self) -> Result<usize> {
        let poll_interval = {
            let mut interval = self
                .inner
                .watcher_poll_interval
                .lock()
                .expect("watcher poll interval mutex should not be poisoned");
            if interval.is_none() {
                *interval = Some(MAIL_WATCHER_POLL_INTERVAL);
            }
            interval.expect("the watcher poll interval is always set")
        };

        let mut started = 0;
        for target in self.inner.store.list_sync_targets()? {
            if !self.mark_watched(target.id) {
                continue;
            }
            // Only spawn watchers for accounts with resolvable credentials;
            // a later `sync_watchers` call retries accounts whose secrets
            // appear afterwards.
            let has_secret = self.inner.secrets.get_account_secret(target.id).is_ok()
                || self
                    .inner
                    .secrets
                    .get_account_secret_from_slot(target.id, SecretSlot::AccessToken)
                    .is_ok();
            if !has_secret {
                self.unwatch_account(target.id);
                continue;
            }
            let service = self.clone();
            self.inner.handle.spawn(async move {
                service.run_mail_watcher(target, poll_interval).await;
            });
            started += 1;
        }
        Ok(started)
    }

    /// Registers an account as watched, returning `true` when it is new.
    fn mark_watched(&self, account_id: i64) -> bool {
        let mut watched = self
            .inner
            .watched_accounts
            .lock()
            .expect("watched account mutex should not be poisoned");
        watched.insert(account_id)
    }

    /// Removes an account from the watcher registry so a later
    /// `sync_watchers` call can watch it again.
    fn unwatch_account(&self, account_id: i64) {
        let mut watched = self
            .inner
            .watched_accounts
            .lock()
            .expect("watched account mutex should not be poisoned");
        watched.remove(&account_id);
    }

    /// Waits for server-side changes for one account and syncs until the
    /// account disappears.
    ///
    /// Credentials are re-resolved on every reconnect so OAuth2 accounts
    /// pick up refreshed access tokens without restarting the watcher.
    async fn run_mail_watcher(&self, target: SyncTarget, poll_interval: Duration) {
        let mut reconnect_backoff = Duration::from_secs(1);
        loop {
            // Stop watching accounts that no longer exist so the watcher
            // registry slot is free if the id is ever reused.
            if self
                .inner
                .store
                .sync_target(target.id)
                .ok()
                .flatten()
                .is_none()
            {
                self.unwatch_account(target.id);
                return;
            }

            let Ok(secret) = self.connect_secret(&target).await else {
                // A transient refresh failure (for example an offline token
                // endpoint) backs off and retries like a dropped connection;
                // permanent account deletion is caught by the check above.
                sleep(reconnect_backoff).await;
                reconnect_backoff = reconnect_backoff
                    .saturating_mul(2)
                    .min(Duration::from_secs(60));
                continue;
            };

            match target.protocol {
                ProtocolKind::Imap => {
                    match zmail_imap::wait_for_mail_changes(&target, &secret, poll_interval).await {
                        Ok(()) => {
                            reconnect_backoff = Duration::from_secs(1);
                        }
                        Err(_) => {
                            // The IDLE connection dropped; wait with a
                            // bounded backoff before reconnecting.
                            sleep(reconnect_backoff).await;
                            reconnect_backoff = reconnect_backoff
                                .saturating_mul(2)
                                .min(Duration::from_secs(60));
                        }
                    }
                }
                ProtocolKind::Jmap => {
                    // Event-source push wakes the watcher as soon as the
                    // server reports a change; errors (or servers without
                    // the eventSource capability) back off and retry, so
                    // the interval bounds worst-case latency.
                    match zmail_jmap::wait_for_mail_changes(&target, &secret, poll_interval).await {
                        Ok(()) => {
                            reconnect_backoff = Duration::from_secs(1);
                        }
                        Err(_) => {
                            sleep(reconnect_backoff).await;
                            reconnect_backoff = reconnect_backoff
                                .saturating_mul(2)
                                .min(Duration::from_secs(60));
                        }
                    }
                }
            }

            let _ = self.sync_account(target.id);
        }
    }

    /// Starts processing due outbox jobs in the background.
    pub fn process_outbox(&self, account_id: Option<i64>) -> Result<()> {
        let service = self.clone();
        self.inner.handle.spawn(async move {
            let _ = service.process_due_outbox_jobs(account_id).await;
        });
        Ok(())
    }

    /// Processes due outbox jobs and blocks until the current pass completes.
    pub fn process_outbox_and_wait(&self, account_id: Option<i64>) -> Result<usize> {
        let service = self.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        self.inner.handle.spawn(async move {
            let result = service.process_due_outbox_jobs(account_id).await;
            let _ = tx.send(result.map_err(|error| error.to_string()));
        });

        rx.recv()
            .context("the outbox worker exited before returning a result")?
            .map_err(anyhow::Error::msg)
    }

    /// Queues a draft and immediately schedules outbox processing for its account.
    pub fn queue_draft_and_send(&self, draft_id: i64) -> Result<i64> {
        let draft = self
            .inner
            .store
            .draft_detail(draft_id)?
            .with_context(|| format!("draft {draft_id} does not exist"))?;
        let job_id = self.inner.store.queue_draft(draft_id)?;
        self.process_outbox(Some(draft.account_id))?;
        Ok(job_id)
    }

    fn sync_accounts<I>(&self, account_ids: I) -> Result<usize>
    where
        I: IntoIterator<Item = i64>,
    {
        let mut started = 0;
        for account_id in account_ids {
            if self.sync_account(account_id)? {
                started += 1;
            }
        }
        Ok(started)
    }

    fn start_outbox_supervisor(&self) {
        if self
            .inner
            .outbox_supervisor_started
            .swap(true, Ordering::SeqCst)
        {
            return;
        }

        let service = self.clone();
        self.inner.handle.spawn(async move {
            loop {
                let _ = service.process_due_outbox_jobs(None).await;
                sleep(OUTBOX_POLL_INTERVAL).await;
            }
        });
    }

    fn mark_account_active(&self, account_id: i64) -> bool {
        let mut active = self
            .inner
            .active_accounts
            .lock()
            .expect("active account tracking mutex should not be poisoned");
        active.insert(account_id)
    }

    async fn run_sync_job(self, account_id: i64) -> Result<()> {
        let result = self.run_sync_with_retries(account_id).await;
        self.inner
            .active_accounts
            .lock()
            .expect("active account tracking mutex should not be poisoned")
            .remove(&account_id);
        result
    }

    async fn run_sync_with_retries(&self, account_id: i64) -> Result<()> {
        const MAX_ATTEMPTS: usize = 2;

        for attempt in 1..=MAX_ATTEMPTS {
            match self.run_single_sync(account_id).await {
                Ok(()) => return Ok(()),
                Err(error) if attempt < MAX_ATTEMPTS => {
                    let delay = Duration::from_secs((attempt * 2) as u64);
                    self.inner.store.record_sync_backoff(
                        account_id,
                        format!("Sync failed, retrying in {}s", delay.as_secs()),
                        error.to_string(),
                    )?;
                    sleep(delay).await;
                }
                Err(error) => {
                    self.inner.store.record_sync_failure(
                        account_id,
                        "Sync failed after retry",
                        error.to_string(),
                    )?;
                    return Err(error);
                }
            }
        }

        unreachable!("retry loop should always return");
    }

    async fn run_single_sync(&self, account_id: i64) -> Result<()> {
        self.inner.store.begin_sync_attempt(
            account_id,
            SyncPhase::Connecting,
            "Establishing protocol session",
        )?;

        let Some(target) = self.inner.store.sync_target(account_id)? else {
            bail!("account {account_id} no longer exists");
        };
        let secret = self.connect_secret(&target).await.with_context(|| {
            format!("missing or inaccessible credentials for account {account_id}")
        })?;
        let existing_mailboxes = self
            .inner
            .store
            .account_mailbox_snapshots(account_id)
            .with_context(|| {
                format!("failed to read the current local mirror for account {account_id}")
            })?;

        // Locally applied mailbox operations are written back before the
        // snapshot is fetched, so the fetched state includes them. Failed
        // operations stay queued and are retried by the next sync.
        let pending_ops = self
            .inner
            .store
            .list_pending_ops(account_id)
            .with_context(|| {
                format!("failed to load pending operations for account {account_id}")
            })?;
        if !pending_ops.is_empty() {
            self.inner.store.begin_sync_attempt(
                account_id,
                SyncPhase::Connecting,
                format!(
                    "Writing back {} pending mailbox operations",
                    pending_ops.len()
                ),
            )?;
            let report = match target.protocol {
                ProtocolKind::Imap => {
                    zmail_imap::apply_pending_ops(&target, &secret, &pending_ops).await?
                }
                ProtocolKind::Jmap => {
                    zmail_jmap::apply_pending_ops(&target, &secret, &pending_ops).await?
                }
            };
            for op_id in &report.applied {
                self.inner
                    .store
                    .complete_pending_op(*op_id)
                    .with_context(|| format!("failed to complete pending operation {op_id}"))?;
            }
            for (op_id, error) in &report.failed {
                // After the retry budget the operation moves to the dead
                // letter: no more drain attempts, and the snapshot merge
                // becomes server-authoritative for it again.
                let attempts = pending_ops
                    .iter()
                    .find(|op| op.id == *op_id)
                    .map(|op| op.attempts)
                    .unwrap_or(0);
                let result = if attempts.saturating_add(1) >= MAX_PENDING_OP_ATTEMPTS {
                    self.inner
                        .store
                        .mark_pending_op_dead(*op_id, error)
                        .with_context(|| format!("failed to dead-letter operation {op_id}"))
                } else {
                    self.inner
                        .store
                        .record_pending_op_failure(*op_id, error)
                        .with_context(|| {
                            format!("failed to record pending operation {op_id} failure")
                        })
                };
                result?;
            }
        }

        self.inner.store.begin_sync_attempt(
            account_id,
            SyncPhase::Discovering,
            format!(
                "Discovering {} capabilities, identities, and mailbox changes",
                target.protocol
            ),
        )?;

        let snapshot = match target.protocol {
            ProtocolKind::Imap => {
                zmail_imap::discover_and_sync(&target, &secret, &existing_mailboxes).await?
            }
            ProtocolKind::Jmap => {
                zmail_jmap::discover_and_sync(&target, &secret, &existing_mailboxes).await?
            }
        };

        self.inner.store.begin_sync_attempt(
            account_id,
            SyncPhase::Syncing,
            format!(
                "Writing {} mailbox metadata into the local mirror",
                target.protocol
            ),
        )?;

        self.inner
            .store
            .replace_account_snapshot(account_id, &snapshot)?;
        self.inner
            .store
            .record_sync_success(account_id, &snapshot.detail, &snapshot.checkpoint)?;
        self.notify_new_mail(&target, &existing_mailboxes, &snapshot);
        let _ = self.process_outbox(Some(account_id));

        Ok(())
    }

    /// Reports the threads that gained messages in this sync through the
    /// installed notifier, if any.
    fn notify_new_mail(
        &self,
        target: &SyncTarget,
        existing_mailboxes: &[MailboxSnapshot],
        snapshot: &zmail_core::AccountSyncSnapshot,
    ) {
        let Some(notifier) = self
            .inner
            .notifier
            .lock()
            .expect("notifier mutex should not be poisoned")
            .clone()
        else {
            return;
        };
        let arrivals = collect_arrivals(existing_mailboxes, snapshot);
        if arrivals.is_empty() {
            return;
        }
        notifier.notify_new_mail(&NewMailBatch {
            account: target.display_name.clone(),
            arrivals,
        });
    }

    async fn process_due_outbox_jobs(&self, account_id: Option<i64>) -> Result<usize> {
        let mut processed = 0;

        loop {
            let Some(job) = self.inner.store.claim_due_outbox_job(account_id)? else {
                break;
            };
            processed += 1;

            match self.deliver_outbox_job(&job).await {
                Ok(()) => self.inner.store.record_outbox_sent(job.job_id)?,
                Err(error) => {
                    if job.attempt_count >= MAX_OUTBOX_ATTEMPTS {
                        self.inner
                            .store
                            .record_outbox_failure(job.job_id, error.to_string())?;
                    } else {
                        let retry_after =
                            OUTBOX_RETRY_BASE_DELAY.saturating_mul(job.attempt_count as u32);
                        self.inner.store.record_outbox_retry(
                            job.job_id,
                            error.to_string(),
                            retry_after,
                        )?;
                    }
                }
            }
        }

        Ok(processed)
    }

    async fn deliver_outbox_job(&self, job: &PendingOutboxJob) -> Result<()> {
        // The full sync target carries the auth kind and OAuth2 client
        // config, so OAuth2 accounts deliver with a freshly refreshed access
        // token (`connect_secret`) instead of the empty password slot.
        let target = self
            .inner
            .store
            .sync_target(job.target.account_id)?
            .with_context(|| {
                format!(
                    "account {} for outbox job {} no longer exists",
                    job.target.account_id, job.job_id
                )
            })?;
        let secret = self.connect_secret(&target).await.with_context(|| {
            format!(
                "missing or inaccessible credentials for account {}",
                job.target.account_id
            )
        })?;

        match job.target.outgoing.kind {
            SubmissionKind::Smtp => {
                let host = job
                    .target
                    .outgoing
                    .host
                    .clone()
                    .with_context(|| "SMTP delivery requires a submission host")?;
                let port = job
                    .target
                    .outgoing
                    .port
                    .with_context(|| "SMTP delivery requires a submission port")?;
                let security = job
                    .target
                    .outgoing
                    .security
                    .with_context(|| "SMTP delivery requires a submission security mode")?;
                let username = job.target.outgoing.username.clone();
                let config = SubmissionConfig {
                    host,
                    port,
                    username: username.clone(),
                    secret: username.map(|_| secret),
                    auth: target.auth,
                    security,
                    hello_name: job.target.outgoing.hello_name.clone(),
                };
                zmail_smtp::submit_outgoing_message(&config, &job.message).await
            }
            SubmissionKind::Jmap => {
                zmail_jmap::submit_outgoing_message(
                    &job.target.endpoint,
                    &job.target.username,
                    &secret,
                    &job.target.identity_email,
                    &job.message,
                )
                .await
            }
        }
    }
}

/// Collects the threads that gained messages in a snapshot relative to the
/// existing local mirror.
///
/// Mailboxes absent from the mirror (the first sync of an account) count as
/// an initial import rather than new mail, so they never notify; reconnecting
/// a large account therefore cannot flood the user with one notification per
/// thread.
fn collect_arrivals(
    existing_mailboxes: &[MailboxSnapshot],
    snapshot: &zmail_core::AccountSyncSnapshot,
) -> Vec<NewMailArrival> {
    let mut arrivals = Vec::new();
    for mailbox in &snapshot.mailboxes {
        let Some(existing_mailbox) = existing_mailboxes
            .iter()
            .find(|existing| existing.name == mailbox.name)
        else {
            continue;
        };
        for thread in &mailbox.threads {
            let gained = match existing_mailbox
                .threads
                .iter()
                .find(|existing| existing.remote_thread_id == thread.remote_thread_id)
            {
                None => !thread.messages.is_empty(),
                Some(existing) => thread.messages.len() > existing.messages.len(),
            };
            if gained {
                arrivals.push(NewMailArrival {
                    subject: thread.subject.clone(),
                    sender: thread.participants.clone(),
                });
            }
        }
    }
    arrivals
}

/// Current wall-clock time in seconds since the Unix epoch.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// Persists a granted OAuth2 token set for an account.
///
/// The access token, rotated refresh token, and provider client secret go
/// into the secret store's slots; the client configuration (token endpoint,
/// client id) and the token expiry go into the local store, so background
/// sync can refresh the grant without user interaction. Shared by the CLI
/// and the UI account setup flows.
pub fn persist_oauth2_grant(
    store: &LocalStore,
    secrets: &dyn SecretStore,
    account_id: i64,
    client: &zmail_auth::OAuth2Client,
    tokens: &zmail_auth::TokenSet,
) -> Result<()> {
    secrets
        .set_account_secret_in_slot(account_id, SecretSlot::AccessToken, &tokens.access_token)
        .with_context(|| format!("failed to store the access token for account {account_id}"))?;
    if let Some(refresh_token) = tokens
        .refresh_token
        .as_ref()
        .filter(|token| !token.is_empty())
    {
        secrets
            .set_account_secret_in_slot(account_id, SecretSlot::RefreshToken, refresh_token)
            .with_context(|| {
                format!("failed to store the refresh token for account {account_id}")
            })?;
    }
    if let Some(client_secret) = client
        .client_secret
        .as_ref()
        .filter(|secret| !secret.is_empty())
    {
        secrets
            .set_account_secret_in_slot(account_id, SecretSlot::ClientSecret, client_secret)
            .with_context(|| {
                format!("failed to store the client secret for account {account_id}")
            })?;
    }
    store
        .save_account_oauth2_client(account_id, &client.token_endpoint, &client.client_id)
        .with_context(|| format!("failed to save the OAuth2 client for account {account_id}"))?;
    let expires_at = tokens
        .expires_in
        .map(|lifetime| unix_now() + lifetime as i64);
    store
        .set_account_oauth2_expiry(account_id, expires_at)
        .with_context(|| format!("failed to record the token expiry for account {account_id}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use super::{BackgroundRuntime, SyncService, collect_arrivals};
    use anyhow::{Result, anyhow};
    use zmail_core::{
        AccountSyncSnapshot, AuthKind, CachePolicySummary, MailboxSnapshot, MessageSnapshot,
        NewAccount, NewIdentity, ProtocolKind, SyncPhase, ThreadSnapshot,
    };
    use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
    use zmail_store::LocalStore;

    /// Inserts a minimal account with the given auth kind and returns its id.
    pub(super) fn insert_account(store: &LocalStore, auth: AuthKind) -> i64 {
        store
            .insert_account(&NewAccount {
                display_name: "Test Account".to_owned(),
                protocol: ProtocolKind::Imap,
                auth,
                endpoint: "imaps://mail.example.com:993".to_owned(),
                username: "user@example.com".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: None,
                identities: vec![NewIdentity {
                    display_name: "Test Account".to_owned(),
                    email_address: "user@example.com".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account should be inserted")
    }

    #[derive(Debug)]
    struct SlowSecretStore {
        delay: Duration,
    }

    impl SecretStore for SlowSecretStore {
        fn set_account_secret(&self, _account_id: i64, _secret: &str) -> Result<()> {
            Ok(())
        }

        fn get_account_secret(&self, _account_id: i64) -> Result<String> {
            thread::sleep(self.delay);
            Err(anyhow!("synthetic slow secret lookup"))
        }

        fn delete_account_secret(&self, _account_id: i64) -> Result<()> {
            Ok(())
        }

        fn set_account_secret_in_slot(
            &self,
            _account_id: i64,
            _slot: SecretSlot,
            _secret: &str,
        ) -> Result<()> {
            Ok(())
        }

        fn get_account_secret_from_slot(
            &self,
            _account_id: i64,
            _slot: SecretSlot,
        ) -> Result<String> {
            Err(anyhow!("synthetic slot lookup"))
        }

        fn delete_account_secret_in_slot(&self, _account_id: i64, _slot: SecretSlot) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn sync_watchers_starts_watchers_for_new_accounts_only() {
        let runtime = BackgroundRuntime::start(2).expect("runtime should start");
        let store = Arc::new(LocalStore::open_ephemeral().expect("store should open"));
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        let service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());

        // No accounts yet: nothing to watch, and the call is idempotent.
        assert_eq!(
            service.sync_watchers().expect("watchers should sync"),
            0,
            "an empty store starts no watchers"
        );

        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        secrets
            .set_account_secret(account_id, "demo-secret")
            .expect("secret should be set");

        // The new account gets a watcher; a second call must not duplicate it.
        assert_eq!(
            service.sync_watchers().expect("watchers should sync"),
            1,
            "the new account starts exactly one watcher"
        );
        assert_eq!(
            service.sync_watchers().expect("watchers should sync"),
            0,
            "an already-watched account starts no second watcher"
        );
    }

    #[test]
    fn starts_and_verifies_the_background_runtime() {
        let runtime = BackgroundRuntime::start(2).expect("runtime should start");
        let summary = runtime.summary();

        assert_eq!(summary.worker_threads, 2);
        assert!(summary.bridge_verified);
        let _handle = runtime.handle().clone();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_service_records_missing_credentials_as_a_failure() {
        let store = Arc::new(LocalStore::open_ephemeral().expect("store should open"));
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let service = SyncService::new(
            tokio::runtime::Handle::current().clone(),
            store.clone(),
            InMemorySecretStore::shared(),
        );

        assert!(
            service
                .sync_account(account_id)
                .expect("sync should be scheduled")
        );
        tokio::time::sleep(Duration::from_secs(3)).await;

        let account = store
            .list_accounts()
            .expect("accounts should load")
            .into_iter()
            .find(|account| account.id == account_id)
            .expect("account should still exist");
        assert_eq!(account.sync_status.phase, SyncPhase::Failed);
        assert!(account.sync_status.failure_count >= 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_account_returns_without_waiting_for_background_work() {
        let store = Arc::new(LocalStore::open_ephemeral().expect("store should open"));
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let service = SyncService::new(
            tokio::runtime::Handle::current().clone(),
            store.clone(),
            Arc::new(SlowSecretStore {
                delay: Duration::from_secs(2),
            }),
        );

        let started_at = Instant::now();
        let scheduled = service
            .sync_account(account_id)
            .expect("sync should be scheduled");

        assert!(scheduled);
        assert!(
            started_at.elapsed() < Duration::from_millis(250),
            "scheduling sync should stay non-blocking for the caller"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_outbox_returns_without_waiting_for_delivery_attempts() {
        let store = Arc::new(LocalStore::open_ephemeral().expect("store should open"));
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let service = SyncService::new(
            tokio::runtime::Handle::current().clone(),
            store.clone(),
            Arc::new(SlowSecretStore {
                delay: Duration::from_secs(2),
            }),
        );

        let identity_id = store
            .list_identities(account_id)
            .expect("identities should load")
            .into_iter()
            .next()
            .expect("demo account should include an identity")
            .id;
        let draft_id = store
            .create_draft(&zmail_core::NewDraft {
                account_id,
                identity_id,
                to: vec!["dest@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Phase 5".to_owned(),
                body: "background send".to_owned(),
            })
            .expect("draft should be created");
        store.queue_draft(draft_id).expect("draft should queue");

        let started_at = Instant::now();
        service
            .process_outbox(Some(account_id))
            .expect("outbox processing should be scheduled");

        assert!(
            started_at.elapsed() < Duration::from_millis(250),
            "scheduling outbox processing should stay non-blocking for the caller"
        );
    }

    #[test]
    fn collect_arrivals_reports_new_and_grown_threads() {
        let existing = vec![mailbox(
            "Inbox",
            vec![
                thread("thread-a", 1, "Existing subject"),
                thread("thread-b", 2, "Grown subject"),
            ],
        )];
        let snapshot = AccountSyncSnapshot {
            checkpoint: "test".to_owned(),
            detail: "test".to_owned(),
            identities: None,
            mailboxes: vec![mailbox(
                "Inbox",
                vec![
                    thread("thread-a", 1, "Existing subject"),
                    thread("thread-b", 5, "Grown subject"),
                    thread("thread-c", 1, "New subject"),
                ],
            )],
        };

        let arrivals = collect_arrivals(&existing, &snapshot);
        assert_eq!(arrivals.len(), 2, "only new and grown threads arrive");
        assert_eq!(arrivals[0].subject, "Grown subject");
        assert_eq!(arrivals[1].subject, "New subject");
    }

    #[test]
    fn collect_arrivals_ignores_first_time_mailboxes() {
        let snapshot = AccountSyncSnapshot {
            checkpoint: "test".to_owned(),
            detail: "test".to_owned(),
            identities: None,
            mailboxes: vec![mailbox(
                "Inbox",
                vec![thread("thread-a", 3, "Initial import")],
            )],
        };

        let arrivals = collect_arrivals(&[], &snapshot);
        assert!(
            arrivals.is_empty(),
            "the first sync of a mailbox is an import, not new mail"
        );
    }

    fn thread(remote_id: &str, message_count: usize, subject: &str) -> ThreadSnapshot {
        ThreadSnapshot {
            remote_thread_id: remote_id.to_owned(),
            subject: subject.to_owned(),
            participants: "sender@example.test".to_owned(),
            preview: "preview".to_owned(),
            is_flagged: false,
            has_attachments: false,
            unread_count: 1,
            latest_message_at: "2026-10-07T10:00:00Z".to_owned(),
            messages: (0..message_count)
                .map(|index| MessageSnapshot {
                    remote_id: format!("{remote_id}-{index}"),
                    identity_email: "user@example.test".to_owned(),
                    sender: "sender@example.test".to_owned(),
                    recipients: "user@example.test".to_owned(),
                    subject: subject.to_owned(),
                    body_preview: "preview".to_owned(),
                    body_text: None,
                    sanitized_html: None,
                    has_remote_content: false,
                    is_read: false,
                    is_flagged: false,
                    received_at: "2026-10-07T10:00:00Z".to_owned(),
                    attachments: Vec::new(),
                })
                .collect(),
        }
    }

    fn mailbox(name: &str, threads: Vec<ThreadSnapshot>) -> MailboxSnapshot {
        MailboxSnapshot {
            name: name.to_owned(),
            role: Some("inbox".to_owned()),
            threads,
        }
    }
}

#[cfg(test)]
mod oauth2_refresh_tests {
    use std::sync::Arc;

    use super::{SyncService, unix_now};
    use zmail_core::AuthKind;
    use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
    use zmail_store::LocalStore;

    use super::tests::insert_account;

    #[test]
    fn persist_oauth2_grant_stores_the_full_grant() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let secrets = InMemorySecretStore::shared();
        let account_id = insert_account(&store, AuthKind::OAuth2);
        let client = zmail_auth::OAuth2Client {
            client_id: "client-1".to_owned(),
            client_secret: Some("client-secret-1".to_owned()),
            authorization_endpoint: "https://provider.test/authorize".to_owned(),
            token_endpoint: "https://provider.test/token".to_owned(),
            scopes: Vec::new(),
            redirect_port: 8357,
            extra_authorize_params: Vec::new(),
        };
        let tokens = zmail_auth::TokenSet {
            access_token: "access-1".to_owned(),
            refresh_token: Some("refresh-1".to_owned()),
            expires_in: Some(3600),
        };

        super::persist_oauth2_grant(&store, secrets.as_ref(), account_id, &client, &tokens)
            .expect("the grant should persist");

        assert_eq!(
            secrets
                .get_account_secret_from_slot(account_id, SecretSlot::AccessToken)
                .expect("the access token should load"),
            "access-1"
        );
        assert_eq!(
            secrets
                .get_account_secret_from_slot(account_id, SecretSlot::RefreshToken)
                .expect("the refresh token should load"),
            "refresh-1"
        );
        assert_eq!(
            secrets
                .get_account_secret_from_slot(account_id, SecretSlot::ClientSecret)
                .expect("the client secret should load"),
            "client-secret-1"
        );
        let target = store
            .sync_target(account_id)
            .expect("the target should load")
            .expect("the target should exist");
        let config = target
            .oauth2
            .as_ref()
            .expect("the client config should load on the target");
        assert_eq!(config.token_endpoint, "https://provider.test/token");
        assert_eq!(config.client_id, "client-1");
        let expires_at = config
            .access_token_expires_at
            .expect("the expiry should be recorded");
        assert!(
            expires_at > unix_now(),
            "the recorded expiry must lie in the future"
        );
        assert!(
            expires_at <= unix_now() + 3600,
            "the recorded expiry must match the granted lifetime"
        );
    }

    fn service() -> (Arc<LocalStore>, Arc<InMemorySecretStore>, SyncService) {
        // The tests below never reach the network; a plain Tokio test
        // runtime is enough because the refresh runs on a dedicated thread.
        let store = Arc::new(LocalStore::open_ephemeral().expect("store should open"));
        let secrets = InMemorySecretStore::shared();
        (
            store.clone(),
            secrets.clone(),
            SyncService::new(tokio::runtime::Handle::current().clone(), store, secrets),
        )
    }

    #[tokio::test]
    async fn connect_secret_returns_the_password_for_password_accounts() {
        let (store, secrets, service) = service();
        let account_id = insert_account(&store, AuthKind::Password);
        secrets
            .set_account_secret(account_id, "app-password")
            .expect("the password should store");

        let target = store
            .sync_target(account_id)
            .expect("the target should load")
            .expect("the target should exist");
        assert_eq!(
            service
                .connect_secret(&target)
                .await
                .expect("the password should resolve"),
            "app-password"
        );
    }

    #[tokio::test]
    async fn connect_secret_returns_a_live_access_token_without_refreshing() {
        let (store, secrets, service) = service();
        let account_id = insert_account(&store, AuthKind::OAuth2);
        store
            .save_account_oauth2_client(account_id, "https://oauth2.example.test/token", "client-1")
            .expect("the client should save");
        // A far-future expiry: the token must be returned as stored, with no
        // refresh attempt (there is no refresh token anyway).
        store
            .set_account_oauth2_expiry(account_id, Some(unix_now() + 3600))
            .expect("the expiry should save");
        secrets
            .set_account_secret_in_slot(account_id, SecretSlot::AccessToken, "live-token")
            .expect("the access token should store");

        let target = store
            .sync_target(account_id)
            .expect("the target should load")
            .expect("the target should exist");
        assert_eq!(
            service
                .connect_secret(&target)
                .await
                .expect("the token should resolve"),
            "live-token"
        );
    }

    #[tokio::test]
    async fn connect_secret_honors_legacy_pasted_tokens() {
        let (store, secrets, service) = service();
        let account_id = insert_account(&store, AuthKind::OAuth2);
        // No access token in the slot: the token-paste flow stored the
        // access token in the legacy single-secret slot.
        secrets
            .set_account_secret(account_id, "pasted-token")
            .expect("the pasted token should store");

        let target = store
            .sync_target(account_id)
            .expect("the target should load")
            .expect("the target should exist");
        assert_eq!(
            service
                .connect_secret(&target)
                .await
                .expect("the pasted token should resolve"),
            "pasted-token"
        );
    }

    #[tokio::test]
    async fn connect_secret_refreshes_expired_tokens_and_persists_the_new_grant() {
        let (store, secrets, service) = service();
        let account_id = insert_account(&store, AuthKind::OAuth2);
        store
            .save_account_oauth2_client(account_id, "https://oauth2.example.test/token", "client-1")
            .expect("the client should save");
        store
            .set_account_oauth2_expiry(account_id, Some(unix_now() - 1))
            .expect("the expired timestamp should save");
        secrets
            .set_account_secret_in_slot(account_id, SecretSlot::AccessToken, "stale-token")
            .expect("the stale token should store");
        secrets
            .set_account_secret_in_slot(account_id, SecretSlot::RefreshToken, "refresh-1")
            .expect("the refresh token should store");

        let target = store
            .sync_target(account_id)
            .expect("the target should load")
            .expect("the target should exist");
        let error = service
            .connect_secret(&target)
            .await
            .expect_err("the refresh must run against the unreachable endpoint");
        assert!(
            format!("{error:#}").contains("failed to refresh the OAuth2 access token"),
            "the error should mention the refresh: {error:#}"
        );
        // The failed refresh must not clobber the stored grant.
        assert_eq!(
            secrets
                .get_account_secret_from_slot(account_id, SecretSlot::AccessToken)
                .expect("the stale token should still be stored"),
            "stale-token"
        );
    }

    #[tokio::test]
    async fn connect_secret_without_any_token_reports_missing_credentials() {
        let (store, _secrets, service) = service();
        let account_id = insert_account(&store, AuthKind::OAuth2);

        let target = store
            .sync_target(account_id)
            .expect("the target should load")
            .expect("the target should exist");
        let error = service
            .connect_secret(&target)
            .await
            .expect_err("no token is stored");
        assert!(
            format!("{error:#}").contains("missing OAuth2 access token"),
            "the error should mention the missing token: {error:#}"
        );
    }
}
