//! Proton Mail accounts in the sync runtime (ADR 0004).
//!
//! A Proton account connects with a live session rather than a stored
//! secret: [`SyncService::connect`] hands the adapters a [`Credentials`].
//! The Proton half only exists with the `proton` feature, which `zmail-app`
//! does not enable until the local store is encrypted at rest (#139);
//! without it a Proton account fails to connect with a clear message and
//! is never watched.
//!
//! A Proton sync writes in two steps. First the account's events since the
//! checkpoint (or, the first time, its newest mail) replace the mirror, as
//! other protocols' snapshots do. Then, while the checkpoint has a backfill
//! cursor, the next stretch of older mail is merged in without removing
//! anything and without new-mail notifications. Each step stores its
//! checkpoint, so a failure resumes where it stopped, and the next sync is
//! scheduled [`BACKFILL_PAUSE`] later until the history is complete;
//! write-back, sending and events run between the stretches.

use std::time::Duration;

#[cfg(not(feature = "proton"))]
use anyhow::bail;
use anyhow::{Context, Result};
use zmail_core::{OutgoingMessage, ProtocolKind, SyncTarget};

use crate::{SyncService, backoff::Backoff, wake::unless_woken};

/// The pause between two syncs that fetch older Proton mail.
#[cfg(feature = "proton")]
const BACKFILL_PAUSE: Duration = Duration::from_millis(1500);

/// What a protocol adapter connects with.
pub(crate) enum Credentials {
    /// A password or OAuth2 access token (IMAP and JMAP).
    Secret(String),
    /// A resumed Proton session.
    #[cfg(feature = "proton")]
    Proton(std::sync::Arc<zmail_proton::ProtonSession>),
}

/// The client configuration for Proton's production API: Bridge's TLS pins
/// and Zmail's identity: Bridge's `x-pm-appversion` and a User-Agent
/// naming Zmail (ADR 0004).
#[cfg(feature = "proton")]
pub fn proton_production_config(version: &str) -> Result<zmail_proton::ClientConfig> {
    let app_version = zmail_proton::AppVersion::zmail(version)?;
    Ok(zmail_proton::ClientConfig::production(app_version)?)
}

impl SyncService {
    /// Resolves what the account's adapter connects with.
    pub(crate) async fn connect(&self, target: &SyncTarget) -> Result<Credentials> {
        match target.protocol {
            ProtocolKind::Proton => self.connect_proton(target).await,
            ProtocolKind::Imap | ProtocolKind::Jmap => {
                let secret = self.connect_secret(target).await.with_context(|| {
                    format!(
                        "missing or inaccessible credentials for account {}",
                        target.id
                    )
                })?;
                Ok(Credentials::Secret(secret))
            }
        }
    }

    /// Sets up Proton support: the API client configuration (the
    /// production host and pins, or the test harness) used for every
    /// Proton account's session.
    #[cfg(feature = "proton")]
    pub fn configure_proton(&self, config: zmail_proton::ClientConfig) -> Result<()> {
        let sessions = zmail_proton::ProtonSessions::new(config, self.inner.secrets.clone())?;
        *self
            .inner
            .proton
            .lock()
            .expect("proton session mutex should not be poisoned") =
            Some(std::sync::Arc::new(sessions));
        Ok(())
    }

    /// Sets how much of the message listing each Proton sync does. Zmail
    /// keeps Bridge's page size and its own first-sync and backfill page
    /// counts ([`zmail_proton::SyncPaging::default`]); tests use small
    /// pages so a handful of messages spans several stretches.
    #[cfg(feature = "proton")]
    pub fn set_proton_paging(&self, paging: zmail_proton::SyncPaging) {
        *self
            .inner
            .proton_paging
            .lock()
            .expect("proton paging mutex should not be poisoned") = paging;
    }

    /// Syncs a Proton account's mail and returns the status detail and the
    /// checkpoint to record: the events become changes to just the messages
    /// they name (or a first sync replaces the mirror), then the next
    /// stretch of older mail is merged in.
    #[cfg(feature = "proton")]
    pub(crate) async fn sync_proton_mail(
        &self,
        target: &SyncTarget,
        session: &zmail_proton::ProtonSession,
    ) -> Result<(String, String)> {
        use std::collections::HashSet;

        use zmail_core::SyncPhase;

        let account_id = target.id;
        let store = &self.inner.store;
        let paging = *self
            .inner
            .proton_paging
            .lock()
            .expect("proton paging mutex should not be poisoned");
        let mailboxes: HashSet<String> = store
            .list_mailboxes(account_id)?
            .into_iter()
            .map(|mailbox| mailbox.name)
            .collect();
        let progress = |detail: &str| {
            // Progress is only shown; a failed write must not stop the sync.
            let _ = store.record_sync_progress(account_id, detail);
        };

        let update = zmail_proton::discover_and_sync(
            target,
            session,
            !mailboxes.is_empty(),
            &paging,
            &progress,
        )
        .await?;
        match &update {
            zmail_proton::MirrorUpdate::Replace(snapshot) => {
                // A resync diffs against the whole mirror for its new-mail
                // notifications; a first sync has nothing to diff against.
                let existing = if mailboxes.is_empty() {
                    Vec::new()
                } else {
                    self.local_mirror(account_id)?
                };
                self.write_snapshot(target, &existing, snapshot)?;
                self.inner.index_wake.notify_one();
            }
            zmail_proton::MirrorUpdate::Apply { delta, created, .. } => {
                store.begin_sync_attempt(
                    account_id,
                    SyncPhase::Syncing,
                    "Writing Proton message changes into the local mirror",
                )?;
                let unknown: HashSet<String> = store
                    .apply_mail_delta(account_id, delta)?
                    .into_iter()
                    .collect();
                // New mail is what the events created and the mirror did not
                // hold; an update placing older mail the backfill has not
                // reached is not.
                let arrived: HashSet<&str> = created
                    .iter()
                    .filter(|id| unknown.contains(*id))
                    .map(String::as_str)
                    .collect();
                self.notify_arrivals(
                    target,
                    crate::collect_delta_arrivals(&mailboxes, delta, &arrived),
                );
                self.inner.index_wake.notify_one();
            }
            zmail_proton::MirrorUpdate::Unchanged { .. } => {}
        }
        store.record_sync_checkpoint(account_id, update.checkpoint())?;
        let mut detail = update.detail().to_owned();
        let mut checkpoint = update.checkpoint().to_owned();

        let backfill =
            zmail_proton::backfill_older_mail(session, &checkpoint, &paging, &progress).await?;
        if let Some(backfill) = backfill {
            store.begin_sync_attempt(account_id, SyncPhase::Syncing, &backfill.snapshot.detail)?;
            // Older mail never notifies: it is history, not new mail.
            store.merge_account_snapshot(account_id, &backfill.snapshot)?;
            store.record_sync_checkpoint(account_id, &backfill.snapshot.checkpoint)?;
            self.inner.index_wake.notify_one();
            checkpoint = backfill.snapshot.checkpoint;
            if !backfill.complete {
                detail = backfill.snapshot.detail;
                let service = self.clone();
                self.inner.handle.spawn(async move {
                    tokio::time::sleep(BACKFILL_PAUSE).await;
                    service.request_sync(account_id);
                });
            }
        }
        Ok((detail, checkpoint))
    }

    /// Starts signing in to a Proton account for onboarding, on the
    /// shared Proton connection. Drive its steps with [`Self::run_proton`].
    #[cfg(feature = "proton")]
    pub fn begin_proton_sign_in(
        &self,
        username: &str,
        password: &str,
    ) -> Result<zmail_proton::ProtonSignIn> {
        let sessions = self
            .proton_sessions()
            .context("Proton support has not been configured")?;
        Ok(zmail_proton::ProtonSignIn::new(
            sessions.manager().clone(),
            username,
            password,
        ))
    }

    /// Runs a Proton sign-in step on the sync runtime. The returned future
    /// can be awaited from any executor (the UI's).
    #[cfg(feature = "proton")]
    pub fn run_proton<F>(
        &self,
        future: F,
    ) -> impl std::future::Future<Output = Result<F::Output>> + use<F>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let task = self.inner.handle.spawn(future);
        async move { task.await.context("the Proton sign-in task stopped") }
    }

    #[cfg(feature = "proton")]
    fn proton_sessions(&self) -> Option<std::sync::Arc<zmail_proton::ProtonSessions>> {
        self.inner
            .proton
            .lock()
            .expect("proton session mutex should not be poisoned")
            .clone()
    }

    #[cfg(feature = "proton")]
    async fn connect_proton(&self, target: &SyncTarget) -> Result<Credentials> {
        let sessions = self
            .proton_sessions()
            .context("Proton support has not been configured")?;
        Ok(Credentials::Proton(sessions.session(target.id).await?))
    }

    #[cfg(not(feature = "proton"))]
    async fn connect_proton(&self, _target: &SyncTarget) -> Result<Credentials> {
        bail!("this build of Zmail has no Proton Mail support")
    }

    /// Delivers an outbox message through the account's Proton session
    /// (Bridge's SMTP send path). The outbox's undo window and retries
    /// apply as for SMTP and JMAP: this runs when the job is claimed.
    pub(crate) async fn deliver_proton(
        &self,
        target: &SyncTarget,
        message: &OutgoingMessage,
    ) -> Result<()> {
        if target.protocol != ProtocolKind::Proton {
            anyhow::bail!("Proton submission needs a Proton account");
        }
        #[cfg(feature = "proton")]
        {
            let Credentials::Proton(session) = self.connect_proton(target).await? else {
                anyhow::bail!("Proton accounts need a Proton session");
            };
            let trusted = self.inner.store.trusted_recipient_keys(target.id)?;
            zmail_proton::submit_outgoing_message(&session, message, &trusted).await
        }
        #[cfg(not(feature = "proton"))]
        {
            let _ = message;
            self.connect_proton(target).await.map(|_| ())
        }
    }

    /// Whether a Proton account has a session to resume (and this build
    /// can resume it).
    pub(crate) fn has_proton_session(&self, account_id: i64) -> bool {
        #[cfg(feature = "proton")]
        {
            self.proton_sessions().is_some()
                && zmail_proton::has_session(self.inner.secrets.as_ref(), account_id)
        }
        #[cfg(not(feature = "proton"))]
        {
            let _ = account_id;
            false
        }
    }

    /// Drops a removed account's Proton session.
    pub(crate) async fn forget_proton_session(&self, account_id: i64) {
        #[cfg(feature = "proton")]
        if let Some(sessions) = self.proton_sessions() {
            sessions.forget(account_id).await;
        }
        #[cfg(not(feature = "proton"))]
        let _ = account_id;
    }

    /// Watches a Proton account until the next reconnect: Bridge's event
    /// loop. Every 20 s plus up to 20 s of jitter it polls the event stream
    /// from the stored checkpoint, and syncs when something changed (the
    /// sync applies the events). A wake signal syncs at once.
    pub(crate) async fn watch_proton(
        &self,
        target: &SyncTarget,
        poll_interval: Duration,
        backoff: &mut Backoff,
        wake: &mut tokio::sync::watch::Receiver<u64>,
    ) {
        #[cfg(feature = "proton")]
        {
            let _ = poll_interval;
            if unless_woken(wake, tokio::time::sleep(zmail_proton::event_poll_delay()))
                .await
                .is_none()
            {
                backoff.reset();
                self.request_sync(target.id);
                return;
            }
            let checkpoint = self
                .inner
                .store
                .sync_target(target.id)
                .ok()
                .flatten()
                .and_then(|target| target.checkpoint)
                .filter(|checkpoint| !checkpoint.is_empty());
            let Some(checkpoint) = checkpoint else {
                // Never synced: the first sync sets the checkpoint.
                self.request_sync(target.id);
                return;
            };
            if zmail_proton::ProtonCheckpoint::parse(&checkpoint)
                .is_some_and(|checkpoint| checkpoint.backfill_pending())
            {
                // Older mail is still to come. Each sync schedules the next
                // stretch; this carries on after one that failed.
                backoff.reset();
                self.request_sync(target.id);
                return;
            }
            let changed = match self.connect_proton(target).await {
                Ok(Credentials::Proton(session)) => {
                    zmail_proton::has_new_events(&session, &checkpoint).await
                }
                Ok(Credentials::Secret(_)) => Ok(false),
                Err(error) => Err(error),
            };
            match changed {
                Ok(true) => {
                    backoff.reset();
                    self.request_sync(target.id);
                }
                Ok(false) => backoff.reset(),
                Err(_) => {
                    let _ = unless_woken(wake, tokio::time::sleep(backoff.next_delay())).await;
                }
            }
        }
        #[cfg(not(feature = "proton"))]
        {
            // Without Proton support no Proton account is watched; this
            // only keeps the loop from spinning if one ever is.
            let _ = (target, backoff);
            let _ = unless_woken(wake, tokio::time::sleep(poll_interval)).await;
        }
    }
}
