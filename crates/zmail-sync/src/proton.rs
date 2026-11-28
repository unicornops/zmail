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
//! checkpoint are applied in place: changed messages, renamed, added and
//! removed mailboxes, and the identities (the first time, its newest mail
//! replaces the empty mirror, as other protocols' snapshots do). Then,
//! while the checkpoint has a backfill cursor, the next stretch of older
//! mail is merged in without removing anything and without new-mail
//! notifications; after a refresh request the stretch is written as a
//! delta instead, so each listed message lands where Proton has it. Each step stores its
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

/// The account's saved key records a Proton send starts from: the keys the
/// user trusted and the pins remembered from verified contact cards.
#[cfg(feature = "proton")]
pub(crate) fn recipient_keys(
    store: &zmail_store::LocalStore,
    account_id: i64,
) -> Result<zmail_core::key_change::RecipientKeys> {
    Ok(zmail_core::key_change::RecipientKeys::new(
        store.trusted_recipient_keys(account_id)?,
        store.remembered_recipient_pins(account_id)?,
    ))
}

/// Saves what a Proton send saw on verified contact cards (ADR 0004,
/// "Contact cards that go missing"), whether the send went or stopped,
/// and hands back how the send went. A send that went is never reported
/// failed for a record that could not be saved, which would send it
/// again; a send that failed keeps its own error.
#[cfg(feature = "proton")]
pub(crate) fn save_noted_pins(
    store: &zmail_store::LocalStore,
    account_id: i64,
    sent: Result<()>,
    keys: &zmail_core::key_change::RecipientKeys,
) -> Result<()> {
    for pins in keys.take_noted() {
        // A record that cannot be saved now is noted again by the next
        // send that uses the card.
        let _ = store.remember_recipient_pins(account_id, &pins);
    }
    sent
}

/// What a protocol adapter connects with.
pub(crate) enum Credentials {
    /// A password or OAuth2 access token (IMAP and JMAP), wiped when
    /// dropped.
    Secret(zeroize::Zeroizing<String>),
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

        let update =
            zmail_proton::discover_and_sync(target, session, &mailboxes, &paging, &progress)
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
                    "Writing Proton changes into the local mirror",
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
            match backfill.relist_delta() {
                Some(delta) => {
                    store.apply_mail_delta(account_id, &delta)?;
                }
                None => store.merge_account_snapshot(account_id, &backfill.snapshot)?,
            }
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
        // A refresh token the keychain refused is said in the account's
        // status, so the user knows why the next start would ask to sign
        // in again. The sync itself worked, and sending must carry on, so
        // this is its report rather than a failure.
        if let Err(error) = session.save_refresh_token() {
            detail = format!("Synced, but {error:#}");
        }
        // Contacts whose card did not verify when mail was sent to them
        // (the mail went) stay in the report until a later send finds them
        // in order: the report is what both frontends show without anything
        // being marked failed.
        if let Some(warning) = session.contact_warning() {
            detail = format!("{}. {warning}", detail.trim_end_matches('.'));
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
            let keys = recipient_keys(&self.inner.store, target.id)?;
            let sent = zmail_proton::submit_outgoing_message(&session, message, &keys).await;
            save_noted_pins(&self.inner.store, target.id, sent, &keys)
        }
        #[cfg(not(feature = "proton"))]
        {
            let _ = message;
            self.connect_proton(target).await.map(|_| ())
        }
    }

    /// Looks for the message an interrupted send was sending on a Proton
    /// account, by its Message-ID (see [`crate::interrupted`]).
    pub(crate) async fn find_interrupted_proton_send(
        &self,
        target: &SyncTarget,
        message_id: &str,
    ) -> Result<crate::interrupted::InterruptedSend> {
        #[cfg(feature = "proton")]
        {
            use crate::interrupted::{InterruptedSend, ResumeSend};

            let Credentials::Proton(session) = self.connect_proton(target).await? else {
                anyhow::bail!("Proton accounts need a Proton session");
            };
            Ok(
                match zmail_proton::find_outgoing_message(&session, message_id).await? {
                    zmail_proton::OutgoingTrace::Sent => InterruptedSend::Sent,
                    zmail_proton::OutgoingTrace::Draft(draft_id) => {
                        InterruptedSend::Resume(ResumeSend::ProtonDraft(draft_id))
                    }
                    zmail_proton::OutgoingTrace::None => InterruptedSend::NotSent,
                },
            )
        }
        #[cfg(not(feature = "proton"))]
        {
            let _ = message_id;
            self.connect_proton(target).await?;
            anyhow::bail!("this build of Zmail has no Proton Mail support")
        }
    }

    /// Sends the draft an interrupted send left on a Proton account, as
    /// [`Self::deliver_proton`] would have sent the message.
    pub(crate) async fn resume_proton_send(
        &self,
        target: &SyncTarget,
        draft_id: &str,
        message: &OutgoingMessage,
    ) -> Result<()> {
        #[cfg(feature = "proton")]
        {
            let Credentials::Proton(session) = self.connect_proton(target).await? else {
                anyhow::bail!("Proton accounts need a Proton session");
            };
            let keys = recipient_keys(&self.inner.store, target.id)?;
            let sent = zmail_proton::send_existing_draft(&session, draft_id, message, &keys).await;
            save_noted_pins(&self.inner.store, target.id, sent, &keys)
        }
        #[cfg(not(feature = "proton"))]
        {
            let _ = (draft_id, message);
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

    /// The message of a Proton send that stopped because the recipient's
    /// contact card could not be verified
    /// ([`zmail_proton::UnverifiedContactCard`]), if `error` is one. Only
    /// the user can fix that, in the Proton Mail web app, so the outbox
    /// fails the message at once with these words instead of retrying.
    pub(crate) fn unverified_contact_card(error: &anyhow::Error) -> Option<String> {
        #[cfg(feature = "proton")]
        {
            error
                .chain()
                .find_map(|cause| cause.downcast_ref::<zmail_proton::UnverifiedContactCard>())
                .map(ToString::to_string)
        }
        #[cfg(not(feature = "proton"))]
        {
            let _ = error;
            None
        }
    }

    /// Signs a removed Proton account's session out on Proton's side, in
    /// the background and best effort, so its refresh token stops working
    /// wherever a copy may be (the keychain entry that could not be
    /// deleted, a backup). `uid` and `refresh_token` are the stored
    /// session, read before the account's keychain entries were deleted;
    /// the session Zmail has open is used when there is one. Never waits
    /// for the network: removing an account works offline.
    pub fn sign_out_proton_session(
        &self,
        account_id: i64,
        uid: Option<String>,
        refresh_token: Option<zeroize::Zeroizing<String>>,
    ) {
        #[cfg(feature = "proton")]
        if let Some(sessions) = self.proton_sessions() {
            let stored = uid
                .zip(refresh_token)
                .map(|(uid, refresh_token)| zmail_proton::StoredSession::new(uid, refresh_token));
            self.inner.handle.spawn(async move {
                sessions.sign_out(account_id, stored).await;
            });
        }
        #[cfg(not(feature = "proton"))]
        let _ = (account_id, uid, refresh_token);
    }

    /// Replaces a Proton account's session with a new sign-in (Sign In
    /// Again), while the old session may still be in use: `store` keeps
    /// the new session in the account's keychain slots
    /// (`zmail_setup::proton::store_signed_in_again`).
    ///
    /// Under the Proton sessions lock, the account's open session is
    /// dropped and stops writing refreshed tokens to the keychain, then it
    /// is signed out on Proton's side as [`Self::sign_out_proton_session`]
    /// does (best effort: the open session, else the stored one), and only
    /// then does `store` run. No session of the account is resumed until
    /// it has finished, so the keychain ends up holding the new UID and
    /// refresh token and nothing of the old session, even when the old one
    /// refreshes during the swap ([`zmail_proton::ProtonSessions::replace`]).
    #[cfg(feature = "proton")]
    pub fn replace_proton_session<F>(
        &self,
        account_id: i64,
        store: F,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static + use<F>
    where
        F: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let sessions = self.proton_sessions();
        async move {
            sessions
                .context("Proton support has not been configured")?
                .replace(account_id, store)
                .await
        }
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

#[cfg(all(test, feature = "proton"))]
mod tests {
    use std::sync::{Arc, Condvar, Mutex};

    use anyhow::Result;
    use zeroize::Zeroizing;
    use zmail_proton_api::{
        Auth, SaltedKeyPass,
        mock::{MockServer, Recorded, Reply},
    };
    use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
    use zmail_store::LocalStore;

    use crate::SyncService;

    /// A send stopped by an unverifiable contact card is recognised
    /// through the send error's context, with the words the outbox shows.
    #[test]
    fn an_unverified_contact_card_is_found_through_the_send_error_context() {
        let error = anyhow::Error::new(zmail_proton::UnverifiedContactCard {
            recipient: "bob@example.org".to_owned(),
            reason: "signature verification failed".to_owned(),
        })
        .context("failed to get recipients")
        .context("failed to send the message through Proton");
        let message = SyncService::unverified_contact_card(&error).expect("found");
        assert!(message.starts_with("Not sent: the Proton contact for bob@example.org"));
        assert!(message.contains("Proton Mail web app"), "{message}");
        assert!(!message.contains("signature verification failed"));
        assert!(SyncService::unverified_contact_card(&anyhow::anyhow!("other")).is_none());
    }

    /// A Proton send reads the account's saved keys, and what it saw on
    /// verified contact cards is saved for the next one, failed or not.
    #[test]
    fn a_send_reads_the_saved_keys_and_saves_the_pins_it_saw() {
        use zmail_core::{
            ProtocolKind,
            key_change::{RecipientKeys, RecipientPins},
        };
        use zmail_store::LocalStore;

        const PINNED: &str = "0123456789ABCDEF0123456789ABCDEF01234567";
        const SERVED: &str = "89ABCDEF0123456789ABCDEF0123456789ABCDEF";
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Proton)
            .expect("account");
        let other = store
            .create_demo_account(ProtocolKind::Proton)
            .expect("account");
        store
            .trust_recipient_key(account_id, "bob@example.org", SERVED)
            .expect("trusted");
        store
            .remember_recipient_pins(
                account_id,
                &RecipientPins::new("bob@example.org", [PINNED.to_owned()], false),
            )
            .expect("remembered");

        let keys: RecipientKeys = super::recipient_keys(&store, account_id).expect("keys");
        assert_eq!(keys.trusted.len(), 1);
        assert_eq!(
            keys.pins_for("Bob@example.org").map(|pins| pins.encrypt),
            Some(false)
        );
        assert!(
            super::recipient_keys(&store, other)
                .expect("keys")
                .pins
                .is_empty()
        );

        keys.note(RecipientPins::new(
            "bob@example.org",
            [SERVED.to_owned()],
            true,
        ));
        keys.note(RecipientPins::new("alice@example.org", [], true));
        super::save_noted_pins(&store, account_id, Ok(()), &keys).expect("saved");
        assert!(keys.take_noted().is_empty(), "saved once");
        assert_eq!(
            store.remembered_recipient_pins(account_id).expect("pins"),
            [
                RecipientPins::new("alice@example.org", [], true),
                RecipientPins::new("bob@example.org", [SERVED.to_owned()], true),
            ]
        );
        assert!(
            store
                .remembered_recipient_pins(other)
                .expect("pins")
                .is_empty()
        );
    }

    // Issue #464: signing in to a Proton account again while its session
    // is still in use. The old session may refresh at any moment, and its
    // auth handler writes each new refresh token to the account's keychain
    // slot, so the swap must leave the new session alone in the keychain.

    const ACCOUNT: i64 = 7;
    /// The access token the old session holds once resumed. Requests with
    /// it are refused, so the old session refreshes on its next request.
    const STALE_ACCESS: &str = "old-access-2";

    /// A gate one thread opens and others wait at.
    #[derive(Clone, Default)]
    struct Latch(Arc<(Mutex<bool>, Condvar)>);

    impl Latch {
        fn open(&self) {
            let (open, changed) = &*self.0;
            *open.lock().expect("latch") = true;
            changed.notify_all();
        }

        /// Blocks until the latch opens. On a runtime worker (the mock
        /// server and the keychain wait there) the worker's other tasks
        /// are handed on first, so they do not wait with it.
        fn wait(&self) {
            tokio::task::block_in_place(|| {
                let (open, changed) = &*self.0;
                let mut guard = open.lock().expect("latch");
                while !*guard {
                    guard = changed.wait(guard).expect("latch");
                }
            });
        }

        /// Waits off the async workers.
        async fn reached(&self) {
            let latch = self.clone();
            tokio::task::spawn_blocking(move || latch.wait())
                .await
                .expect("the wait finished");
        }
    }

    /// A keychain that can hold one write of a refresh token half done.
    #[derive(Default)]
    struct Keychain {
        inner: InMemorySecretStore,
        /// The refresh token whose write is held, if any.
        held: Option<&'static str>,
        /// Opened when the held write starts.
        held_started: Latch,
        /// The held write goes on once this opens.
        release: Latch,
        /// Opened when the held write is done.
        held_done: Latch,
    }

    impl Keychain {
        fn read(&self, slot: SecretSlot) -> Option<String> {
            self.inner
                .get_account_secret_from_slot(ACCOUNT, slot)
                .ok()
                .map(|secret| secret.as_str().to_owned())
        }
    }

    impl SecretStore for Keychain {
        fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()> {
            self.inner.set_account_secret(account_id, secret)
        }

        fn get_account_secret(&self, account_id: i64) -> Result<Zeroizing<String>> {
            self.inner.get_account_secret(account_id)
        }

        fn delete_account_secret(&self, account_id: i64) -> Result<()> {
            self.inner.delete_account_secret(account_id)
        }

        fn set_account_secret_in_slot(
            &self,
            account_id: i64,
            slot: SecretSlot,
            secret: &str,
        ) -> Result<()> {
            if slot == SecretSlot::ProtonRefreshToken && self.held == Some(secret) {
                self.held_started.open();
                self.release.wait();
                self.inner
                    .set_account_secret_in_slot(account_id, slot, secret)?;
                self.held_done.open();
                return Ok(());
            }
            if self.held.is_some() && slot == SecretSlot::ProtonKeyPass && secret == "new-key-pass"
            {
                // The new session's last write: a held write still under
                // way finishes first, as it would if nothing waited for it.
                self.release.open();
                self.held_done.wait();
            }
            self.inner
                .set_account_secret_in_slot(account_id, slot, secret)
        }

        fn get_account_secret_from_slot(
            &self,
            account_id: i64,
            slot: SecretSlot,
        ) -> Result<Zeroizing<String>> {
            self.inner.get_account_secret_from_slot(account_id, slot)
        }

        fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
            self.inner.delete_account_secret_in_slot(account_id, slot)
        }
    }

    fn tokens(uid: &str, access: &str, refresh: &str) -> Reply {
        Reply::json(
            200,
            serde_json::json!({"Code": 1000, "UID": uid, "AccessToken": access, "RefreshToken": refresh, "Scope": "full mail"}),
        )
    }

    /// Proton for the old and new sessions: each refresh token is good
    /// once; `refresh` answers the old session's second refresh.
    fn proton(request: &Recorded, refresh: impl Fn() -> Reply) -> Reply {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/auth/v4/refresh") => match request.json()["RefreshToken"].as_str() {
                Some("old-1") => tokens("uid-old", STALE_ACCESS, "old-2"),
                Some("old-2") => refresh(),
                Some("new-1") => tokens("uid-new", "new-access-2", "new-2"),
                _ => Reply::json(
                    422,
                    serde_json::json!({"Code": 10013, "Error": "Invalid refresh token"}),
                ),
            },
            ("DELETE", "/auth/v4") => Reply::json(200, serde_json::json!({"Code": 1000})),
            _ if request.header("authorization") == Some(&format!("Bearer {STALE_ACCESS}")) => {
                Reply::json(
                    401,
                    serde_json::json!({"Code": 401, "Error": "Invalid access token"}),
                )
            }
            _ => Reply::json(
                200,
                serde_json::json!({"Code": 1000, "User": {"ID": "user-1", "Name": "rob", "Email": "rob@proton.test"}}),
            ),
        }
    }

    /// A sync service with Proton on `server`, and the old session
    /// stored for the account.
    fn service(
        runtime: &tokio::runtime::Runtime,
        server: &MockServer,
        keychain: &Arc<Keychain>,
    ) -> SyncService {
        let store = Arc::new(LocalStore::open_ephemeral().expect("store"));
        let service = SyncService::new(runtime.handle().clone(), store, keychain.clone());
        service
            .configure_proton(zmail_proton_api::mock::config(&server.url, None))
            .expect("Proton");
        for (slot, secret) in [
            (SecretSlot::ProtonUid, "uid-old"),
            (SecretSlot::ProtonRefreshToken, "old-1"),
            (SecretSlot::ProtonKeyPass, "old-key-pass"),
        ] {
            keychain
                .inner
                .set_account_secret_in_slot(ACCOUNT, slot, secret)
                .expect("stored");
        }
        service
    }

    /// What Sign In Again stores: the new session, as
    /// `ProtonSignIn::store` writes it.
    fn store_new_session(
        keychain: &Arc<Keychain>,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let keychain = keychain.clone();
        async move {
            let auth = Auth {
                uid: "uid-new".to_owned(),
                access_token: "new-access-1".to_owned().into(),
                refresh_token: "new-1".to_owned().into(),
                ..Auth::default()
            };
            zmail_proton::store_new_session(
                keychain.as_ref(),
                ACCOUNT,
                &auth,
                &SaltedKeyPass::from_secret("new-key-pass"),
            )
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        // The mock server and the keychain hold threads while they wait.
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime")
    }

    fn assert_only_the_new_session_is_kept(keychain: &Keychain) {
        assert_eq!(
            keychain.read(SecretSlot::ProtonUid).as_deref(),
            Some("uid-new")
        );
        assert_eq!(
            keychain.read(SecretSlot::ProtonRefreshToken).as_deref(),
            Some("new-1"),
            "the old session's refreshed token must not be written over the new one"
        );
        assert_eq!(
            keychain.read(SecretSlot::ProtonKeyPass).as_deref(),
            Some("new-key-pass")
        );
    }

    /// The old session's refresh is answered only once the new session is
    /// stored: what it then gets is written nowhere.
    #[test]
    fn a_refresh_answered_after_the_swap_leaves_the_new_session_alone() {
        let runtime = runtime();
        let keychain = Arc::new(Keychain::default());
        let refresh_started = Latch::default();
        let answer_refresh = Latch::default();
        let server = {
            let (started, answer) = (refresh_started.clone(), answer_refresh.clone());
            runtime.block_on(MockServer::start(move |request, _| {
                proton(request, || {
                    started.open();
                    answer.wait();
                    tokens("uid-old", "old-access-3", "old-3")
                })
            }))
        };
        let service = service(&runtime, &server, &keychain);

        runtime.block_on(async {
            let sessions = service.proton_sessions().expect("Proton is configured");
            let old = sessions
                .session(ACCOUNT)
                .await
                .expect("the old session resumes");
            assert_eq!(
                keychain.read(SecretSlot::ProtonRefreshToken).as_deref(),
                Some("old-2")
            );
            // A request on the old session refreshes it, and Proton is slow
            // to answer.
            let request = tokio::spawn(async move { old.client().get_user().await.map(|_| ()) });
            refresh_started.reached().await;

            service
                .replace_proton_session(ACCOUNT, store_new_session(&keychain))
                .await
                .expect("replaced");
            answer_refresh.open();
            let _ = request.await.expect("the request finished");
            assert_only_the_new_session_is_kept(&keychain);

            // The old session was signed out on Proton's side, and the next
            // sync resumes the new one.
            let signed_out = server.requests().into_iter().any(|request| {
                request.method == "DELETE" && request.header("x-pm-uid") == Some("uid-old")
            });
            assert!(signed_out, "the old session is signed out");
            let new = sessions
                .session(ACCOUNT)
                .await
                .expect("the new session resumes");
            assert_eq!(new.client().uid(), "uid-new");
            assert_eq!(
                keychain.read(SecretSlot::ProtonRefreshToken).as_deref(),
                Some("new-2")
            );
        });
    }

    /// The old session's refreshed token is being written when the swap
    /// starts: that write finishes before the new session goes in.
    #[test]
    fn a_refresh_written_during_the_swap_leaves_the_new_session_alone() {
        let runtime = runtime();
        let keychain = Arc::new(Keychain {
            held: Some("old-3"),
            ..Keychain::default()
        });
        let server = {
            let release = keychain.release.clone();
            runtime.block_on(MockServer::start(move |request, _| {
                if request.method == "DELETE" {
                    // Once the old session is signed out, the held write
                    // may finish.
                    release.open();
                }
                proton(request, || tokens("uid-old", "old-access-3", "old-3"))
            }))
        };
        let service = service(&runtime, &server, &keychain);

        runtime.block_on(async {
            let sessions = service.proton_sessions().expect("Proton is configured");
            let old = sessions
                .session(ACCOUNT)
                .await
                .expect("the old session resumes");
            let request = tokio::spawn(async move { old.client().get_user().await.map(|_| ()) });
            keychain.held_started.reached().await;

            service
                .replace_proton_session(ACCOUNT, store_new_session(&keychain))
                .await
                .expect("replaced");
            let _ = request.await.expect("the request finished");
            assert_only_the_new_session_is_kept(&keychain);
        });
    }
}
