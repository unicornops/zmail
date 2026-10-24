//! Secret storage and security defaults for Zmail.

pub mod attachment_safety;
pub mod html;
pub mod sender_safety;
pub mod vault;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use keyring::Entry;

/// Default remote content stance.
pub const REMOTE_CONTENT_POLICY: &str = "Blocked by default.";

/// Default HTML stance.
pub const HTML_RENDERING_POLICY: &str = "Sanitise every message (html::sanitize_html); render it with the OS web engine behind a renderer boundary, with the text view as the fallback (ADR 0006).";

const KEYRING_SERVICE: &str = "dev.zmail.desktop";

/// A message's HTML part as the store keeps it, with its readable text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedHtml {
    /// Sanitised HTML with remote resources blocked and each link keeping
    /// its checked target ([`html::StoredHtml`]).
    pub html: String,
    /// Readable text derived from the sanitized HTML.
    pub text: String,
    /// Whether the source HTML referenced remote content.
    pub has_remote_content: bool,
}

/// Named per-account secret slots.
///
/// The `Password` slot is the original single per-account secret; the
/// OAuth2 slots hold the provider client secret and the granted tokens so
/// background sync can refresh access tokens without user interaction.
/// The Proton slots hold a Proton session (ADR 0004): the session UID, its
/// refresh token and the salted key password that unlocks the account's
/// keys. A Proton account never stores its login password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretSlot {
    /// Account password or app password.
    Password,
    /// OAuth2 provider application client secret (confidential clients only).
    ClientSecret,
    /// Current OAuth2 bearer access token.
    AccessToken,
    /// OAuth2 refresh token used to obtain new access tokens.
    RefreshToken,
    /// Proton session UID (`x-pm-uid`).
    ProtonUid,
    /// Proton session refresh token.
    ProtonRefreshToken,
    /// Proton salted key password (the bcrypt-derived secret that unlocks
    /// the user keys, never the login password).
    ProtonKeyPass,
}

impl SecretSlot {
    /// Returns the stable key fragment identifying the slot.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::ClientSecret => "client-secret",
            Self::AccessToken => "access-token",
            Self::RefreshToken => "refresh-token",
            Self::ProtonUid => "proton-uid",
            Self::ProtonRefreshToken => "proton-refresh-token",
            Self::ProtonKeyPass => "proton-key-pass",
        }
    }
}

/// Stores and retrieves account secrets without writing them to the local metadata database.
pub trait SecretStore: Send + Sync {
    /// Persists the secret for an account.
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()>;
    /// Loads the secret for an account.
    fn get_account_secret(&self, account_id: i64) -> Result<String>;
    /// Removes the secret for an account.
    fn delete_account_secret(&self, account_id: i64) -> Result<()>;
    /// Persists the secret for an account in a named slot.
    fn set_account_secret_in_slot(
        &self,
        account_id: i64,
        slot: SecretSlot,
        secret: &str,
    ) -> Result<()>;
    /// Loads the secret for an account from a named slot.
    fn get_account_secret_from_slot(&self, account_id: i64, slot: SecretSlot) -> Result<String>;
    /// Removes the secret for an account in a named slot.
    fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()>;
}

/// OS keychain-backed implementation used by the desktop app.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    /// Persists the secret for an account in a named slot.
    fn set_slot(&self, account_id: i64, slot: SecretSlot, secret: &str) -> Result<()> {
        entry_for_slot(account_id, slot)?
            .set_password(secret)
            .with_context(|| {
                format!("failed to store {slot:?} credentials for account {account_id}")
            })
    }

    /// Loads the secret for an account from a named slot.
    fn get_slot(&self, account_id: i64, slot: SecretSlot) -> Result<String> {
        entry_for_slot(account_id, slot)?
            .get_password()
            .with_context(|| {
                format!("failed to load {slot:?} credentials for account {account_id}")
            })
    }

    /// Removes the secret for an account in a named slot.
    fn delete_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
        match entry_for_slot(account_id, slot)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!("failed to delete {slot:?} credentials for account {account_id}")
            }),
        }
    }
}

impl SecretStore for KeyringSecretStore {
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()> {
        entry_for(account_id)?
            .set_password(secret)
            .with_context(|| format!("failed to store credentials for account {account_id}"))
    }

    fn get_account_secret(&self, account_id: i64) -> Result<String> {
        entry_for(account_id)?
            .get_password()
            .with_context(|| format!("failed to load credentials for account {account_id}"))
    }

    fn delete_account_secret(&self, account_id: i64) -> Result<()> {
        match entry_for(account_id)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to delete credentials for account {account_id}")),
        }
    }

    fn set_account_secret_in_slot(
        &self,
        account_id: i64,
        slot: SecretSlot,
        secret: &str,
    ) -> Result<()> {
        self.set_slot(account_id, slot, secret)
    }

    fn get_account_secret_from_slot(&self, account_id: i64, slot: SecretSlot) -> Result<String> {
        self.get_slot(account_id, slot)
    }

    fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
        self.delete_slot(account_id, slot)
    }
}

/// In-memory secret store used by tests and local fixtures.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<(i64, SecretSlot), String>>,
    /// Store keys ([`vault::StoreKeychain`]), as the keychain holds them.
    store_keys: Mutex<HashMap<vault::KeyId, zeroize::Zeroizing<String>>>,
}

impl InMemorySecretStore {
    /// Creates a new in-memory store wrapped in `Arc` for convenience.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(i64, SecretSlot), String>> {
        self.secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_store_keys(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<vault::KeyId, zeroize::Zeroizing<String>>> {
        self.store_keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SecretStore for InMemorySecretStore {
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()> {
        self.set_account_secret_in_slot(account_id, SecretSlot::Password, secret)
    }

    fn get_account_secret(&self, account_id: i64) -> Result<String> {
        self.get_account_secret_from_slot(account_id, SecretSlot::Password)
    }

    fn delete_account_secret(&self, account_id: i64) -> Result<()> {
        self.delete_account_secret_in_slot(account_id, SecretSlot::Password)
    }

    fn set_account_secret_in_slot(
        &self,
        account_id: i64,
        slot: SecretSlot,
        secret: &str,
    ) -> Result<()> {
        self.lock().insert((account_id, slot), secret.to_owned());
        Ok(())
    }

    fn get_account_secret_from_slot(&self, account_id: i64, slot: SecretSlot) -> Result<String> {
        self.lock()
            .get(&(account_id, slot))
            .cloned()
            .with_context(|| format!("no credentials stored for account {account_id}"))
    }

    fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
        self.lock().remove(&(account_id, slot));
        Ok(())
    }
}

fn entry_for(account_id: i64) -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, &format!("account-{account_id}"))
        .map_err(anyhow::Error::from)
        .with_context(|| format!("failed to prepare secure storage for account {account_id}"))
}

/// Builds the keyring entry for a named slot.
///
/// The password slot keeps the original `account-{id}` entry so existing
/// installs keep working; token slots append a stable suffix.
fn entry_for_slot(account_id: i64, slot: SecretSlot) -> Result<Entry> {
    match slot {
        SecretSlot::Password => entry_for(account_id),
        slot => Entry::new(
            KEYRING_SERVICE,
            &format!("account-{account_id}-{}", slot.as_str()),
        )
        .map_err(anyhow::Error::from)
        .with_context(|| {
            format!("failed to prepare secure storage for account {account_id} {slot:?}")
        }),
    }
}

/// Sanitises a message's HTML part for the store and the text view: the
/// text-output path of [`html::sanitize_html_for_storage`].
pub fn sanitize_html_for_display(html: &str) -> SanitizedHtml {
    let stored = html::sanitize_html_for_storage(html);
    SanitizedHtml {
        html: stored.html,
        text: if stored.text.is_empty() {
            "No body available.".to_owned()
        } else {
            stored.text
        },
        has_remote_content: stored.has_remote_content,
    }
}

#[cfg(test)]
mod tests {
    use super::{InMemorySecretStore, SecretSlot, SecretStore};

    #[test]
    fn in_memory_store_round_trips_secrets() {
        let store = InMemorySecretStore::default();
        store
            .set_account_secret(4, "top-secret")
            .expect("secret should store");
        assert_eq!(
            store.get_account_secret(4).expect("secret should load"),
            "top-secret"
        );
        store
            .delete_account_secret(4)
            .expect("secret should delete");
        assert!(store.get_account_secret(4).is_err());
    }

    #[test]
    fn in_memory_store_keeps_named_slots_independent() {
        let store = InMemorySecretStore::default();
        store
            .set_account_secret(4, "app-password")
            .expect("the password slot should store");
        store
            .set_account_secret_in_slot(4, SecretSlot::AccessToken, "access-1")
            .expect("the access token slot should store");
        store
            .set_account_secret_in_slot(4, SecretSlot::RefreshToken, "refresh-1")
            .expect("the refresh token slot should store");
        store
            .set_account_secret_in_slot(4, SecretSlot::ClientSecret, "client-1")
            .expect("the client secret slot should store");

        // The legacy single-secret API addresses the password slot only.
        assert_eq!(
            store.get_account_secret(4).expect("password loads"),
            "app-password"
        );
        assert_eq!(
            store
                .get_account_secret_from_slot(4, SecretSlot::AccessToken)
                .expect("access token loads"),
            "access-1"
        );
        assert_eq!(
            store
                .get_account_secret_from_slot(4, SecretSlot::RefreshToken)
                .expect("refresh token loads"),
            "refresh-1"
        );
        assert_eq!(
            store
                .get_account_secret_from_slot(4, SecretSlot::ClientSecret)
                .expect("client secret loads"),
            "client-1"
        );

        store
            .delete_account_secret_in_slot(4, SecretSlot::AccessToken)
            .expect("the access token slot should delete");
        assert!(
            store
                .get_account_secret_from_slot(4, SecretSlot::AccessToken)
                .is_err()
        );
        // Deleting one slot leaves the others untouched.
        assert!(store.get_account_secret(4).is_ok());
        assert!(
            store
                .get_account_secret_from_slot(4, SecretSlot::RefreshToken)
                .is_ok()
        );
    }

    #[test]
    fn secret_slots_have_stable_keys() {
        assert_eq!(SecretSlot::Password.as_str(), "password");
        assert_eq!(SecretSlot::ClientSecret.as_str(), "client-secret");
        assert_eq!(SecretSlot::AccessToken.as_str(), "access-token");
        assert_eq!(SecretSlot::RefreshToken.as_str(), "refresh-token");
        assert_eq!(SecretSlot::ProtonUid.as_str(), "proton-uid");
        assert_eq!(
            SecretSlot::ProtonRefreshToken.as_str(),
            "proton-refresh-token"
        );
        assert_eq!(SecretSlot::ProtonKeyPass.as_str(), "proton-key-pass");
    }

    #[test]
    fn proton_slots_are_separate_from_oauth_slots() {
        let store = InMemorySecretStore::default();
        store
            .set_account_secret_in_slot(9, SecretSlot::RefreshToken, "oauth-refresh")
            .expect("oauth");
        store
            .set_account_secret_in_slot(9, SecretSlot::ProtonRefreshToken, "proton-refresh")
            .expect("proton");
        store
            .set_account_secret_in_slot(9, SecretSlot::ProtonKeyPass, "salted")
            .expect("key pass");
        assert_eq!(
            store
                .get_account_secret_from_slot(9, SecretSlot::RefreshToken)
                .expect("oauth"),
            "oauth-refresh"
        );
        assert_eq!(
            store
                .get_account_secret_from_slot(9, SecretSlot::ProtonRefreshToken)
                .expect("proton"),
            "proton-refresh"
        );
        assert!(
            store
                .get_account_secret_from_slot(9, SecretSlot::Password)
                .is_err()
        );
    }
}
