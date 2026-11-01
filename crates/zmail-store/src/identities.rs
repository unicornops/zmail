//! Identities kept in step with a JMAP server's (`zmail_core::identity_sync`).
//!
//! Each identity row keeps, beside the user's values (`display_name`,
//! `reply_to`, `signature`), the server identity it is linked to
//! (`server_id`) and that identity's values as last seen (`server_name`,
//! `server_reply_to`, `server_signature`). `server_state` holds what the
//! values alone can't tell: `conflict` when the last sync found both sides
//! changed, `removed` when the server no longer lists the identity, and
//! empty otherwise. Whether an identity differs from the server is the
//! comparison of its values with the server's last seen, so an edit made
//! between syncs shows at once.
//!
//! IMAP and Proton accounts never link their identities, so all of this
//! leaves them alone.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, bail};
use rusqlite::{OptionalExtension as _, Transaction, TransactionBehavior, params};
use zmail_core::{
    IdentitySummary,
    identity_sync::{
        IdentitySyncStatus, IdentityValues, Merge, ServerIdentity, SyncedIdentity, fill_blanks,
        merge,
    },
};

use crate::{LocalStore, primary_identity_id};

/// The local values and the server bookkeeping of identities (migration
/// 0027).
pub(crate) const IDENTITY_SERVER_SYNC_SCHEMA: &str = r#"
ALTER TABLE identities ADD COLUMN reply_to TEXT NOT NULL DEFAULT '';
ALTER TABLE identities ADD COLUMN server_id TEXT;
ALTER TABLE identities ADD COLUMN server_name TEXT;
ALTER TABLE identities ADD COLUMN server_reply_to TEXT;
ALTER TABLE identities ADD COLUMN server_signature TEXT;
ALTER TABLE identities ADD COLUMN server_state TEXT NOT NULL DEFAULT '';
"#;

const CONFLICT: &str = "conflict";
const REMOVED: &str = "removed";

/// What merging the server's identities changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityMergeReport {
    /// The addresses of the identities added from the server.
    pub imported: Vec<String>,
    /// Identities that differ from the server and did not before.
    pub newly_differing: usize,
    /// Identities the server stopped listing in this sync.
    pub newly_removed: usize,
    /// Server identities left out because the account already has an
    /// identity with their address (Zmail keeps one per address).
    pub skipped: Vec<String>,
}

/// What pushing an identity to the server needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityLink {
    /// The account it belongs to.
    pub account_id: i64,
    /// Its address.
    pub email: String,
    /// The server identity it is linked to, if any.
    pub server_id: Option<String>,
    /// Its values in Zmail.
    pub local: IdentityValues,
    /// How it stands.
    pub status: IdentitySyncStatus,
}

/// One identity row with its bookkeeping.
#[derive(Debug, Clone)]
struct Row {
    id: i64,
    email: String,
    is_primary: bool,
    local: IdentityValues,
    server_id: Option<String>,
    seen: Option<IdentityValues>,
    state: String,
}

impl Row {
    fn status(&self) -> IdentitySyncStatus {
        let (Some(_), Some(seen)) = (&self.server_id, &self.seen) else {
            return IdentitySyncStatus::Local;
        };
        if self.state == REMOVED {
            IdentitySyncStatus::NotOnServer
        } else if self.local == *seen {
            IdentitySyncStatus::InSync
        } else if self.state == CONFLICT {
            IdentitySyncStatus::Conflict
        } else {
            IdentitySyncStatus::ChangedHere
        }
    }
}

const ROW_COLUMNS: &str = "id, email_address, is_primary, display_name, reply_to, signature, \
     server_id, server_name, server_reply_to, server_signature, server_state";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let server_name: Option<String> = row.get(7)?;
    let server_reply_to: Option<String> = row.get(8)?;
    let server_signature: Option<String> = row.get(9)?;
    Ok(Row {
        id: row.get(0)?,
        email: row.get(1)?,
        is_primary: row.get(2)?,
        local: IdentityValues {
            name: row.get(3)?,
            reply_to: row.get(4)?,
            signature: row.get(5)?,
        },
        server_id: row.get(6)?,
        seen: server_name.map(|name| IdentityValues {
            name,
            reply_to: server_reply_to.unwrap_or_default(),
            signature: server_signature.unwrap_or_default(),
        }),
        state: row.get(10)?,
    })
}

fn load_rows(conn: &rusqlite::Connection, account_id: i64) -> Result<Vec<Row>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} FROM identities WHERE account_id = ?1 \
         ORDER BY is_primary DESC, id ASC"
    ))?;
    let rows = statement.query_map(params![account_id], read_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list the identities")
}

fn load_row(conn: &rusqlite::Connection, identity_id: i64) -> Result<Option<(i64, Row)>> {
    conn.query_row(
        &format!("SELECT account_id, {ROW_COLUMNS} FROM identities WHERE id = ?1"),
        params![identity_id],
        |row| {
            let account_id = row.get(0)?;
            // The row's columns follow the account's.
            let server_name: Option<String> = row.get(8)?;
            let server_reply_to: Option<String> = row.get(9)?;
            let server_signature: Option<String> = row.get(10)?;
            Ok((
                account_id,
                Row {
                    id: row.get(1)?,
                    email: row.get(2)?,
                    is_primary: row.get(3)?,
                    local: IdentityValues {
                        name: row.get(4)?,
                        reply_to: row.get(5)?,
                        signature: row.get(6)?,
                    },
                    server_id: row.get(7)?,
                    seen: server_name.map(|name| IdentityValues {
                        name,
                        reply_to: server_reply_to.unwrap_or_default(),
                        signature: server_signature.unwrap_or_default(),
                    }),
                    state: row.get(11)?,
                },
            ))
        },
    )
    .optional()
    .context("failed to load the identity")
}

/// Writes an identity's values and its link to `server`.
fn write_linked(
    transaction: &Transaction<'_>,
    identity_id: i64,
    local: &IdentityValues,
    server: &ServerIdentity,
    state: &str,
) -> Result<()> {
    transaction
        .execute(
            "UPDATE identities SET display_name = ?2, reply_to = ?3, signature = ?4, \
             server_id = ?5, server_name = ?6, server_reply_to = ?7, server_signature = ?8, \
             server_state = ?9 WHERE id = ?1",
            params![
                identity_id,
                local.name,
                local.reply_to,
                local.signature,
                server.id,
                server.values.name,
                server.values.reply_to,
                server.values.signature,
                state,
            ],
        )
        .context("failed to update the identity from the server")?;
    Ok(())
}

impl LocalStore {
    /// Merges the identities a JMAP server lists into the account's
    /// (`zmail_core::identity_sync`): links each to the local identity
    /// with its server id, else with its address; adds the ones Zmail
    /// lacks; takes the server's changes where Zmail's values are
    /// untouched, and otherwise keeps Zmail's and marks them as differing.
    /// Identities the server stopped listing are kept and marked, never
    /// deleted.
    pub fn merge_server_identities(
        &self,
        account_id: i64,
        server: &[ServerIdentity],
    ) -> Result<IdentityMergeReport> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the identity merge")?;
        let rows = load_rows(&transaction, account_id)?;
        let before: HashMap<i64, IdentitySyncStatus> =
            rows.iter().map(|row| (row.id, row.status())).collect();
        let listed: HashSet<&str> = server.iter().map(|identity| identity.id.as_str()).collect();
        // Zmail keeps one identity per address. When a server lists
        // several for one (Apache James lists the one it derives from an
        // alias beside the user's own), the ones the user may delete, the
        // user's own, come first.
        let mut server: Vec<&ServerIdentity> = server.iter().collect();
        server.sort_by_key(|identity| !identity.may_delete);
        let mut report = IdentityMergeReport::default();

        // Each server identity's local row: by server id first, then by
        // address among the rows not linked to anything the server lists.
        let mut claimed: HashSet<i64> = HashSet::new();
        let mut matched: Vec<Option<usize>> = server
            .iter()
            .map(|identity| {
                let index = rows.iter().position(|row| {
                    row.server_id.as_deref() == Some(identity.id.as_str())
                        && !claimed.contains(&row.id)
                })?;
                claimed.insert(rows[index].id);
                Some(index)
            })
            .collect();
        for (identity, slot) in server.iter().zip(matched.iter_mut()) {
            if slot.is_some() {
                continue;
            }
            *slot = rows.iter().position(|row| {
                row.email.eq_ignore_ascii_case(&identity.email)
                    && !claimed.contains(&row.id)
                    && row
                        .server_id
                        .as_deref()
                        .is_none_or(|id| !listed.contains(id))
            });
            if let Some(index) = *slot {
                claimed.insert(rows[index].id);
            }
        }

        for (identity, slot) in server.iter().zip(&matched) {
            let Some(index) = *slot else {
                if rows
                    .iter()
                    .any(|row| row.email.eq_ignore_ascii_case(&identity.email))
                    || report
                        .imported
                        .iter()
                        .any(|email| email.eq_ignore_ascii_case(&identity.email))
                {
                    report.skipped.push(identity.email.clone());
                    continue;
                }
                transaction
                    .execute(
                        "INSERT INTO identities(account_id, display_name, email_address, \
                         signature, is_primary, reply_to, server_id, server_name, \
                         server_reply_to, server_signature, server_state) \
                         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?2, ?5, ?4, '')",
                        params![
                            account_id,
                            identity.values.name,
                            identity.email,
                            identity.values.signature,
                            identity.values.reply_to,
                            identity.id,
                        ],
                    )
                    .context("failed to add the server's identity")?;
                report.imported.push(identity.email.clone());
                continue;
            };
            let row = &rows[index];
            let linked = row.server_id.as_deref() == Some(identity.id.as_str());
            let (local, last_seen) = if linked {
                (row.local.clone(), row.seen.as_ref())
            } else {
                (fill_blanks(&row.local, &identity.values), None)
            };
            let was_conflict = linked && row.state == CONFLICT;
            let (local, state) = match merge(&local, last_seen, &identity.values) {
                Merge::InSync => (local, ""),
                Merge::TakeServer => (identity.values.clone(), ""),
                Merge::KeepLocal { conflict } => (
                    local,
                    if conflict || was_conflict {
                        CONFLICT
                    } else {
                        ""
                    },
                ),
            };
            write_linked(&transaction, row.id, &local, identity, state)?;
        }

        for row in &rows {
            let gone = row
                .server_id
                .as_deref()
                .is_some_and(|id| !listed.contains(id))
                && !claimed.contains(&row.id);
            if gone && row.state != REMOVED {
                transaction
                    .execute(
                        "UPDATE identities SET server_state = ?2 WHERE id = ?1",
                        params![row.id, REMOVED],
                    )
                    .context("failed to mark the identity as gone from the server")?;
            }
        }

        for row in load_rows(&transaction, account_id)? {
            let previous = before.get(&row.id).copied();
            let status = row.status();
            if status.differs() && !previous.is_some_and(IdentitySyncStatus::differs) {
                report.newly_differing += 1;
            }
            if status == IdentitySyncStatus::NotOnServer
                && previous != Some(IdentitySyncStatus::NotOnServer)
            {
                report.newly_removed += 1;
            }
        }
        transaction
            .commit()
            .context("failed to commit the identity merge")?;
        Ok(report)
    }

    /// The account's identities with how each stands against the server,
    /// primary first.
    pub fn list_synced_identities(&self, account_id: i64) -> Result<Vec<SyncedIdentity>> {
        let conn = self.connection()?;
        Ok(load_rows(&conn, account_id)?
            .into_iter()
            .map(|row| {
                let status = row.status();
                SyncedIdentity {
                    identity: IdentitySummary {
                        id: row.id,
                        display_name: row.local.name.clone(),
                        email_address: row.email,
                        signature: row.local.signature.clone(),
                        is_primary: row.is_primary,
                    },
                    local: row.local,
                    server: row.seen.filter(|_| row.server_id.is_some()),
                    status,
                }
            })
            .collect())
    }

    /// Changes an identity's name, reply-to and signature in Zmail. A
    /// linked identity then differs from the server until the user updates
    /// the server or takes the server's values.
    pub fn update_identity(&self, identity_id: i64, values: &IdentityValues) -> Result<()> {
        let changed = self
            .connection()?
            .execute(
                "UPDATE identities SET display_name = ?2, reply_to = ?3, signature = ?4 \
                 WHERE id = ?1",
                params![identity_id, values.name, values.reply_to, values.signature],
            )
            .context("failed to update the identity")?;
        if changed == 0 {
            bail!("that identity no longer exists");
        }
        Ok(())
    }

    /// "Use the server's": the identity takes the server's values as last
    /// seen.
    pub fn use_server_identity(&self, identity_id: i64) -> Result<()> {
        let conn = self.connection()?;
        let Some((_, row)) = load_row(&conn, identity_id)? else {
            bail!("that identity no longer exists");
        };
        let (Some(_), Some(seen)) = (&row.server_id, &row.seen) else {
            bail!("{} is not on the server", row.email);
        };
        if row.state == REMOVED {
            bail!("{} is no longer on the server", row.email);
        }
        conn.execute(
            "UPDATE identities SET display_name = ?2, reply_to = ?3, signature = ?4, \
             server_state = '' WHERE id = ?1",
            params![identity_id, seen.name, seen.reply_to, seen.signature],
        )
        .context("failed to take the server's identity")?;
        Ok(())
    }

    /// What updating the server with an identity (or creating it there)
    /// needs; `None` when it no longer exists.
    pub fn identity_link(&self, identity_id: i64) -> Result<Option<IdentityLink>> {
        let conn = self.connection()?;
        Ok(load_row(&conn, identity_id)?.map(|(account_id, row)| {
            let status = row.status();
            IdentityLink {
                account_id,
                email: row.email,
                server_id: row.server_id,
                local: row.local,
                status,
            }
        }))
    }

    /// Records that the server now holds `values` for the identity, as
    /// server identity `server_id`: after updating the server with it, or
    /// creating it there again.
    pub fn record_identity_on_server(
        &self,
        identity_id: i64,
        server_id: &str,
        values: &IdentityValues,
    ) -> Result<()> {
        self.connection()?
            .execute(
                "UPDATE identities SET server_id = ?2, server_name = ?3, server_reply_to = ?4, \
                 server_signature = ?5, server_state = '' WHERE id = ?1",
                params![
                    identity_id,
                    server_id,
                    values.name,
                    values.reply_to,
                    values.signature,
                ],
            )
            .context("failed to record the server's identity")?;
        Ok(())
    }

    /// Removes an identity from Zmail (not from the server): offered for
    /// one the server no longer lists. Its mail is filed under the
    /// account's primary identity. The primary identity, the last one and
    /// one a draft is written from are kept.
    pub fn remove_identity(&self, identity_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin removing the identity")?;
        let Some((account_id, row)) = load_row(&transaction, identity_id)? else {
            bail!("that identity no longer exists");
        };
        if row.is_primary {
            bail!(
                "{} is the account's primary identity, so it stays",
                row.email
            );
        }
        let drafts: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM drafts WHERE identity_id = ?1",
            params![identity_id],
            |row| row.get(0),
        )?;
        if drafts > 0 {
            bail!(
                "a draft is written from {}: send or delete it first",
                row.email
            );
        }
        let primary = primary_identity_id(&transaction, account_id)?
            .filter(|primary| *primary != identity_id)
            .with_context(|| format!("{} is the account's only identity", row.email))?;
        transaction
            .execute(
                "UPDATE messages SET identity_id = ?2 WHERE identity_id = ?1",
                params![identity_id, primary],
            )
            .context("failed to file the identity's mail under the primary identity")?;
        transaction
            .execute("DELETE FROM identities WHERE id = ?1", params![identity_id])
            .context("failed to remove the identity")?;
        transaction
            .commit()
            .context("failed to commit removing the identity")
    }
}

#[cfg(test)]
mod tests {
    use zmail_core::{
        AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind,
        identity_sync::{IdentitySyncStatus, IdentityValues, ServerIdentity},
    };

    use crate::LocalStore;

    fn values(name: &str, signature: &str) -> IdentityValues {
        IdentityValues {
            name: name.to_owned(),
            reply_to: String::new(),
            signature: signature.to_owned(),
        }
    }

    fn server(id: &str, email: &str, values: IdentityValues) -> ServerIdentity {
        ServerIdentity {
            id: id.to_owned(),
            email: email.to_owned(),
            values,
            may_delete: true,
        }
    }

    fn jmap_account(store: &LocalStore) -> i64 {
        store
            .insert_account(&NewAccount {
                display_name: "Fastmail".to_owned(),
                protocol: ProtocolKind::Jmap,
                auth: AuthKind::Password,
                endpoint: "https://api.example.com/jmap/session".to_owned(),
                username: "rob@example.com".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: None,
                identities: vec![NewIdentity {
                    display_name: "Rob".to_owned(),
                    email_address: "rob@example.com".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account is stored")
    }

    fn status_of(store: &LocalStore, account_id: i64, email: &str) -> IdentitySyncStatus {
        store
            .list_synced_identities(account_id)
            .expect("identities load")
            .into_iter()
            .find(|identity| identity.identity.email_address == email)
            .expect("the identity exists")
            .status
    }

    #[test]
    fn a_sync_links_imports_and_classifies_each_identity() {
        let store = LocalStore::open_ephemeral().expect("the store opens");
        let account = jmap_account(&store);
        let primary = store.list_identities(account).expect("identities")[0].id;
        assert_eq!(
            status_of(&store, account, "rob@example.com"),
            IdentitySyncStatus::Local
        );

        // First sync: the existing identity links by address (its blank
        // signature filled in, its name the same), and a new one is added.
        let mut listed = vec![
            server("i1", "ROB@example.com", values("Rob", "-- Rob")),
            server("i2", "help@example.com", values("Help desk", "")),
        ];
        let report = store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert_eq!(report.imported, ["help@example.com"]);
        assert_eq!(report.newly_differing, 0);
        let identities = store.list_synced_identities(account).expect("identities");
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0].identity.id, primary);
        assert_eq!(identities[0].local, values("Rob", "-- Rob"));
        assert_eq!(identities[0].status, IdentitySyncStatus::InSync);
        assert_eq!(identities[1].status, IdentitySyncStatus::InSync);
        assert!(!identities[1].identity.is_primary);

        // Only the server changed: Zmail takes it.
        listed[0].values.name = "Rob Lazzurs".to_owned();
        store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert_eq!(
            store.list_identities(account).expect("identities")[0].display_name,
            "Rob Lazzurs"
        );

        // Only Zmail changed: kept, and differing at once.
        let edited = values("Rob Lazzurs", "-- Rob, from Zmail");
        store.update_identity(primary, &edited).expect("edited");
        assert_eq!(
            status_of(&store, account, "rob@example.com"),
            IdentitySyncStatus::ChangedHere
        );
        let report = store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert_eq!(report.newly_differing, 0, "it differed before the sync");
        let identities = store.list_synced_identities(account).expect("identities");
        assert_eq!(identities[0].local, edited, "the local edit is kept");
        assert_eq!(identities[0].status, IdentitySyncStatus::ChangedHere);

        // Both changed: still Zmail's values, now a conflict.
        listed[0].values.signature = "-- Rob, from the web".to_owned();
        store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        let identities = store.list_synced_identities(account).expect("identities");
        assert_eq!(identities[0].local, edited);
        assert_eq!(identities[0].status, IdentitySyncStatus::Conflict);
        assert_eq!(
            identities[0]
                .server
                .as_ref()
                .map(|server| &server.signature[..]),
            Some("-- Rob, from the web")
        );

        // "Use the server's" resolves it.
        store.use_server_identity(primary).expect("taken");
        assert_eq!(
            status_of(&store, account, "rob@example.com"),
            IdentitySyncStatus::InSync
        );

        // Removed on the server: kept and marked, then removable.
        let report = store
            .merge_server_identities(account, &listed[..1])
            .expect("the merge runs");
        assert_eq!(report.newly_removed, 1);
        assert_eq!(
            status_of(&store, account, "help@example.com"),
            IdentitySyncStatus::NotOnServer
        );
        let help = store.list_synced_identities(account).expect("identities")[1]
            .identity
            .id;
        assert!(store.use_server_identity(help).is_err());
        assert!(store.remove_identity(primary).is_err(), "the primary stays");
        store.remove_identity(help).expect("removed");
        assert_eq!(store.list_identities(account).expect("identities").len(), 1);
    }

    #[test]
    fn a_first_link_that_disagrees_is_a_conflict_and_pushing_settles_it() {
        let store = LocalStore::open_ephemeral().expect("the store opens");
        let account = jmap_account(&store);
        let primary = store.list_identities(account).expect("identities")[0].id;
        let listed = [server("i1", "rob@example.com", values("Robert", ""))];
        let report = store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert_eq!(report.newly_differing, 1);
        assert_eq!(
            status_of(&store, account, "rob@example.com"),
            IdentitySyncStatus::Conflict
        );
        assert_eq!(
            store.list_identities(account).expect("identities")[0].display_name,
            "Rob",
            "Zmail's name is kept"
        );

        let link = store
            .identity_link(primary)
            .expect("loads")
            .expect("exists");
        assert_eq!(link.server_id.as_deref(), Some("i1"));
        store
            .record_identity_on_server(primary, "i1", &link.local)
            .expect("recorded");
        assert_eq!(
            status_of(&store, account, "rob@example.com"),
            IdentitySyncStatus::InSync
        );
        // The next sync, with the server holding Zmail's values, agrees.
        let listed = [server("i1", "rob@example.com", link.local.clone())];
        let report = store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert_eq!(report, Default::default());
    }

    #[test]
    fn a_second_server_identity_for_an_address_is_skipped() {
        let store = LocalStore::open_ephemeral().expect("the store opens");
        let account = jmap_account(&store);
        let listed = [
            server("i1", "rob@example.com", values("Rob", "")),
            server("i2", "rob@example.com", values("Rob at work", "")),
        ];
        let report = store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert_eq!(report.skipped, ["rob@example.com"]);
        assert_eq!(store.list_identities(account).expect("identities").len(), 1);

        // Of a server-made identity and the user's own for one address,
        // listed in either order, the user's own is imported.
        for reversed in [false, true] {
            let store = LocalStore::open_ephemeral().expect("the store opens");
            let account = jmap_account(&store);
            let mut derived = server("d1", "alias@example.com", values("alias@example.com", ""));
            derived.may_delete = false;
            let mut listed = vec![
                derived,
                server("u1", "alias@example.com", values("Alias", "")),
            ];
            if reversed {
                listed.reverse();
            }
            let report = store
                .merge_server_identities(account, &listed)
                .expect("the merge runs");
            assert_eq!(report.imported, ["alias@example.com"]);
            let link = store
                .identity_link(store.list_identities(account).expect("identities")[1].id)
                .expect("loads")
                .expect("exists");
            assert_eq!(link.server_id.as_deref(), Some("u1"));
            assert_eq!(link.local.name, "Alias");
        }
    }
}
