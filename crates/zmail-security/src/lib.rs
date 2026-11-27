//! Secret storage and security defaults for Zmail.

pub mod attachment_safety;
pub mod html;
pub mod keychain;
pub mod links;
pub mod mailto;
pub mod sender_safety;
pub mod sender_warning;
pub mod terminal;
pub mod vault;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use keyring::Entry;

pub use keychain::{InMemoryKeychain, Keychain, StoreId, StoreSecrets};

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

/// Defines [`SecretSlot`], [`SecretSlot::ALL`] and [`SecretSlot::as_str`]
/// from one list, so a slot can't be left out of `ALL`: removing an
/// account clears every slot in it, and a slot missing from it would
/// outlive its account in the keychain.
macro_rules! secret_slots {
    (
        $(#[$meta:meta])*
        pub enum SecretSlot {
            $($(#[doc = $doc:literal])* $slot:ident => $name:literal,)+
        }
    ) => {
        $(#[$meta])*
        pub enum SecretSlot {
            $($(#[doc = $doc])* $slot,)+
        }

        impl SecretSlot {
            /// Every slot an account can have a secret in.
            pub const ALL: [Self; [$($name),+].len()] = [$(Self::$slot),+];

            /// Returns the stable key fragment identifying the slot.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$slot => $name,)+
                }
            }
        }
    };
}

secret_slots! {
    /// Named per-account secret slots.
    ///
    /// The `Password` slot is the original single per-account secret; the
    /// OAuth2 slots hold the provider client secret and the granted tokens so
    /// background sync can refresh access tokens without user interaction.
    /// The Proton slots hold a Proton session (ADR 0004): the session UID, its
    /// refresh token and the salted key password that unlocks the account's
    /// keys. A Proton account never stores its login password. Each slot's
    /// key fragment names its keychain entry ([`keychain::account_entry_name`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum SecretSlot {
        /// Account password or app password.
        Password => "password",
        /// OAuth2 provider application client secret (confidential clients only).
        ClientSecret => "client-secret",
        /// Current OAuth2 bearer access token.
        AccessToken => "access-token",
        /// OAuth2 refresh token used to obtain new access tokens.
        RefreshToken => "refresh-token",
        /// Proton session UID (`x-pm-uid`).
        ProtonUid => "proton-uid",
        /// Proton session refresh token.
        ProtonRefreshToken => "proton-refresh-token",
        /// Proton salted key password (the bcrypt-derived secret that unlocks
        /// the user keys, never the login password).
        ProtonKeyPass => "proton-key-pass",
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
    /// Removes every secret kept for `account_id`, in every slot
    /// ([`SecretSlot::ALL`]): what removing an account does.
    ///
    /// It also runs before a new account's secrets are stored. Secrets
    /// are named by the store and the account's id, and a store never
    /// hands an id out twice, but a store restored from a backup can hand
    /// out again an id that an account added after the backup had: what
    /// that account left behind is never read as the new account's.
    /// Only this store's entries are touched. Every slot is tried; the
    /// first failure is returned.
    fn clear_account_secrets(&self, account_id: i64) -> Result<()> {
        let mut cleared = Ok(());
        for slot in SecretSlot::ALL {
            let deleted = self.delete_account_secret_in_slot(account_id, slot);
            if cleared.is_ok() {
                cleared = deleted;
            }
        }
        cleared
    }
}

/// The OS keychain (macOS login keychain, Windows Credential Manager,
/// Secret Service on Linux), under Zmail's service `dev.zmail.desktop`.
///
/// It holds the store keys and markers ([`vault::StoreKeychain`]) and,
/// through [`StoreSecrets`], the account secrets of every store on the
/// machine; it is never a [`SecretStore`] itself, so no account secret is
/// named without its store.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsKeychain;

impl Keychain for OsKeychain {
    fn read_entry(&self, name: &str) -> Result<Option<String>> {
        match entry(name)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("failed to read {name} from the keychain"))
            }
        }
    }

    fn write_entry(&self, name: &str, secret: &str) -> Result<()> {
        entry(name)?
            .set_password(secret)
            .with_context(|| format!("failed to write {name} to the keychain"))
    }

    fn delete_entry(&self, name: &str) -> Result<()> {
        match entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to delete {name} from the keychain"))
            }
        }
    }
}

/// The OS keychain entry `name` under Zmail's service.
fn entry(name: &str) -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, name)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("failed to prepare secure storage for {name}"))
}

/// One store's account secrets and its store key in memory, for tests and
/// fixtures with a single store. Tests with several stores sharing a
/// keychain use [`InMemoryKeychain`] and [`StoreSecrets`], as the app does.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<(i64, SecretSlot), String>>,
    /// Store keys ([`vault::StoreKeychain`]), as the keychain holds them.
    store_keys: Mutex<HashMap<vault::KeyId, zeroize::Zeroizing<String>>>,
    /// Store markers ([`vault::StoreLocation`]), as the keychain holds them.
    store_markers: Mutex<HashMap<vault::StoreLocation, String>>,
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

    fn lock_store_markers(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<vault::StoreLocation, String>> {
        self.store_markers
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
    use super::{InMemoryKeychain, InMemorySecretStore, Keychain, SecretSlot, SecretStore};
    use crate::{StoreId, StoreSecrets};

    /// Compiles only for a type that wipes itself when dropped.
    fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>(_: &T) {}

    /// Every way a secret is read hands it over in a buffer that wipes it
    /// when dropped (#474): the secret stores' getters and the keychain's
    /// entries, which hold the store keys as well.
    #[test]
    fn secrets_are_read_into_buffers_that_wipe_them() {
        let store = InMemorySecretStore::default();
        store.set_account_secret(4, "password").expect("stored");
        store
            .set_account_secret_in_slot(4, SecretSlot::ProtonKeyPass, "key-pass")
            .expect("stored");
        let password = store.get_account_secret(4).expect("password");
        wiped_on_drop(&password);
        assert_eq!(password.as_str(), "password");
        let key_pass = store
            .get_account_secret_from_slot(4, SecretSlot::ProtonKeyPass)
            .expect("key password");
        wiped_on_drop(&key_pass);
        assert_eq!(key_pass.as_str(), "key-pass");

        let keychain = InMemoryKeychain::shared();
        let secrets = StoreSecrets::new(keychain.clone(), StoreId::generate(), []);
        secrets
            .set_account_secret_in_slot(1, SecretSlot::RefreshToken, "refresh")
            .expect("stored");
        let refresh = secrets
            .get_account_secret_from_slot(1, SecretSlot::RefreshToken)
            .expect("refresh token");
        wiped_on_drop(&refresh);
        assert_eq!(refresh.as_str(), "refresh");
        keychain.write_entry("entry", "secret").expect("written");
        let entry = keychain.read_entry("entry").expect("read").expect("entry");
        wiped_on_drop(&entry);
        assert_eq!(entry.as_str(), "secret");
    }

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
    fn clearing_an_account_empties_every_slot_and_no_other_account() {
        let store = InMemorySecretStore::default();
        for account in [4, 5] {
            for slot in SecretSlot::ALL {
                store
                    .set_account_secret_in_slot(account, slot, "stale")
                    .expect("stored");
            }
        }
        store.clear_account_secrets(4).expect("cleared");
        for slot in SecretSlot::ALL {
            assert!(
                store.get_account_secret_from_slot(4, slot).is_err(),
                "{slot:?}"
            );
            assert!(
                store.get_account_secret_from_slot(5, slot).is_ok(),
                "{slot:?}"
            );
        }
        // Clearing an account with nothing stored is not an error.
        store.clear_account_secrets(4).expect("cleared again");
    }

    #[test]
    fn every_slot_is_listed_once() {
        let mut names: Vec<&str> = SecretSlot::ALL.iter().map(|slot| slot.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SecretSlot::ALL.len());
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
