//! Which store an account's keychain secrets belong to (#475, ADR 0005 §3,
//! "Account secrets").
//!
//! Every store on a machine shares the one keychain. Each store has a
//! random id of its own (`store_identity`), and its accounts' secrets are
//! named by it ([`zmail_security::StoreSecrets`]), so a store never reads,
//! replaces or deletes another store's. Within a store an account id is
//! never handed out twice: `last_account_id` remembers the highest id the
//! store has given, and a new account gets the next one even after the
//! accounts above it were removed, as SQLite's `AUTOINCREMENT` would.
//! (`accounts` can't be rebuilt with `AUTOINCREMENT` in a migration: the
//! runner keeps foreign keys on, and dropping the table would delete every
//! row that refers to an account.)
//!
//! Accounts already in a store when it was first opened by a build with
//! this change are marked `legacy_keychain_names`: only they may still
//! have secrets under the old names that left the store out, and only they
//! move them ([`LocalStore::account_secrets`]). An account added later,
//! to this store or any other, never touches the old names.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use rusqlite::{OptionalExtension, params};
use zmail_security::{Keychain, SecretStore, StoreId, StoreSecrets};

use crate::LocalStore;

pub(crate) const ACCOUNT_SECRETS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS store_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    store_id TEXT NOT NULL,
    last_account_id INTEGER NOT NULL
);

ALTER TABLE accounts ADD COLUMN legacy_keychain_names INTEGER NOT NULL DEFAULT 0;
UPDATE accounts SET legacy_keychain_names = 1;

CREATE TRIGGER IF NOT EXISTS accounts_remember_last_id AFTER INSERT ON accounts
BEGIN
    UPDATE store_identity SET last_account_id = MAX(last_account_id, NEW.id);
END;
"#;

/// The id [`LocalStore::insert_account`] gives a new account: one above
/// any the store has handed out, never one a removed account had.
pub(crate) const NEXT_ACCOUNT_ID: &str = "(SELECT MAX(\
     COALESCE((SELECT last_account_id FROM store_identity), 0), \
     COALESCE((SELECT MAX(id) FROM accounts), 0)) + 1)";

impl LocalStore {
    /// Gives the store its id the first time it opens with this schema:
    /// a random one, with the highest account id it has so far.
    pub(crate) fn ensure_store_identity(&self) -> Result<()> {
        self.connection()?
            .execute(
                "INSERT OR IGNORE INTO store_identity(singleton, store_id, last_account_id) \
                 VALUES (1, ?1, (SELECT COALESCE(MAX(id), 0) FROM accounts))",
                params![StoreId::generate().to_string()],
            )
            .context("failed to give the store its id")?;
        Ok(())
    }

    /// The store's id, which names its accounts' keychain entries.
    pub fn store_id(&self) -> Result<StoreId> {
        let id: String = self
            .connection()?
            .query_row(
                "SELECT store_id FROM store_identity WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("failed to read the store's id")?
            .context("the store has no id")?;
        id.parse().context("the store's id is unusable")
    }

    /// This store's account secrets in `keychain`: the [`SecretStore`] the
    /// app syncs, sends and sets up accounts with.
    ///
    /// First moves the secrets of the store's accounts marked
    /// `legacy_keychain_names` from the old names to the store's, and
    /// unmarks each account whose secrets all moved. An account whose move
    /// fails (a locked keychain) stays marked: the returned store moves
    /// each of its secrets when it is read, and the next call tries the
    /// whole account again.
    pub fn account_secrets(&self, keychain: Arc<dyn Keychain>) -> Result<Arc<dyn SecretStore>> {
        let legacy = self.accounts_with_legacy_keychain_names()?;
        let secrets = StoreSecrets::new(keychain, self.store_id()?, legacy.iter().copied());
        for account_id in legacy {
            if secrets.claim_legacy_secrets(account_id).is_ok() {
                self.connection()?
                    .execute(
                        "UPDATE accounts SET legacy_keychain_names = 0 WHERE id = ?1",
                        params![account_id],
                    )
                    .context("failed to record the account's secrets as moved")?;
            }
        }
        Ok(Arc::new(secrets))
    }

    /// The accounts whose secrets may still be under the old keychain
    /// names.
    fn accounts_with_legacy_keychain_names(&self) -> Result<Vec<i64>> {
        let conn = self.connection()?;
        let mut statement =
            conn.prepare("SELECT id FROM accounts WHERE legacy_keychain_names = 1 ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list the accounts with secrets under the old names")
    }
}
