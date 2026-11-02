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
//! `server_may_delete` (migration 0028) keeps the server's `mayDelete`
//! for the linked identity, so removing one can offer to delete it on the
//! server too without asking the server first.
//!
//! Removing a linked identity from Zmail only, while the server still
//! lists it, leaves a marker in `hidden_identities` (migration 0029; Rob's
//! decision of 1 November 2026, #366): the server identity's id, address
//! and values as last seen. A sync leaves a hidden server identity out
//! instead of importing it again. It matches a marker by server id, else
//! by address: a server that re-creates the identity under a new id (or,
//! like Apache James, lists the identity it derives from the alias once
//! the user's own is gone) is still the address the user hid, and the
//! marker follows the new id. A marker the server no longer lists at all
//! was deleted on the server, so it goes. Bring back
//! ([`LocalStore::bring_back_identity`]) puts the identity back at once,
//! linked, with the server's values as last seen; the next sync confirms
//! them. Adding an identity for a hidden address brings it back too.
//!
//! IMAP and Proton accounts never link their identities, so all of this
//! leaves them alone. The editor's writes ([`LocalStore::add_identity`],
//! [`LocalStore::set_identity_address`], [`LocalStore::set_primary_identity`]
//! and [`LocalStore::update_identity`]) work for every account; the rules
//! about which an account allows are `zmail_setup::identity_editor`'s.

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

/// The server's `mayDelete` for the linked identity (migration 0028).
pub(crate) const IDENTITY_SERVER_MAY_DELETE_SCHEMA: &str = r#"
ALTER TABLE identities ADD COLUMN server_may_delete INTEGER NOT NULL DEFAULT 0;
"#;

/// Server identities removed from Zmail only (migration 0029).
pub(crate) const HIDDEN_IDENTITIES_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS hidden_identities (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    server_id TEXT NOT NULL,
    email TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    reply_to TEXT NOT NULL DEFAULT '',
    signature TEXT NOT NULL DEFAULT '',
    may_delete INTEGER NOT NULL DEFAULT 0,
    hidden_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_hidden_identities_account ON hidden_identities(account_id);
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
    /// Server identities left out because the user hid them.
    pub hidden: Vec<String>,
}

/// A server identity the user removed from Zmail only, which syncs leave
/// out until the user brings it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenIdentity {
    /// The marker's id, for [`LocalStore::bring_back_identity`].
    pub id: i64,
    /// The account.
    pub account_id: i64,
    /// The server identity's id, as last seen.
    pub server_id: String,
    /// Its address.
    pub email: String,
    /// Its name, reply-to and signature on the server, as last seen.
    pub values: IdentityValues,
    /// Whether the server lets the user delete it.
    pub may_delete: bool,
}

const HIDDEN_COLUMNS: &str =
    "id, account_id, server_id, email, name, reply_to, signature, may_delete";

fn read_hidden(row: &rusqlite::Row<'_>) -> rusqlite::Result<HiddenIdentity> {
    Ok(HiddenIdentity {
        id: row.get(0)?,
        account_id: row.get(1)?,
        server_id: row.get(2)?,
        email: row.get(3)?,
        values: IdentityValues {
            name: row.get(4)?,
            reply_to: row.get(5)?,
            signature: row.get(6)?,
        },
        may_delete: row.get(7)?,
    })
}

fn load_hidden(conn: &rusqlite::Connection, account_id: i64) -> Result<Vec<HiddenIdentity>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {HIDDEN_COLUMNS} FROM hidden_identities WHERE account_id = ?1 ORDER BY email, id"
    ))?;
    let rows = statement.query_map(params![account_id], read_hidden)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list the hidden identities")
}

/// The hidden marker `server` matches, if any: by server id, else by
/// address (the module's matching rule).
fn hidden_match<'a>(
    hidden: &'a [HiddenIdentity],
    server: &ServerIdentity,
) -> Option<&'a HiddenIdentity> {
    hidden
        .iter()
        .find(|marker| marker.server_id == server.id)
        .or_else(|| {
            hidden
                .iter()
                .find(|marker| marker.email.eq_ignore_ascii_case(&server.email))
        })
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
    /// Whether the server lets the user delete the linked server identity.
    pub may_delete: bool,
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
    may_delete: bool,
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
     server_id, server_name, server_reply_to, server_signature, server_state, server_may_delete";

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
        may_delete: row.get(11)?,
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
                    may_delete: row.get(12)?,
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
             server_state = ?9, server_may_delete = ?10 WHERE id = ?1",
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
                server.may_delete,
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

        // Hidden server identities stay out. Each marker follows the server
        // identity it matches (its id, address and latest values); a marker
        // matching nothing was deleted on the server, so it goes.
        let hidden = load_hidden(&transaction, account_id)?;
        let mut followed: HashMap<i64, &ServerIdentity> = HashMap::new();
        for identity in &server {
            if let Some(marker) = hidden_match(&hidden, identity)
                && !followed.contains_key(&marker.id)
            {
                followed.insert(marker.id, identity);
            }
        }
        for marker in &hidden {
            match followed.get(&marker.id) {
                Some(identity) => transaction
                    .execute(
                        "UPDATE hidden_identities SET server_id = ?2, email = ?3, name = ?4, \
                         reply_to = ?5, signature = ?6, may_delete = ?7 WHERE id = ?1",
                        params![
                            marker.id,
                            identity.id,
                            identity.email,
                            identity.values.name,
                            identity.values.reply_to,
                            identity.values.signature,
                            identity.may_delete,
                        ],
                    )
                    .context("failed to update a hidden identity")?,
                None => transaction
                    .execute(
                        "DELETE FROM hidden_identities WHERE id = ?1",
                        params![marker.id],
                    )
                    .context("failed to forget a hidden identity the server deleted")?,
            };
        }
        // Every server identity a marker matches stays out, not just the
        // one it follows (a server may list two for the address).
        server.retain(|identity| {
            let hide = hidden_match(&hidden, identity).is_some();
            if hide {
                report.hidden.push(identity.email.clone());
            }
            !hide
        });

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
                         server_reply_to, server_signature, server_state, server_may_delete) \
                         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?2, ?5, ?4, '', ?7)",
                        params![
                            account_id,
                            identity.values.name,
                            identity.email,
                            identity.values.signature,
                            identity.values.reply_to,
                            identity.id,
                            identity.may_delete,
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
                let linked = row.server_id.is_some();
                SyncedIdentity {
                    identity: IdentitySummary {
                        id: row.id,
                        display_name: row.local.name.clone(),
                        email_address: row.email,
                        signature: row.local.signature.clone(),
                        reply_to: row.local.reply_to.clone(),
                        is_primary: row.is_primary,
                    },
                    local: row.local,
                    server: row.seen.filter(|_| linked),
                    status,
                    may_delete_on_server: linked && row.may_delete,
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
                may_delete: row.may_delete,
            }
        }))
    }

    /// Records that the server now holds `values` for the identity, as
    /// server identity `server_id`: after updating the server with it, or
    /// creating it there (`created`: an identity the user made, which the
    /// user may delete again).
    pub fn record_identity_on_server(
        &self,
        identity_id: i64,
        server_id: &str,
        values: &IdentityValues,
        created: bool,
    ) -> Result<()> {
        self.connection()?
            .execute(
                "UPDATE identities SET server_id = ?2, server_name = ?3, server_reply_to = ?4, \
                 server_signature = ?5, server_state = '', \
                 server_may_delete = CASE WHEN ?6 THEN 1 ELSE server_may_delete END \
                 WHERE id = ?1",
                params![
                    identity_id,
                    server_id,
                    values.name,
                    values.reply_to,
                    values.signature,
                    created,
                ],
            )
            .context("failed to record the server's identity")?;
        Ok(())
    }

    /// Adds an identity the user made to an account, with `values`; it
    /// becomes the primary identity only when the account has none.
    /// Returns its id. The address must be new to the account (Zmail keeps
    /// one identity per address).
    pub fn add_identity(
        &self,
        account_id: i64,
        email: &str,
        values: &IdentityValues,
    ) -> Result<i64> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin adding the identity")?;
        ensure_address_is_new(&transaction, account_id, email, None)?;
        // Adding a hidden address back brings it back: the next sync links
        // the new identity to the server's.
        transaction
            .execute(
                "DELETE FROM hidden_identities WHERE account_id = ?1 AND email = ?2 COLLATE NOCASE",
                params![account_id, email],
            )
            .context("failed to bring the hidden identity back")?;
        let has_primary = primary_identity_id(&transaction, account_id)?.is_some();
        transaction
            .execute(
                "INSERT INTO identities(account_id, display_name, email_address, signature, \
                 is_primary, reply_to) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    account_id,
                    values.name,
                    email,
                    values.signature,
                    !has_primary,
                    values.reply_to,
                ],
            )
            .context("failed to add the identity")?;
        let identity_id = transaction.last_insert_rowid();
        transaction
            .commit()
            .context("failed to commit adding the identity")?;
        Ok(identity_id)
    }

    /// Changes the address of an identity that isn't linked to a server
    /// identity (a linked one's address is the server's, RFC 8621). The new
    /// address must be new to the account. Drafts keep the address they
    /// started with.
    pub fn set_identity_address(&self, identity_id: i64, email: &str) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin changing the address")?;
        let Some((account_id, row)) = load_row(&transaction, identity_id)? else {
            bail!("that identity no longer exists");
        };
        if row.email == email {
            return Ok(());
        }
        if row.server_id.is_some() {
            bail!("{}'s address is set by the server", row.email);
        }
        ensure_address_is_new(&transaction, account_id, email, Some(identity_id))?;
        transaction
            .execute(
                "UPDATE identities SET email_address = ?2 WHERE id = ?1",
                params![identity_id, email],
            )
            .context("failed to change the address")?;
        transaction
            .commit()
            .context("failed to commit changing the address")
    }

    /// Makes an identity its account's primary one: the identity new mail
    /// starts from, and the one mail from a removed identity is filed
    /// under.
    pub fn set_primary_identity(&self, identity_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin changing the primary identity")?;
        let Some((account_id, _)) = load_row(&transaction, identity_id)? else {
            bail!("that identity no longer exists");
        };
        transaction
            .execute(
                "UPDATE identities SET is_primary = (id = ?2) WHERE account_id = ?1",
                params![account_id, identity_id],
            )
            .context("failed to change the primary identity")?;
        transaction
            .commit()
            .context("failed to commit changing the primary identity")
    }

    /// Why an identity can't be removed from Zmail, or `None` when it can:
    /// the primary identity, the last one and one a draft is written from
    /// are kept.
    pub fn identity_removal_blocker(&self, identity_id: i64) -> Result<Option<String>> {
        let conn = self.connection()?;
        let Some((account_id, row)) = load_row(&conn, identity_id)? else {
            bail!("that identity no longer exists");
        };
        removal_blocker(&conn, account_id, &row)
    }

    /// Removes an identity from Zmail (not from the server). Its mail is
    /// filed under the account's primary identity. The primary identity,
    /// the last one and one a draft is written from are kept
    /// ([`Self::identity_removal_blocker`]). A linked identity the server
    /// still lists is hidden, so later syncs leave it out
    /// ([`HiddenIdentity`]).
    pub fn remove_identity(&self, identity_id: i64) -> Result<()> {
        self.remove_identity_hiding(identity_id, true)
    }

    /// Removes an identity whose server identity was just deleted on the
    /// server: like [`Self::remove_identity`], but nothing is left to hide,
    /// and a marker for it goes too.
    pub fn remove_destroyed_identity(&self, identity_id: i64) -> Result<()> {
        self.remove_identity_hiding(identity_id, false)
    }

    fn remove_identity_hiding(&self, identity_id: i64, hide: bool) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin removing the identity")?;
        let Some((account_id, row)) = load_row(&transaction, identity_id)? else {
            bail!("that identity no longer exists");
        };
        if let Some(blocker) = removal_blocker(&transaction, account_id, &row)? {
            bail!(blocker);
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
        if let Some(server_id) = &row.server_id {
            transaction
                .execute(
                    "DELETE FROM hidden_identities WHERE account_id = ?1 AND server_id = ?2",
                    params![account_id, server_id],
                )
                .context("failed to update the hidden identities")?;
            if hide && row.status() != IdentitySyncStatus::NotOnServer {
                let seen = row.seen.clone().unwrap_or_default();
                transaction
                    .execute(
                        "INSERT INTO hidden_identities(account_id, server_id, email, name, \
                         reply_to, signature, may_delete) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            account_id,
                            server_id,
                            row.email,
                            seen.name,
                            seen.reply_to,
                            seen.signature,
                            row.may_delete,
                        ],
                    )
                    .context("failed to hide the identity from later syncs")?;
            }
        }
        transaction
            .commit()
            .context("failed to commit removing the identity")
    }

    /// The account's hidden server identities, by address.
    pub fn list_hidden_identities(&self, account_id: i64) -> Result<Vec<HiddenIdentity>> {
        let conn = self.connection()?;
        load_hidden(&conn, account_id)
    }

    /// Bring back: un-hides a server identity and puts it back at once,
    /// linked, with the server's values as last seen (the next sync
    /// confirms or updates them). Returns the identity's id. When the
    /// account has an identity for the address again, only the marker
    /// goes.
    pub fn bring_back_identity(&self, hidden_id: i64) -> Result<Option<i64>> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin bringing the identity back")?;
        let marker = transaction
            .query_row(
                &format!("SELECT {HIDDEN_COLUMNS} FROM hidden_identities WHERE id = ?1"),
                params![hidden_id],
                read_hidden,
            )
            .optional()
            .context("failed to load the hidden identity")?
            .context("that identity is no longer hidden")?;
        transaction
            .execute(
                "DELETE FROM hidden_identities WHERE id = ?1",
                params![hidden_id],
            )
            .context("failed to un-hide the identity")?;
        if ensure_address_is_new(&transaction, marker.account_id, &marker.email, None).is_err() {
            transaction
                .commit()
                .context("failed to commit bringing the identity back")?;
            return Ok(None);
        }
        let has_primary = primary_identity_id(&transaction, marker.account_id)?.is_some();
        let values = &marker.values;
        transaction
            .execute(
                "INSERT INTO identities(account_id, display_name, email_address, signature, \
                 is_primary, reply_to, server_id, server_name, server_reply_to, \
                 server_signature, server_state, server_may_delete) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?2, ?6, ?4, '', ?8)",
                params![
                    marker.account_id,
                    values.name,
                    marker.email,
                    values.signature,
                    !has_primary,
                    values.reply_to,
                    marker.server_id,
                    marker.may_delete,
                ],
            )
            .context("failed to bring the identity back")?;
        let identity_id = transaction.last_insert_rowid();
        transaction
            .commit()
            .context("failed to commit bringing the identity back")?;
        Ok(Some(identity_id))
    }
}

/// Why `row`, an identity of `account_id`, can't be removed, if it can't.
fn removal_blocker(
    conn: &rusqlite::Connection,
    account_id: i64,
    row: &Row,
) -> Result<Option<String>> {
    if row.is_primary {
        return Ok(Some(format!(
            "{} is the account's primary identity, so it stays",
            row.email
        )));
    }
    let others: i64 = conn.query_row(
        "SELECT COUNT(*) FROM identities WHERE account_id = ?1 AND id <> ?2",
        params![account_id, row.id],
        |row| row.get(0),
    )?;
    if others == 0 {
        return Ok(Some(format!(
            "{} is the account's only identity",
            row.email
        )));
    }
    let drafts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM drafts WHERE identity_id = ?1",
        params![row.id],
        |row| row.get(0),
    )?;
    if drafts > 0 {
        return Ok(Some(format!(
            "a draft is written from {}: send or delete it first",
            row.email
        )));
    }
    Ok(None)
}

/// Fails when `account_id` already has an identity for `email` (ignoring
/// case), other than `except`.
fn ensure_address_is_new(
    conn: &rusqlite::Connection,
    account_id: i64,
    email: &str,
    except: Option<i64>,
) -> Result<()> {
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM identities WHERE account_id = ?1 \
         AND email_address = ?2 COLLATE NOCASE AND id IS NOT ?3)",
        params![account_id, email, except],
        |row| row.get(0),
    )?;
    if taken {
        bail!("the account already has an identity for {email}");
    }
    Ok(())
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
        assert!(
            store
                .list_hidden_identities(account)
                .expect("hidden")
                .is_empty(),
            "one the server no longer lists has nothing to hide"
        );
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
            .record_identity_on_server(primary, "i1", &link.local, false)
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
    fn the_editor_adds_readdresses_makes_primary_and_guards_removal() {
        use zmail_core::{DraftThreading, DraftUpdate, NewDraft};

        let store = LocalStore::open_ephemeral().expect("the store opens");
        let account = jmap_account(&store);
        let primary = store.list_identities(account).expect("identities")[0].id;
        assert_eq!(
            store.identity_removal_blocker(primary).expect("checked"),
            Some("rob@example.com is the account's primary identity, so it stays".to_owned())
        );

        // Added: not primary, one per address.
        let help = store
            .add_identity(account, "help@example.com", &values("Help", "-- Help"))
            .expect("added");
        assert!(
            store
                .add_identity(account, "HELP@example.com", &values("Help", ""))
                .is_err()
        );
        let listed = store.list_synced_identities(account).expect("identities");
        assert_eq!(listed[1].identity.id, help);
        assert!(!listed[1].identity.is_primary);
        assert_eq!(listed[1].status, IdentitySyncStatus::Local);
        assert!(!listed[1].may_delete_on_server);

        // Its address can change, but not to one the account has, and a
        // linked identity's never.
        store
            .set_identity_address(help, "desk@example.com")
            .expect("readdressed");
        assert!(store.set_identity_address(help, "rob@example.com").is_err());
        store
            .merge_server_identities(
                account,
                &[server("i1", "rob@example.com", values("Rob", ""))],
            )
            .expect("merged");
        let error = store
            .set_identity_address(primary, "rob2@example.com")
            .expect_err("linked");
        assert!(error.to_string().contains("set by the server"), "{error}");
        let link = store
            .identity_link(primary)
            .expect("loads")
            .expect("exists");
        assert!(link.may_delete, "the server's mayDelete is kept");
        assert!(store.list_synced_identities(account).expect("identities")[0].may_delete_on_server);

        // Make primary: the other one is no longer.
        store.set_primary_identity(help).expect("made primary");
        let listed = store.list_identities(account).expect("identities");
        assert_eq!(listed[0].id, help);
        assert!(listed[0].is_primary && !listed[1].is_primary);
        assert_eq!(
            store.identity_removal_blocker(primary).expect("checked"),
            None
        );

        // A draft written from it keeps it, and keeps the values it started
        // with when the identity is edited meanwhile.
        let draft = store
            .create_draft(&NewDraft {
                account_id: account,
                identity_id: primary,
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Hello".to_owned(),
                body: String::new(),
                threading: DraftThreading::default(),
            })
            .expect("the draft starts");
        assert!(
            store
                .identity_removal_blocker(primary)
                .expect("checked")
                .is_some_and(|blocker| blocker.contains("a draft is written from"))
        );
        assert!(store.remove_identity(primary).is_err());
        store
            .update_identity(primary, &values("Rob, edited", "-- edited"))
            .expect("edited");
        let update = |identity_id| DraftUpdate {
            identity_id,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Hello".to_owned(),
            body: "Hi".to_owned(),
        };
        store.update_draft(draft, &update(primary)).expect("saved");
        let detail = store.draft_detail(draft).expect("loads").expect("exists");
        assert_eq!(detail.identity_name, "Rob");
        // Choosing another identity takes that one's values.
        store.update_draft(draft, &update(help)).expect("saved");
        let detail = store.draft_detail(draft).expect("loads").expect("exists");
        assert_eq!(detail.identity_email, "desk@example.com");
        assert_eq!(detail.identity_signature, "-- Help");

        // Now the old primary can go; its mail is filed under the new one.
        store.remove_identity(primary).expect("removed");
        assert_eq!(
            store.identity_removal_blocker(help).expect("checked"),
            Some("desk@example.com is the account's primary identity, so it stays".to_owned())
        );
    }

    #[test]
    fn an_identity_removed_from_zmail_only_stays_out_of_syncs_until_brought_back() {
        let store = LocalStore::open_ephemeral().expect("the store opens");
        let account = jmap_account(&store);
        let mut listed = vec![
            server("i1", "rob@example.com", values("Rob", "")),
            server("i2", "help@example.com", values("Help desk", "-- Help")),
        ];
        store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        let help = store.list_identities(account).expect("identities")[1].id;

        // Removed from Zmail only: hidden, and the next sync leaves it out.
        store.remove_identity(help).expect("removed");
        let hidden = store.list_hidden_identities(account).expect("hidden");
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].server_id, "i2");
        assert_eq!(hidden[0].values, values("Help desk", "-- Help"));
        assert!(hidden[0].may_delete);
        let report = store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert!(report.imported.is_empty(), "{report:?}");
        assert_eq!(report.hidden, ["help@example.com"]);
        assert_eq!(store.list_identities(account).expect("identities").len(), 1);

        // Re-created on the server under a new id: still the hidden
        // address, and the marker follows it with the server's values.
        listed[1] = server("i3", "help@example.com", values("Help", "-- new"));
        let report = store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert!(report.imported.is_empty());
        let hidden = store.list_hidden_identities(account).expect("hidden");
        assert_eq!(hidden[0].server_id, "i3");
        assert_eq!(hidden[0].values.name, "Help");

        // Bring back: at once, linked and in step with the server.
        let back = store
            .bring_back_identity(hidden[0].id)
            .expect("brought back")
            .expect("added");
        assert!(
            store
                .list_hidden_identities(account)
                .expect("hidden")
                .is_empty()
        );
        assert_eq!(
            status_of(&store, account, "help@example.com"),
            IdentitySyncStatus::InSync
        );
        let report = store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        assert_eq!(report, Default::default(), "the next sync agrees");
        assert_eq!(
            store
                .identity_link(back)
                .expect("loads")
                .expect("exists")
                .server_id
                .as_deref(),
            Some("i3")
        );

        // Hidden, then deleted on the server: the marker goes.
        store.remove_identity(back).expect("removed");
        store
            .merge_server_identities(account, &listed[..1])
            .expect("the merge runs");
        assert!(
            store
                .list_hidden_identities(account)
                .expect("hidden")
                .is_empty()
        );

        // Hidden, then added again in Zmail: no longer hidden, and the
        // next sync links it.
        store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        let again = store.list_identities(account).expect("identities")[1].id;
        store.remove_identity(again).expect("removed");
        assert_eq!(
            store.list_hidden_identities(account).expect("hidden").len(),
            1
        );
        let added = store
            .add_identity(account, "help@example.com", &values("Mine", ""))
            .expect("added");
        assert!(
            store
                .list_hidden_identities(account)
                .expect("hidden")
                .is_empty()
        );
        store
            .merge_server_identities(account, &listed)
            .expect("the merge runs");
        let link = store.identity_link(added).expect("loads").expect("exists");
        assert_eq!(link.server_id.as_deref(), Some("i3"));

        // Removed after deleting it on the server: nothing to hide.
        store.remove_destroyed_identity(added).expect("removed");
        assert!(
            store
                .list_hidden_identities(account)
                .expect("hidden")
                .is_empty()
        );
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
