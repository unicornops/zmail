//! Proton sessions kept alive for each account.
//!
//! Bridge keeps one API client per user for as long as the user is
//! connected, and writes every rotated refresh token to its vault
//! (`internal/user`). Zmail does the same: [`ProtonSessions`] holds one
//! [`Client`] per account on one shared [`Manager`], resumes it from the
//! keychain the first time the account syncs, and writes each new refresh
//! token back to the keychain before the request that caused the refresh
//! carries on. A refresh token is single-use, so losing a rotated one would
//! sign the account out. A token that could not be written is kept in
//! memory and reported ([`ProtonSession::save_refresh_token`]), so the
//! account's sync status says so instead of the next start signing the
//! account out without a word.
//!
//! Removing an account signs its session out on Proton's side
//! ([`ProtonSessions::sign_out`]), best effort: the refresh token is
//! worthless afterwards, wherever a copy of it may be.

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::sync::{Mutex, OnceCell};
use zeroize::Zeroizing;
use zmail_proton_api::{
    Auth, Client, ClientConfig, Error, Manager, SaltedKeyPass, UnlockedAccount,
};
use zmail_security::{SecretSlot, SecretStore};

/// A connected Proton account: its API client and salted key password.
pub struct ProtonSession {
    client: Client,
    key_pass: SaltedKeyPass,
    signed_out: Arc<AtomicBool>,
    /// Where rotated refresh tokens are written.
    tokens: Arc<RefreshTokenSlot>,
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

    /// Makes sure the session's newest refresh token is in the keychain.
    ///
    /// A refresh writes the new token at once. When that write failed (a
    /// locked or unavailable keychain), this tries again and, failing
    /// that, returns an error for the account's sync status: the session
    /// works while Zmail runs, but the keychain holds a token Proton no
    /// longer accepts, so the next start would have to sign in again.
    pub fn save_refresh_token(&self) -> Result<()> {
        self.tokens.save_unsaved().map_err(|error| {
            anyhow::anyhow!(
                "Zmail could not save the Proton session in the keychain ({error:#}). Mail still \
                 syncs while Zmail is open, but you will have to sign in again after restarting \
                 it unless the keychain starts working"
            )
        })
    }
}

/// The keychain slot a session's rotated refresh tokens go to.
struct RefreshTokenSlot {
    secrets: Arc<dyn SecretStore>,
    account_id: i64,
    /// The newest token, while the keychain does not hold it.
    unsaved: StdMutex<Option<Zeroizing<String>>>,
    /// Set when the session is signed out for good (the account was
    /// removed): nothing more is written.
    closed: AtomicBool,
}

impl RefreshTokenSlot {
    fn new(secrets: Arc<dyn SecretStore>, account_id: i64) -> Self {
        Self {
            secrets,
            account_id,
            unsaved: StdMutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    /// Writes a rotated token, remembering it if the write fails.
    fn save(&self, refresh_token: &str) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let mut unsaved = self
            .unsaved
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *unsaved = write_refresh_token(self.secrets.as_ref(), self.account_id, refresh_token)
            .is_err()
            .then(|| Zeroizing::new(refresh_token.to_owned()));
    }

    /// Writes the remembered token, if a write failed earlier.
    fn save_unsaved(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut unsaved = self
            .unsaved
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(refresh_token) = unsaved.as_ref() {
            write_refresh_token(self.secrets.as_ref(), self.account_id, refresh_token)?;
            *unsaved = None;
        }
        Ok(())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// A stored session's UID and refresh token, read from the keychain
/// before an account's entries are deleted so the session can still be
/// signed out ([`ProtonSessions::sign_out`]).
pub struct StoredSession {
    uid: String,
    refresh_token: Zeroizing<String>,
}

impl StoredSession {
    /// A session from its UID and refresh token.
    pub fn new(uid: String, refresh_token: String) -> Self {
        Self {
            uid,
            refresh_token: Zeroizing::new(refresh_token),
        }
    }
}

impl fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredSession")
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}

/// How long signing a removed account's session out may take before it is
/// given up: it is best effort and must never hold anything up.
const SIGN_OUT_TIMEOUT: Duration = Duration::from_secs(20);

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
        if let Some(session) = self.sessions.lock().await.remove(&account_id) {
            // Nothing of a forgotten session is written to the keychain:
            // the account's entries are gone, and its id may be used again.
            session.tokens.close();
        }
    }

    /// Signs a removed account's session out on Proton's side (`DELETE
    /// /auth/v4`) and drops it, so its refresh token stops working
    /// wherever a copy may be. Best effort: offline, or with a session
    /// Proton already ended, the session is only dropped.
    ///
    /// The session Zmail has open is used when there is one. Otherwise
    /// `stored`, read from the keychain before the account's entries were
    /// deleted, is resumed just to sign it out. Nothing is written to the
    /// keychain. Returns whether Proton confirmed the sign-out.
    pub async fn sign_out(&self, account_id: i64, stored: Option<StoredSession>) -> bool {
        let live = self.sessions.lock().await.remove(&account_id);
        if let Some(session) = &live {
            session.tokens.close();
        }
        let sign_out = async {
            if let Some(session) = live.filter(|session| !session.is_signed_out()) {
                let signed_out = session.client.auth_delete().await.is_ok();
                session.client.close();
                if signed_out {
                    return true;
                }
                // The open session may have been replaced by one resumed
                // from the keychain since; try the stored one too.
            }
            let Some(stored) = stored else {
                return false;
            };
            let Ok((client, _)) = self
                .manager
                .new_client_with_refresh(&stored.uid, &stored.refresh_token)
                .await
            else {
                return false;
            };
            let signed_out = client.auth_delete().await.is_ok();
            client.close();
            signed_out
        };
        tokio::time::timeout(SIGN_OUT_TIMEOUT, sign_out)
            .await
            .unwrap_or(false)
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

        let tokens = Arc::new(RefreshTokenSlot::new(self.secrets.clone(), account_id));
        let slot = tokens.clone();
        // A failed write leaves the old token in the keychain, which
        // Proton no longer accepts. The slot keeps the new one, and the
        // sync reports it (`ProtonSession::save_refresh_token`).
        client.add_auth_handler(move |auth| slot.save(&auth.refresh_token));
        let signed_out = Arc::new(AtomicBool::new(false));
        let flag = signed_out.clone();
        client.add_deauth_handler(move || flag.store(true, Ordering::SeqCst));

        Ok(ProtonSession {
            client,
            key_pass,
            signed_out,
            tokens,
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

/// The account's stored session, if the keychain holds its UID and
/// refresh token.
pub fn stored_session(secrets: &dyn SecretStore, account_id: i64) -> Option<StoredSession> {
    let read = |slot| {
        secrets
            .get_account_secret_from_slot(account_id, slot)
            .ok()
            .filter(|value| !value.is_empty())
    };
    Some(StoredSession::new(
        read(SecretSlot::ProtonUid)?,
        read(SecretSlot::ProtonRefreshToken)?,
    ))
}

fn store_refresh_token(secrets: &dyn SecretStore, account_id: i64, auth: &Auth) -> Result<()> {
    write_refresh_token(secrets, account_id, &auth.refresh_token)
}

fn write_refresh_token(
    secrets: &dyn SecretStore,
    account_id: i64,
    refresh_token: &str,
) -> Result<()> {
    secrets
        .set_account_secret_in_slot(account_id, SecretSlot::ProtonRefreshToken, refresh_token)
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

    /// A keychain that can be made to refuse writes.
    #[derive(Default)]
    struct FlakySecrets {
        inner: InMemorySecretStore,
        refuse_writes: AtomicBool,
    }

    impl SecretStore for FlakySecrets {
        fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()> {
            self.inner.set_account_secret(account_id, secret)
        }

        fn get_account_secret(&self, account_id: i64) -> Result<String> {
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
            if self.refuse_writes.load(Ordering::SeqCst) {
                anyhow::bail!("the keychain is locked");
            }
            self.inner
                .set_account_secret_in_slot(account_id, slot, secret)
        }

        fn get_account_secret_from_slot(
            &self,
            account_id: i64,
            slot: SecretSlot,
        ) -> Result<String> {
            self.inner.get_account_secret_from_slot(account_id, slot)
        }

        fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
            self.inner.delete_account_secret_in_slot(account_id, slot)
        }
    }

    /// A rotated refresh token the keychain refuses is kept and reported,
    /// then written once the keychain works again.
    #[test]
    fn a_refresh_token_the_keychain_refuses_is_reported_and_retried() {
        let secrets = Arc::new(FlakySecrets::default());
        let read = || {
            secrets
                .get_account_secret_from_slot(7, SecretSlot::ProtonRefreshToken)
                .ok()
        };
        let slot = RefreshTokenSlot::new(secrets.clone(), 7);
        slot.save("refresh-1");
        assert_eq!(read().as_deref(), Some("refresh-1"));
        slot.save_unsaved().expect("nothing unsaved");

        secrets.refuse_writes.store(true, Ordering::SeqCst);
        slot.save("refresh-2");
        assert_eq!(read().as_deref(), Some("refresh-1"), "the write failed");
        let error = slot.save_unsaved().expect_err("still refused");
        assert!(format!("{error:#}").contains("the keychain is locked"));
        // A newer token replaces the unsaved one.
        slot.save("refresh-3");

        secrets.refuse_writes.store(false, Ordering::SeqCst);
        slot.save_unsaved().expect("written now");
        assert_eq!(read().as_deref(), Some("refresh-3"));
        slot.save_unsaved().expect("nothing unsaved");

        // A later successful write clears an earlier failure.
        secrets.refuse_writes.store(true, Ordering::SeqCst);
        slot.save("refresh-4");
        secrets.refuse_writes.store(false, Ordering::SeqCst);
        slot.save("refresh-5");
        slot.save_unsaved().expect("nothing unsaved");
        assert_eq!(read().as_deref(), Some("refresh-5"));
    }

    /// Once the account is removed, nothing of its session is written to
    /// the keychain again.
    #[test]
    fn a_closed_slot_writes_nothing() {
        let secrets = Arc::new(FlakySecrets::default());
        let slot = RefreshTokenSlot::new(secrets.clone(), 7);
        secrets.refuse_writes.store(true, Ordering::SeqCst);
        slot.save("refresh-1");
        secrets.refuse_writes.store(false, Ordering::SeqCst);
        slot.close();
        slot.save("refresh-2");
        slot.save_unsaved().expect("closed");
        assert!(
            secrets
                .get_account_secret_from_slot(7, SecretSlot::ProtonRefreshToken)
                .is_err()
        );
    }

    #[test]
    fn reads_the_stored_session_only_when_it_is_whole() {
        let secrets = InMemorySecretStore::default();
        assert!(stored_session(&secrets, 7).is_none());
        secrets
            .set_account_secret_in_slot(7, SecretSlot::ProtonUid, "uid-1")
            .expect("stored");
        assert!(stored_session(&secrets, 7).is_none());
        store_refresh_token(&secrets, 7, &auth("refresh-1")).expect("stored");
        let stored = stored_session(&secrets, 7).expect("whole");
        assert_eq!(stored.uid, "uid-1");
        assert_eq!(stored.refresh_token.as_str(), "refresh-1");
        assert!(!format!("{stored:?}").contains("refresh-1"));
    }

    /// With no open session and nothing stored there is nothing to sign
    /// out, and nothing is asked of the network.
    #[tokio::test]
    async fn signing_out_without_a_session_does_nothing() {
        let sessions = ProtonSessions::new(
            ClientConfig::production(
                zmail_proton_api::AppVersion::zmail("0.0.1").expect("version"),
            )
            .expect("config"),
            InMemorySecretStore::shared(),
        )
        .expect("sessions");
        assert!(!sessions.sign_out(3, None).await);
        sessions.forget(3).await;
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
