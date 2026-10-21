//! Proton sessions kept alive for each account.
//!
//! Bridge keeps one API client per user for as long as the user is
//! connected, and writes every rotated refresh token to its vault
//! (`internal/user`). Zmail does the same: [`ProtonSessions`] holds one
//! [`Client`] per account on one shared [`Manager`], resumes it from the
//! keychain the first time the account syncs, and writes each new refresh
//! token back to the keychain before the request that caused the refresh
//! carries on. A refresh token is single-use, so losing a rotated one would
//! sign the account out.

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result};
use tokio::sync::{Mutex, OnceCell};
use zmail_proton_api::{
    Auth, Client, ClientConfig, Error, Manager, SaltedKeyPass, UnlockedAccount,
};
use zmail_security::{SecretSlot, SecretStore};

/// A connected Proton account: its API client and salted key password.
pub struct ProtonSession {
    client: Client,
    key_pass: SaltedKeyPass,
    signed_out: Arc<AtomicBool>,
    /// The unlocked user and address keys, unlocked on first use and kept
    /// in memory only for the session's lifetime (zeroized on drop).
    keys: OnceCell<Arc<UnlockedAccount>>,
}

impl ProtonSession {
    /// The session's API client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The salted key password that unlocks the account's keys.
    pub fn key_pass(&self) -> &SaltedKeyPass {
        &self.key_pass
    }

    /// The account's unlocked keys, unlocking them the first time.
    ///
    /// Unlocking is local CPU work (bcrypt was done at sign-in; this is
    /// the OpenPGP unlock and key checks), so it runs off the async
    /// workers.
    pub async fn unlocked_keys(&self) -> Result<Arc<UnlockedAccount>> {
        self.keys
            .get_or_try_init(|| async {
                let user = self.client.get_user().await?;
                let addresses = self.client.get_addresses().await?;
                let key_pass = self.key_pass.clone();
                let account = tokio::task::spawn_blocking(move || {
                    zmail_proton_api::unlock(&user, &addresses, &key_pass)
                })
                .await
                .context("the key unlock task stopped")??;
                anyhow::Ok(Arc::new(account))
            })
            .await
            .cloned()
    }

    /// Whether Proton rejected the session's refresh token, so the user
    /// has to sign in again.
    pub fn is_signed_out(&self) -> bool {
        self.signed_out.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for ProtonSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtonSession")
            .field("uid", &self.client.uid())
            .field("signed_out", &self.is_signed_out())
            .finish_non_exhaustive()
    }
}

/// The Proton sessions of every account, on one shared [`Manager`].
pub struct ProtonSessions {
    manager: Manager,
    secrets: Arc<dyn SecretStore>,
    sessions: Mutex<HashMap<i64, Arc<ProtonSession>>>,
}

impl fmt::Debug for ProtonSessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtonSessions")
            .field("manager", &self.manager)
            .finish_non_exhaustive()
    }
}

impl ProtonSessions {
    /// Creates the registry with one API manager for `config`.
    pub fn new(config: ClientConfig, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        Ok(Self {
            manager: Manager::new(config).context("failed to set up the Proton API client")?,
            secrets,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// The shared API manager.
    pub fn manager(&self) -> &Manager {
        &self.manager
    }

    /// Returns the account's session, resuming it from the keychain the
    /// first time (or after it was signed out elsewhere and signed in
    /// again).
    ///
    /// The lock is held while resuming, so two syncs of one account never
    /// spend the same single-use refresh token.
    pub async fn session(&self, account_id: i64) -> Result<Arc<ProtonSession>> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(&account_id) {
            if !session.is_signed_out() {
                return Ok(session.clone());
            }
            sessions.remove(&account_id);
        }
        let session = Arc::new(self.resume(account_id).await?);
        sessions.insert(account_id, session.clone());
        Ok(session)
    }

    /// Drops the account's session, for example when the account is
    /// removed. The keychain entries are the caller's to delete.
    pub async fn forget(&self, account_id: i64) {
        self.sessions.lock().await.remove(&account_id);
    }

    async fn resume(&self, account_id: i64) -> Result<ProtonSession> {
        let read = |slot: SecretSlot| {
            self.secrets
                .get_account_secret_from_slot(account_id, slot)
                .with_context(|| format!("no stored Proton session for account {account_id}"))
        };
        let uid = read(SecretSlot::ProtonUid)?;
        let refresh_token = read(SecretSlot::ProtonRefreshToken)?;
        let key_pass = SaltedKeyPass::from_secret(&read(SecretSlot::ProtonKeyPass)?);

        let (client, auth) = self
            .manager
            .new_client_with_refresh(&uid, &refresh_token)
            .await
            .map_err(|error| sign_in_error(account_id, error))?;
        store_refresh_token(self.secrets.as_ref(), account_id, &auth)?;

        let secrets = self.secrets.clone();
        client.add_auth_handler(move |auth| {
            // A failed write leaves the old token in the keychain, which
            // Proton no longer accepts; the next resume then asks the user
            // to sign in again instead of failing silently.
            let _ = store_refresh_token(secrets.as_ref(), account_id, auth);
        });
        let signed_out = Arc::new(AtomicBool::new(false));
        let flag = signed_out.clone();
        client.add_deauth_handler(move || flag.store(true, Ordering::SeqCst));

        Ok(ProtonSession {
            client,
            key_pass,
            signed_out,
            keys: OnceCell::new(),
        })
    }
}

/// Stores a new session after sign-in (onboarding): the session UID, its
/// refresh token and the salted key password. The login and mailbox
/// passwords are never stored.
pub fn store_new_session(
    secrets: &dyn SecretStore,
    account_id: i64,
    auth: &Auth,
    key_pass: &SaltedKeyPass,
) -> Result<()> {
    secrets.set_account_secret_in_slot(account_id, SecretSlot::ProtonUid, &auth.uid)?;
    store_refresh_token(secrets, account_id, auth)?;
    secrets.set_account_secret_in_slot(
        account_id,
        SecretSlot::ProtonKeyPass,
        &key_pass.to_secret_string(),
    )?;
    Ok(())
}

/// Removes the account's Proton secrets (sign-out or account removal).
pub fn delete_session(secrets: &dyn SecretStore, account_id: i64) -> Result<()> {
    for slot in [
        SecretSlot::ProtonUid,
        SecretSlot::ProtonRefreshToken,
        SecretSlot::ProtonKeyPass,
    ] {
        secrets.delete_account_secret_in_slot(account_id, slot)?;
    }
    Ok(())
}

/// Whether the account has a stored Proton session to resume.
pub fn has_session(secrets: &dyn SecretStore, account_id: i64) -> bool {
    secrets
        .get_account_secret_from_slot(account_id, SecretSlot::ProtonRefreshToken)
        .is_ok_and(|token| !token.is_empty())
}

fn store_refresh_token(secrets: &dyn SecretStore, account_id: i64, auth: &Auth) -> Result<()> {
    secrets
        .set_account_secret_in_slot(
            account_id,
            SecretSlot::ProtonRefreshToken,
            &auth.refresh_token,
        )
        .with_context(|| format!("failed to store the Proton session for account {account_id}"))
}

fn sign_in_error(account_id: i64, error: Error) -> anyhow::Error {
    match error {
        Error::RefreshTokenInvalid(_) | Error::Deauthenticated(_) => anyhow::Error::new(error)
            .context(format!(
                "the Proton session for account {account_id} has ended; sign in again"
            )),
        // A 400 or 422 on refresh also means the token is no longer valid.
        error if matches!(error.status(), Some(400 | 422)) => anyhow::Error::new(error).context(
            format!("the Proton session for account {account_id} has ended; sign in again"),
        ),
        error => anyhow::Error::new(error).context(format!(
            "failed to resume the Proton session for account {account_id}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use zmail_security::InMemorySecretStore;

    use super::*;

    fn auth(refresh_token: &str) -> Auth {
        Auth {
            uid: "uid-1".to_owned(),
            access_token: "access".to_owned(),
            refresh_token: refresh_token.to_owned(),
            ..Auth::default()
        }
    }

    #[test]
    fn stores_and_deletes_a_session_without_passwords() {
        let secrets = InMemorySecretStore::default();
        assert!(!has_session(&secrets, 7));
        store_new_session(
            &secrets,
            7,
            &auth("refresh-1"),
            &SaltedKeyPass::from_secret("salted-key-pass"),
        )
        .expect("stored");
        assert!(has_session(&secrets, 7));
        let read = |slot| secrets.get_account_secret_from_slot(7, slot).expect("slot");
        assert_eq!(read(SecretSlot::ProtonUid), "uid-1");
        assert_eq!(read(SecretSlot::ProtonRefreshToken), "refresh-1");
        assert_eq!(read(SecretSlot::ProtonKeyPass), "salted-key-pass");
        // No password, access token or OAuth2 slot is written.
        for slot in [
            SecretSlot::Password,
            SecretSlot::AccessToken,
            SecretSlot::RefreshToken,
        ] {
            assert!(secrets.get_account_secret_from_slot(7, slot).is_err());
        }

        store_refresh_token(&secrets, 7, &auth("refresh-2")).expect("rotated");
        assert_eq!(read(SecretSlot::ProtonRefreshToken), "refresh-2");

        delete_session(&secrets, 7).expect("deleted");
        assert!(!has_session(&secrets, 7));
        assert!(
            secrets
                .get_account_secret_from_slot(7, SecretSlot::ProtonKeyPass)
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_missing_session_is_a_clear_error() {
        let sessions = ProtonSessions::new(
            ClientConfig::production(
                zmail_proton_api::AppVersion::zmail("0.0.1").expect("version"),
            )
            .expect("config"),
            InMemorySecretStore::shared(),
        )
        .expect("sessions");
        let error = sessions.session(3).await.expect_err("no session");
        assert!(
            error
                .to_string()
                .contains("no stored Proton session for account 3")
        );
    }
}
