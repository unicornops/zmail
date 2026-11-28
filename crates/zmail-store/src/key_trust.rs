//! Recipient keys the user trusted although a contact card pins others.
//!
//! When a Proton contact card pins keys for a recipient and Proton serves
//! none of them, sending stops until the user trusts the served key for
//! that recipient (ADR 0004, "Pinned keys that no longer match"). Each
//! decision is one row: one account, one recipient address (lower-cased)
//! and one key fingerprint (upper-case hex). It never changes the contact
//! card on the server, and it is removed with the account.
//!
//! Beside them, `recipient_key_pins` remembers what a recipient's verified
//! contact card pinned and whether it required encryption, the last time a
//! send used it (ADR 0004, "Contact cards that go missing"): one row per
//! account and recipient address (lower-cased), the fingerprints
//! space-separated in upper-case hex. A later send whose card is gone
//! still keeps to them. The user can forget a recipient's rows, and they
//! are removed with the account.

use anyhow::{Context as _, Result, bail};
use rusqlite::params;
use zmail_core::key_change::{KeyTrust, RecipientPins};

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

pub(crate) const KEY_PINS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS recipient_key_pins (
    account_id INTEGER NOT NULL,
    recipient TEXT NOT NULL,
    fingerprints TEXT NOT NULL,
    encrypt INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (account_id, recipient),
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);
"#;

/// Whether `fingerprint` is a key fingerprint: hex digits.
fn is_fingerprint(fingerprint: &str) -> bool {
    !fingerprint.is_empty() && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
}

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
        if !is_fingerprint(fingerprint) {
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

    /// Remembers what a verified contact card asked for mail from
    /// `account_id` to `pins.recipient`, replacing what was remembered
    /// before.
    pub fn remember_recipient_pins(&self, account_id: i64, pins: &RecipientPins) -> Result<()> {
        let pins = RecipientPins::new(&pins.recipient, pins.fingerprints.clone(), pins.encrypt);
        if pins.recipient.is_empty() {
            bail!("remembered pins need a recipient");
        }
        if !pins
            .fingerprints
            .iter()
            .all(|fingerprint| is_fingerprint(fingerprint))
        {
            bail!("a key fingerprint is hex digits");
        }
        self.connection()?
            .execute(
                "INSERT INTO recipient_key_pins(account_id, recipient, fingerprints, encrypt) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(account_id, recipient) DO UPDATE SET \
                 fingerprints = excluded.fingerprints, encrypt = excluded.encrypt, \
                 updated_at = CURRENT_TIMESTAMP",
                params![
                    account_id,
                    pins.recipient,
                    pins.fingerprints.join(" "),
                    pins.encrypt
                ],
            )
            .context("failed to remember the recipient's pinned keys")?;
        Ok(())
    }

    /// The pins remembered for the account's recipients, by address.
    pub fn remembered_recipient_pins(&self, account_id: i64) -> Result<Vec<RecipientPins>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT recipient, fingerprints, encrypt FROM recipient_key_pins \
             WHERE account_id = ?1 ORDER BY recipient",
        )?;
        let rows = statement.query_map(params![account_id], |row| {
            let fingerprints: String = row.get(1)?;
            Ok(RecipientPins {
                recipient: row.get(0)?,
                fingerprints: fingerprints.split_whitespace().map(str::to_owned).collect(),
                encrypt: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load the remembered recipient keys")
    }

    /// Forgets every key Zmail saved for mail from `account_id` to
    /// `recipient`: the pins it remembers and the keys the user trusted.
    /// The contact card on the server is not changed.
    pub fn forget_recipient_keys(&self, account_id: i64, recipient: &str) -> Result<()> {
        let recipient = recipient.trim().to_lowercase();
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction()
            .context("failed to begin forgetting the recipient's keys")?;
        for table in ["recipient_key_pins", "recipient_key_trust"] {
            transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE account_id = ?1 AND recipient = ?2"),
                    params![account_id, recipient],
                )
                .context("failed to forget the recipient's keys")?;
        }
        transaction
            .commit()
            .context("failed to forget the recipient's keys")
    }
}
