//! Reviewing the senders whose remote content is always loaded.
//!
//! "Always for this sender" on a message's remote-content notice (ADR
//! 0006) stores the sender's lowercase address per account in
//! `remote_content_senders`; "Block" on a later message from them, or
//! Settings → Privacy, removes it. This module is what Settings lists and
//! removes; choosing a sender happens from a message
//! (`LocalStore::set_remote_content_for_message_sender`).

use anyhow::{Context as _, Result};
use rusqlite::params;

use crate::LocalStore;

/// A sender whose remote content the user chose to always load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteContentSender {
    /// The account the choice belongs to.
    pub account_id: i64,
    /// The sender's address, lowercase.
    pub address: String,
}

impl LocalStore {
    /// Every always-allowed sender, by account and then address.
    pub fn list_remote_content_senders(&self) -> Result<Vec<RemoteContentSender>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT account_id, address FROM remote_content_senders \
             ORDER BY account_id, address",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(RemoteContentSender {
                account_id: row.get(0)?,
                address: row.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load the remote-content choices")
    }

    /// Stops always loading remote content from `address` on `account_id`.
    /// Returns whether there was such a choice to remove.
    pub fn remove_remote_content_sender(&self, account_id: i64, address: &str) -> Result<bool> {
        let removed = self
            .connection()?
            .execute(
                "DELETE FROM remote_content_senders WHERE account_id = ?1 AND address = ?2",
                params![account_id, address.trim().to_ascii_lowercase()],
            )
            .context("failed to remove the remote-content choice")?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use zmail_core::ProtocolKind;

    use super::RemoteContentSender;
    use crate::LocalStore;

    fn allow(store: &LocalStore, account_id: i64, address: &str) {
        store
            .connection()
            .expect("the store opens")
            .execute(
                "INSERT INTO remote_content_senders(account_id, address) VALUES (?1, ?2)",
                rusqlite::params![account_id, address],
            )
            .expect("the choice stores");
    }

    #[test]
    fn lists_and_removes_always_allowed_senders() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let first = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let second = store
            .create_demo_account(ProtocolKind::Jmap)
            .expect("account should be created");
        assert!(
            store
                .list_remote_content_senders()
                .expect("the list loads")
                .is_empty()
        );
        allow(&store, second, "news@example.test");
        allow(&store, first, "shop@example.test");
        allow(&store, first, "alerts@example.test");

        let sender = |account_id, address: &str| RemoteContentSender {
            account_id,
            address: address.to_owned(),
        };
        assert_eq!(
            store.list_remote_content_senders().expect("the list loads"),
            vec![
                sender(first, "alerts@example.test"),
                sender(first, "shop@example.test"),
                sender(second, "news@example.test"),
            ]
        );

        assert!(
            store
                .remove_remote_content_sender(first, " Shop@Example.TEST ")
                .expect("the choice is removed"),
            "the address compares without case"
        );
        assert!(
            !store
                .remove_remote_content_sender(first, "shop@example.test")
                .expect("removing again works"),
            "nothing left to remove"
        );
        assert!(
            !store
                .remove_remote_content_sender(first, "news@example.test")
                .expect("removing works"),
            "another account's choice stays"
        );
        assert_eq!(
            store.list_remote_content_senders().expect("the list loads"),
            vec![
                sender(first, "alerts@example.test"),
                sender(second, "news@example.test"),
            ]
        );
        assert!(
            !store
                .remote_content_allowed_for_sender(first, "shop@example.test")
                .expect("the choice loads")
        );
    }
}
