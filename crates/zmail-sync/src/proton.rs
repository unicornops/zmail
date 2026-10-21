//! Proton Mail accounts in the sync runtime (ADR 0004).
//!
//! A Proton account connects with a live session rather than a stored
//! secret: [`SyncService::connect`] hands the adapters a [`Credentials`].
//! The Proton half only exists with the `proton` feature, which `zmail-app`
//! does not enable until the local store is encrypted at rest (#139);
//! without it a Proton account fails to connect with a clear message and
//! is never watched.

use std::time::Duration;

#[cfg(not(feature = "proton"))]
use anyhow::bail;
use anyhow::{Context, Result};
use zmail_core::{ProtocolKind, SyncTarget};

use crate::{SyncService, backoff::Backoff, wake::unless_woken};

/// What a protocol adapter connects with.
pub(crate) enum Credentials {
    /// A password or OAuth2 access token (IMAP and JMAP).
    Secret(String),
    /// A resumed Proton session.
    #[cfg(feature = "proton")]
    Proton(std::sync::Arc<zmail_proton::ProtonSession>),
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

    /// Watches a Proton account until the next reconnect. Until the event
    /// loop lands (#237) this syncs the account every `poll_interval`.
    pub(crate) async fn watch_proton(
        &self,
        target: &SyncTarget,
        poll_interval: Duration,
        backoff: &mut Backoff,
        wake: &mut tokio::sync::watch::Receiver<u64>,
    ) {
        self.request_sync(target.id);
        if unless_woken(wake, tokio::time::sleep(poll_interval))
            .await
            .is_none()
        {
            backoff.reset();
        }
    }
}
