//! Recipient keys the user trusted although a contact card pins others.
//!
//! When a Proton contact card pins keys for a recipient and Proton serves
//! none of them, sending stops until the user trusts the served key for
//! that recipient (ADR 0004, "Pinned keys that no longer match"). Each
//! decision is one row: one account, one recipient address (lower-cased)
//! and one key fingerprint (upper-case hex). It never changes the contact
//! card on the server, and it is removed with the account.

use anyhow::{Context as _, Result, bail};
use rusqlite::params;
use zmail_core::key_change::KeyTrust;

use crate::LocalStore;

pub(crate) const KEY_TRUST_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS recipient_key_trust (
    account_id INTEGER NOT NULL,
    recipient TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (account_id, recipient, fingerprint),
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);
"#;

impl LocalStore {
    /// Records that the user trusts the key with `fingerprint` for mail
    /// from `account_id` to `recipient`. Recording it again changes
    /// nothing.
    pub fn trust_recipient_key(
        &self,
        account_id: i64,
        recipient: &str,
        fingerprint: &str,
    ) -> Result<()> {
        let recipient = recipient.trim().to_lowercase();
        if recipient.is_empty() {
            bail!("a key trust needs a recipient");
        }
        if fingerprint.is_empty() || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("a key fingerprint is hex digits");
        }
        self.connection()?
            .execute(
                "INSERT OR IGNORE INTO recipient_key_trust(account_id, recipient, fingerprint) \
                 VALUES (?1, ?2, ?3)",
                params![account_id, recipient, fingerprint.to_ascii_uppercase()],
            )
            .context("failed to record the key trust")?;
        Ok(())
    }

    /// The account's key trust decisions.
    pub fn trusted_recipient_keys(&self, account_id: i64) -> Result<Vec<KeyTrust>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT recipient, fingerprint FROM recipient_key_trust \
             WHERE account_id = ?1 ORDER BY recipient, fingerprint",
        )?;
        let rows = statement.query_map(params![account_id], |row| {
            Ok(KeyTrust {
                recipient: row.get(0)?,
                fingerprint: row.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load the key trusts")
    }
}
