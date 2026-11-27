//! Account secrets in the keychain, named by the store they belong to
//! (ADR 0005 §3, "Account secrets").
//!
//! Every store numbers its accounts from 1, and every store on a machine
//! shares the one keychain (a second data directory with
//! `ZMAIL_DATA_DIR`, or a new store after `reset-store`). So an account's
//! secrets are named by its store's id as well as its own:
//! `store-<store id>-account-<account id>-<slot>`. Nothing outside this
//! module builds an account entry's name.
//!
//! Builds before stores were told apart named them `account-<id>` (the
//! password) and `account-<id>-<slot>`. [`StoreSecrets`] moves an
//! account's secrets from those names to its own, and only for the
//! accounts its store says were there before the change (the store marks
//! them when it is first opened by a build with this change): an account
//! added since, in this store or any other, never reads, takes or deletes
//! an entry under an old name.

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::{Context, Result, anyhow, bail};
use rand::{Rng, rand_core::UnwrapErr, rngs::SysRng};
use zeroize::Zeroizing;

use crate::{SecretSlot, SecretStore};

/// The keychain's entries under Zmail's service, by name: the OS keychain
/// in the app ([`crate::OsKeychain`]), memory in tests
/// ([`InMemoryKeychain`]). Account secrets go through [`StoreSecrets`],
/// which names them.
pub trait Keychain: Send + Sync {
    /// The secret in the entry `name`, in a buffer that wipes it when
    /// dropped; `Ok(None)` when there is no such entry.
    fn read_entry(&self, name: &str) -> Result<Option<Zeroizing<String>>>;
    /// Writes `secret` to the entry `name`, replacing what it held.
    fn write_entry(&self, name: &str, secret: &str) -> Result<()>;
    /// Deletes the entry `name`; deleting a missing entry is not an error.
    fn delete_entry(&self, name: &str) -> Result<()>;
}

/// Length of a store id, in bytes.
const STORE_ID_LEN: usize = 16;

/// Identifies a store in the names of its accounts' keychain entries.
///
/// A random id the store keeps in its own (encrypted) database from its
/// first open: it stays the same when the store's key is rotated, and a
/// backup of the store has it too. Not secret.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoreId([u8; STORE_ID_LEN]);

impl StoreId {
    /// A fresh random store id, from the OS's random source.
    pub fn generate() -> Self {
        let mut bytes = [0; STORE_ID_LEN];
        UnwrapErr(SysRng).fill_bytes(&mut bytes);
        Self(bytes)
    }
}

impl fmt::Display for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StoreId({self})")
    }
}

impl FromStr for StoreId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        if value.len() != STORE_ID_LEN * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("a store id is {STORE_ID_LEN} bytes of hexadecimal");
        }
        let mut bytes = [0; STORE_ID_LEN];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .context("a store id is hexadecimal")?;
        }
        Ok(Self(bytes))
    }
}

/// The name of the keychain entry holding `account_id`'s `slot` secret in
/// the store `store`.
pub fn account_entry_name(store: StoreId, account_id: i64, slot: SecretSlot) -> String {
    format!("store-{store}-account-{account_id}-{}", slot.as_str())
}

/// The name builds before #475 gave `account_id`'s `slot` secret, in
/// whichever store: `account-<id>` for the password, `account-<id>-<slot>`
/// for the others. Only read, moved and deleted, never written.
fn legacy_entry_name(account_id: i64, slot: SecretSlot) -> String {
    match slot {
        SecretSlot::Password => format!("account-{account_id}"),
        slot => format!("account-{account_id}-{}", slot.as_str()),
    }
}

/// One store's account secrets, in a keychain other stores may share:
/// the [`SecretStore`] the app uses. Built by the store
/// (`LocalStore::account_secrets`), which knows its id and which of its
/// accounts may still have secrets under the old names.
pub struct StoreSecrets {
    keychain: Arc<dyn Keychain>,
    store: StoreId,
    /// Accounts that were in the store before its secrets were named by
    /// it, whose secrets may still be under the old names. Only these
    /// read, move and delete entries under the old names.
    legacy_accounts: Mutex<HashSet<i64>>,
}

impl fmt::Debug for StoreSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreSecrets")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl StoreSecrets {
    /// The secrets of the store `store` in `keychain`. `legacy_accounts`
    /// are the store's accounts whose secrets may still be under the old
    /// names (see the module's documentation).
    pub fn new(
        keychain: Arc<dyn Keychain>,
        store: StoreId,
        legacy_accounts: impl IntoIterator<Item = i64>,
    ) -> Self {
        Self {
            keychain,
            store,
            legacy_accounts: Mutex::new(legacy_accounts.into_iter().collect()),
        }
    }

    /// The store whose secrets these are.
    pub fn store(&self) -> StoreId {
        self.store
    }

    /// Moves every secret `account_id` still has under the old names to
    /// this store's names, then deletes the old entries. Each slot is
    /// copied, read back and only then deleted under its old name, so a
    /// crash or a keychain error part way leaves every secret under one
    /// name or the other, and calling this again finishes the move. A
    /// secret already under this store's name is kept (it was moved
    /// before a crash, or written since) and the old entry deleted.
    ///
    /// Does nothing for an account the store did not mark as having
    /// secrets under the old names. Once every slot has moved, the
    /// account is no longer looked for under them; the first failure
    /// is returned otherwise, after every slot has been tried.
    pub fn claim_legacy_secrets(&self, account_id: i64) -> Result<()> {
        if !self.may_claim(account_id) {
            return Ok(());
        }
        let mut claimed = Ok(());
        for slot in SecretSlot::ALL {
            let moved = self.claim_legacy_slot(account_id, slot).map(|_| ());
            if claimed.is_ok() {
                claimed = moved;
            }
        }
        if claimed.is_ok() {
            self.legacy().remove(&account_id);
        }
        claimed
    }

    /// Moves `account_id`'s `slot` secret from its old name, as
    /// [`StoreSecrets::claim_legacy_secrets`] does for every slot, and
    /// returns the secret now under this store's name.
    fn claim_legacy_slot(
        &self,
        account_id: i64,
        slot: SecretSlot,
    ) -> Result<Option<Zeroizing<String>>> {
        let name = self.name(account_id, slot);
        let legacy_name = legacy_entry_name(account_id, slot);
        let current = self.keychain.read_entry(&name)?;
        let Some(legacy) = self.keychain.read_entry(&legacy_name)? else {
            return Ok(current);
        };
        let current = match current {
            Some(current) => current,
            None => {
                self.keychain.write_entry(&name, &legacy)?;
                // The old entry goes only once the new one reads back.
                if self.keychain.read_entry(&name)?.as_ref() != Some(&legacy) {
                    bail!(
                        "the keychain did not return the {slot:?} secret of account {account_id} \
                         it was moved to"
                    );
                }
                legacy
            }
        };
        self.keychain.delete_entry(&legacy_name)?;
        Ok(Some(current))
    }

    fn name(&self, account_id: i64, slot: SecretSlot) -> String {
        account_entry_name(self.store, account_id, slot)
    }

    fn may_claim(&self, account_id: i64) -> bool {
        self.legacy().contains(&account_id)
    }

    fn legacy(&self) -> MutexGuard<'_, HashSet<i64>> {
        self.legacy_accounts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SecretStore for StoreSecrets {
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()> {
        self.set_account_secret_in_slot(account_id, SecretSlot::Password, secret)
    }

    fn get_account_secret(&self, account_id: i64) -> Result<Zeroizing<String>> {
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
        self.keychain
            .write_entry(&self.name(account_id, slot), secret)
            .with_context(|| {
                format!("failed to store {slot:?} credentials for account {account_id}")
            })
    }

    fn get_account_secret_from_slot(
        &self,
        account_id: i64,
        slot: SecretSlot,
    ) -> Result<Zeroizing<String>> {
        let found = match self.keychain.read_entry(&self.name(account_id, slot)) {
            // An account whose secrets could not all be moved when the
            // store was opened moves this one now.
            Ok(None) if self.may_claim(account_id) => self.claim_legacy_slot(account_id, slot),
            found => found,
        };
        found
            .with_context(|| {
                format!("failed to load {slot:?} credentials for account {account_id}")
            })?
            .ok_or_else(|| anyhow!("no {slot:?} credentials stored for account {account_id}"))
    }

    fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
        let mut deleted = self.keychain.delete_entry(&self.name(account_id, slot));
        // An account that may still have the secret under its old name
        // owns that entry too.
        if self.may_claim(account_id) {
            deleted = deleted.and(
                self.keychain
                    .delete_entry(&legacy_entry_name(account_id, slot)),
            );
        }
        deleted.with_context(|| {
            format!("failed to delete {slot:?} credentials for account {account_id}")
        })
    }
}

/// What a test keychain was asked to do with an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeychainAccess {
    /// [`Keychain::read_entry`].
    Read,
    /// [`Keychain::write_entry`].
    Write,
    /// [`Keychain::delete_entry`].
    Delete,
}

/// A keychain in memory for tests that need several stores sharing one, as
/// they share the OS keychain. It records the name of every entry it was
/// asked about, so tests can check how entries are named.
#[derive(Debug, Default)]
pub struct InMemoryKeychain {
    entries: Mutex<BTreeMap<String, Zeroizing<String>>>,
    accesses: Mutex<Vec<(KeychainAccess, String)>>,
}

impl InMemoryKeychain {
    /// A new, empty keychain.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The names of the entries it holds, sorted.
    pub fn entry_names(&self) -> Vec<String> {
        self.lock_entries().keys().cloned().collect()
    }

    /// Every access so far, in order, with the entry's name.
    pub fn accesses(&self) -> Vec<(KeychainAccess, String)> {
        self.lock_accesses().clone()
    }

    /// Forgets the accesses so far: for a test that sets the keychain up
    /// before what it checks.
    pub fn clear_accesses(&self) {
        self.lock_accesses().clear();
    }

    fn record(&self, access: KeychainAccess, name: &str) {
        self.lock_accesses().push((access, name.to_owned()));
    }

    fn lock_entries(&self) -> MutexGuard<'_, BTreeMap<String, Zeroizing<String>>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_accesses(&self) -> MutexGuard<'_, Vec<(KeychainAccess, String)>> {
        self.accesses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Keychain for InMemoryKeychain {
    fn read_entry(&self, name: &str) -> Result<Option<Zeroizing<String>>> {
        self.record(KeychainAccess::Read, name);
        Ok(self.lock_entries().get(name).cloned())
    }

    fn write_entry(&self, name: &str, secret: &str) -> Result<()> {
        self.record(KeychainAccess::Write, name);
        self.lock_entries()
            .insert(name.to_owned(), Zeroizing::new(secret.to_owned()));
        Ok(())
    }

    fn delete_entry(&self, name: &str) -> Result<()> {
        self.record(KeychainAccess::Delete, name);
        self.lock_entries().remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        InMemoryKeychain, Keychain, KeychainAccess, StoreId, StoreSecrets, account_entry_name,
        legacy_entry_name,
    };
    use crate::{SecretSlot, SecretStore};

    #[test]
    fn store_ids_are_random_and_read_back() {
        let id = StoreId::generate();
        assert_ne!(id, StoreId::generate());
        assert_eq!(id.to_string().len(), 32);
        assert_eq!(id.to_string().parse::<StoreId>().expect("reads back"), id);
        for bad in ["", "zz", &"0".repeat(31), &"g".repeat(32), &"0".repeat(34)] {
            assert!(bad.parse::<StoreId>().is_err(), "{bad}");
        }
    }

    #[test]
    fn entries_are_named_by_store_account_and_slot() {
        let store: StoreId = "00112233445566778899aabbccddeeff"
            .parse()
            .expect("a store id");
        assert_eq!(
            account_entry_name(store, 4, SecretSlot::Password),
            "store-00112233445566778899aabbccddeeff-account-4-password"
        );
        assert_eq!(
            account_entry_name(store, 4, SecretSlot::ProtonKeyPass),
            "store-00112233445566778899aabbccddeeff-account-4-proton-key-pass"
        );
        // What builds before #475 wrote, exactly.
        assert_eq!(legacy_entry_name(4, SecretSlot::Password), "account-4");
        assert_eq!(
            legacy_entry_name(4, SecretSlot::RefreshToken),
            "account-4-refresh-token"
        );
    }

    /// An account the store did not mark never looks at the old names,
    /// whatever is done with it.
    #[test]
    fn an_unmarked_account_never_touches_the_old_names() {
        let keychain = InMemoryKeychain::shared();
        keychain
            .write_entry("account-1", "another store's")
            .expect("written");
        keychain.clear_accesses();
        let secrets = StoreSecrets::new(keychain.clone(), StoreId::generate(), []);

        assert!(secrets.get_account_secret(1).is_err());
        secrets.claim_legacy_secrets(1).expect("nothing to move");
        secrets.clear_account_secrets(1).expect("cleared");
        secrets.set_account_secret(1, "mine").expect("kept");
        assert_eq!(
            secrets.get_account_secret(1).expect("its own").as_str(),
            "mine"
        );

        assert!(
            keychain
                .accesses()
                .iter()
                .all(|(_, name)| name.starts_with("store-")),
            "{:?}",
            keychain.accesses()
        );
        assert_eq!(
            keychain
                .read_entry("account-1")
                .expect("read")
                .as_ref()
                .map(|s| s.as_str()),
            Some("another store's")
        );
    }

    /// A marked account moves a secret when it is read, and removing it
    /// deletes the old entry too.
    #[test]
    fn a_marked_account_moves_on_read_and_deletes_the_old_entry_on_removal() {
        let keychain = InMemoryKeychain::shared();
        keychain
            .write_entry("account-2", "password")
            .expect("written");
        keychain
            .write_entry("account-2-access-token", "token")
            .expect("written");
        keychain.clear_accesses();
        let store = StoreId::generate();
        let secrets = StoreSecrets::new(keychain.clone(), store, [2]);

        assert_eq!(
            secrets.get_account_secret(2).expect("moved").as_str(),
            "password"
        );
        assert_eq!(
            keychain
                .read_entry(&account_entry_name(store, 2, SecretSlot::Password))
                .expect("read")
                .as_ref()
                .map(|s| s.as_str()),
            Some("password")
        );
        assert_eq!(keychain.read_entry("account-2").expect("read"), None);

        secrets.clear_account_secrets(2).expect("cleared");
        assert!(
            keychain.entry_names().is_empty(),
            "{:?}",
            keychain.entry_names()
        );
        assert!(
            keychain.accesses().iter().all(
                |(access, name)| *access != KeychainAccess::Write || name.starts_with("store-")
            )
        );
    }

    #[test]
    fn a_keychain_that_loses_the_copy_keeps_the_old_entry() {
        /// Accepts writes and forgets them.
        struct Forgetful(Arc<InMemoryKeychain>);
        impl Keychain for Forgetful {
            fn read_entry(&self, name: &str) -> anyhow::Result<Option<zeroize::Zeroizing<String>>> {
                self.0.read_entry(name)
            }
            fn write_entry(&self, _name: &str, _secret: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn delete_entry(&self, name: &str) -> anyhow::Result<()> {
                self.0.delete_entry(name)
            }
        }
        let keychain = InMemoryKeychain::shared();
        keychain
            .write_entry("account-3", "password")
            .expect("written");
        let secrets = StoreSecrets::new(
            Arc::new(Forgetful(keychain.clone())),
            StoreId::generate(),
            [3],
        );
        assert!(secrets.claim_legacy_secrets(3).is_err());
        assert_eq!(
            keychain
                .read_entry("account-3")
                .expect("read")
                .as_ref()
                .map(|s| s.as_str()),
            Some("password")
        );
    }
}
