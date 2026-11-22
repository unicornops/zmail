//! Address history for recipient autocomplete.
//!
//! Compose suggests addresses the user has corresponded with, ranked by
//! frecency: how often and how recently. The history is built only from
//! local state and never leaves the device.
//!
//! Each `address_history` row is one address of one account, with two
//! groups of counts:
//!
//! - **Mirror counts** follow the mirrored mail. A message whose sender is
//!   one of the account's identities counts as *sent* to each of its
//!   recipients; any other message counts as *received* from its sender.
//!   Junk is skipped, so spam senders are never suggested. The counts are
//!   always what [`rebuild_account_address_history`] would work out from
//!   the mirror as it stands, which is how a moved or re-synced message,
//!   back under a new remote id, never counts twice: nothing is counted
//!   for a message, only for the rows the mirror holds.
//!
//!   A sync that removes or changes mail, and every full snapshot, runs
//!   that rebuild, which reads all of the account's mail. A partial write
//!   that only adds message rows (a stretch of a Proton account's older
//!   mail, or new mail from its events) adds just those rows' addresses
//!   instead ([`MirrorChanges`], [`apply_mirror_changes`]), so its cost
//!   does not grow with the mirror (#478). Counts add up, and the last use
//!   and the display name are each the greatest of what was seen, so
//!   adding rows gives exactly what the rebuild would. Whatever else can
//!   change the result marks the account's history stale, and the next
//!   sync that changes mail rebuilds it: deleting or moving a conversation
//!   here, and a change to the account's own addresses (triggers, see
//!   [`ADDRESS_HISTORY_INCREMENTAL_SCHEMA`]).
//! - **Outbox counts** go up by one for every recipient of each message
//!   delivered from this device ([`record_outbox_recipients`]), so a
//!   recipient is suggested at once, before the sent copy syncs back.
//!
//! [`LocalStore::address_suggestions`] ranks matching addresses across
//! every account with Firefox-style frecency: sent mail weighs more than
//! received mail, and the weight decays in steps with the age of the last
//! use. The account's own addresses are never suggested.

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction, params};
use zmail_core::{
    AddressSuggestion, MessageSnapshot, participants::parse_address_list, time::parse_timestamp,
};

use crate::LocalStore;

/// Creates the history and asks [`LocalStore::open`] to build it from the
/// mail already mirrored.
pub(crate) const ADDRESS_HISTORY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS address_history (
    account_id INTEGER NOT NULL,
    address TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    sent_count INTEGER NOT NULL DEFAULT 0,
    received_count INTEGER NOT NULL DEFAULT 0,
    mirror_last_used_at TEXT,
    outbox_sent_count INTEGER NOT NULL DEFAULT 0,
    outbox_last_used_at TEXT,
    PRIMARY KEY (account_id, address),
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_address_history_address ON address_history(address);

INSERT OR REPLACE INTO app_settings(key, value) VALUES ('address_history_rebuild', 'pending');
"#;

/// `app_settings` key set while the history still has to be built from
/// the existing mirror.
const REBUILD_PENDING_KEY: &str = "address_history_rebuild";

/// Lets partial writes add to the history instead of rebuilding it (#478).
///
/// `mirror_named_at` is when the mail that gave `display_name` was
/// received, so a write that adds older mail knows not to take its names.
/// An `address_history_stale:<account id>` row in `app_settings` marks a
/// history that no longer matches the mirror, for the next sync that
/// changes mail to rebuild: the triggers set it when a conversation is
/// deleted or moved to another mailbox (junk does not count) and when the
/// account's own addresses change (they decide what counts as sent).
/// Every existing account starts stale, so its first such sync fills in
/// `mirror_named_at`. The mark goes with the account.
pub(crate) const ADDRESS_HISTORY_INCREMENTAL_SCHEMA: &str = r#"
ALTER TABLE address_history ADD COLUMN mirror_named_at TEXT;

INSERT OR REPLACE INTO app_settings(key, value)
SELECT 'address_history_stale:' || id, '1' FROM accounts;

CREATE TRIGGER IF NOT EXISTS address_history_stale_thread_delete AFTER DELETE ON threads
BEGIN
    INSERT OR REPLACE INTO app_settings(key, value)
    SELECT 'address_history_stale:' || account_id, '1' FROM mailboxes WHERE id = old.mailbox_id;
END;
CREATE TRIGGER IF NOT EXISTS address_history_stale_thread_move
AFTER UPDATE OF mailbox_id ON threads
BEGIN
    INSERT OR REPLACE INTO app_settings(key, value)
    SELECT 'address_history_stale:' || account_id, '1' FROM mailboxes WHERE id = new.mailbox_id;
END;
CREATE TRIGGER IF NOT EXISTS address_history_stale_identity_insert AFTER INSERT ON identities
BEGIN
    INSERT OR REPLACE INTO app_settings(key, value)
    VALUES ('address_history_stale:' || new.account_id, '1');
END;
CREATE TRIGGER IF NOT EXISTS address_history_stale_identity_update
AFTER UPDATE OF email_address ON identities
BEGIN
    INSERT OR REPLACE INTO app_settings(key, value)
    VALUES ('address_history_stale:' || new.account_id, '1');
END;
CREATE TRIGGER IF NOT EXISTS address_history_stale_identity_delete AFTER DELETE ON identities
BEGIN
    INSERT OR REPLACE INTO app_settings(key, value)
    SELECT 'address_history_stale:' || id, '1' FROM accounts WHERE id = old.account_id;
END;
CREATE TRIGGER IF NOT EXISTS address_history_stale_account_delete AFTER DELETE ON accounts
BEGIN
    DELETE FROM app_settings WHERE key = 'address_history_stale:' || old.id;
END;
"#;

/// The `app_settings` key that marks an account's history stale.
fn stale_key(account_id: i64) -> String {
    format!("address_history_stale:{account_id}")
}

/// Whether the account's history may no longer match its mirror.
fn is_stale(conn: &Connection, account_id: i64) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM app_settings WHERE key = ?1)")?
        .query_row(params![stale_key(account_id)], |row| row.get(0))?)
}

/// Weight of one message sent to an address.
const SENT_WEIGHT: f64 = 4.0;
/// Weight of one message received from an address.
const RECEIVED_WEIGHT: f64 = 1.0;

/// How much a use counts, by the age of the last use (Firefox's frecency
/// buckets).
fn recency_weight(age_days: i64) -> f64 {
    match age_days {
        ..=4 => 100.0,
        5..=14 => 70.0,
        15..=31 => 50.0,
        32..=90 => 30.0,
        _ => 10.0,
    }
}

/// What one address has accumulated, while rebuilding or across accounts.
#[derive(Default)]
struct Tally {
    display_name: String,
    /// The timestamp `display_name` came from, so the newest name wins.
    named_at: Option<DateTime<Utc>>,
    sent: i64,
    received: i64,
    last_used_at: Option<DateTime<Utc>>,
}

impl Tally {
    fn add(&mut self, name: Option<&str>, at: Option<DateTime<Utc>>, sent: i64, received: i64) {
        self.sent += sent;
        self.received += received;
        self.last_used_at = self.last_used_at.max(at);
        // The newest name wins, and of two equally new the greater one, so
        // the result never depends on the order the uses come in.
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty())
            && (self.display_name.is_empty()
                || (at, name) > (self.named_at, self.display_name.as_str()))
        {
            self.display_name = name.to_owned();
            self.named_at = at;
        }
    }
}

/// The lowercased addresses of an account's identities.
fn own_addresses(conn: &Connection, account_id: Option<i64>) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT lower(trim(email_address)) FROM identities WHERE ?1 IS NULL OR account_id = ?1",
    )?;
    let rows = statement.query_map(params![account_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load the identity addresses")
}

fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    parse_timestamp(value).map(|at| at.with_timezone(&Utc))
}

fn format_utc(at: Option<DateTime<Utc>>) -> Option<String> {
    at.map(|at| at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Rebuilds an account's mirror counts from its mirrored mail.
///
/// Messages are grouped by their sender and recipient strings in SQL, so
/// only the distinct address headers are parsed, not every message.
pub(crate) fn rebuild_account_address_history(
    transaction: &Transaction<'_>,
    account_id: i64,
) -> Result<()> {
    let own = own_addresses(transaction, Some(account_id))?;
    let mut tallies: HashMap<String, Tally> = HashMap::new();
    {
        let mut statement = transaction.prepare(
            r#"
            SELECT messages.sender, messages.recipients, COUNT(*), MAX(messages.received_at)
            FROM messages
            JOIN threads ON threads.id = messages.thread_id
            JOIN mailboxes ON mailboxes.id = threads.mailbox_id
            WHERE mailboxes.account_id = ?1
              AND lower(COALESCE(mailboxes.role, '')) NOT IN ('junk', 'spam')
            GROUP BY messages.sender, messages.recipients
            "#,
        )?;
        let mut rows = statement.query(params![account_id])?;
        while let Some(row) = rows.next()? {
            let sender: String = row.get(0)?;
            let recipients: String = row.get(1)?;
            let count: i64 = row.get(2)?;
            let at = row
                .get::<_, Option<String>>(3)?
                .as_deref()
                .and_then(parse_utc);
            tally_messages(&mut tallies, &own, &sender, &recipients, at, count);
        }
    }

    transaction.execute(
        r#"
        UPDATE address_history
        SET sent_count = 0, received_count = 0, mirror_last_used_at = NULL, mirror_named_at = NULL
        WHERE account_id = ?1
        "#,
        params![account_id],
    )?;
    {
        let mut upsert = transaction.prepare(
            r#"
            INSERT INTO address_history(
                account_id, address, display_name, sent_count, received_count,
                mirror_last_used_at, mirror_named_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(account_id, address) DO UPDATE SET
                display_name = CASE WHEN excluded.display_name = ''
                    THEN address_history.display_name ELSE excluded.display_name END,
                sent_count = excluded.sent_count,
                received_count = excluded.received_count,
                mirror_last_used_at = excluded.mirror_last_used_at,
                mirror_named_at = excluded.mirror_named_at
            "#,
        )?;
        for (address, tally) in &tallies {
            upsert.execute(params![
                account_id,
                address,
                tally.display_name,
                tally.sent,
                tally.received,
                format_utc(tally.last_used_at),
                format_utc(tally.named_at),
            ])?;
        }
    }
    transaction.execute(
        r#"
        DELETE FROM address_history
        WHERE account_id = ?1 AND sent_count = 0 AND received_count = 0 AND outbox_sent_count = 0
        "#,
        params![account_id],
    )?;
    transaction.execute(
        "DELETE FROM app_settings WHERE key = ?1",
        params![stale_key(account_id)],
    )?;
    #[cfg(test)]
    REBUILDS.with(|rebuilds| rebuilds.set(rebuilds.get() + 1));
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Rebuilds run on this thread, for tests that a write did or did not
    /// need one.
    pub(crate) static REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Makes every partial write that changes mail rebuild the history and
    /// count its mailboxes again, as all of them did before #478, for
    /// tests that compare the two.
    pub(crate) static ALWAYS_REBUILD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One message row the history counts: its address headers and when it
/// was received.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AddressUse {
    pub(crate) sender: String,
    pub(crate) recipients: String,
    pub(crate) received_at: String,
}

impl AddressUse {
    fn of(message: &MessageSnapshot) -> Self {
        Self {
            sender: message.sender.clone(),
            recipients: message.recipients.clone(),
            received_at: message.received_at.clone(),
        }
    }
}

/// What a partial write did to the mail the history counts: the message
/// rows it added, or that only a rebuild can follow it.
#[derive(Debug, Default)]
pub(crate) struct MirrorChanges {
    added: Vec<AddressUse>,
    rebuild: bool,
}

impl MirrorChanges {
    /// Mail left the mirror or changed in a way adding rows cannot
    /// express.
    pub(crate) fn needs_rebuild(&mut self) {
        self.rebuild = true;
    }

    /// The messages of a conversation new to a mailbox. Junk is not
    /// counted.
    pub(crate) fn added<'a>(
        &mut self,
        junk: bool,
        messages: impl IntoIterator<Item = &'a MessageSnapshot>,
    ) {
        if junk || self.rebuild {
            return;
        }
        for message in messages {
            self.add(AddressUse::of(message));
        }
    }

    /// A stored conversation rewritten: `stored` are the rows it had and
    /// `messages` the rows it has now. Rows it kept change nothing; a row
    /// it lost, or one whose addresses or time changed, needs a rebuild.
    pub(crate) fn rewritten<'a>(
        &mut self,
        junk: bool,
        stored: Vec<AddressUse>,
        messages: impl IntoIterator<Item = &'a MessageSnapshot>,
    ) {
        if junk || self.rebuild {
            return;
        }
        let mut kept: HashMap<AddressUse, usize> = HashMap::new();
        for row in stored {
            *kept.entry(row).or_default() += 1;
        }
        for message in messages {
            let row = AddressUse::of(message);
            match kept.get_mut(&row) {
                Some(count) if *count > 0 => *count -= 1,
                _ => self.add(row),
            }
        }
        if kept.values().any(|count| *count > 0) {
            self.rebuild = true;
        }
    }

    fn add(&mut self, row: AddressUse) {
        // Adding a row gives what the rebuild gives when times compare the
        // same as text and as instants: the rebuild takes the greatest
        // `received_at` of equal headers as text. That holds for whole
        // seconds in UTC, which is how the adapters that write partially
        // (Proton) format them; anything else is left to the rebuild.
        if is_utc_seconds(&row.received_at) {
            self.added.push(row);
        } else {
            self.rebuild = true;
        }
    }
}

/// Whether `value` is `YYYY-MM-DDTHH:MM:SS+00:00`.
fn is_utc_seconds(value: &str) -> bool {
    value.len() == 25 && value.ends_with("+00:00") && parse_timestamp(value).is_some()
}

/// The address headers of a mailbox's stored conversation, for
/// [`MirrorChanges::rewritten`].
pub(crate) fn stored_address_uses(conn: &Connection, thread_id: i64) -> Result<Vec<AddressUse>> {
    conn.prepare_cached(
        "SELECT sender, recipients, received_at FROM messages WHERE thread_id = ?1",
    )?
    .query_map(params![thread_id], |row| {
        Ok(AddressUse {
            sender: row.get(0)?,
            recipients: row.get(1)?,
            received_at: row.get(2)?,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()
    .context("failed to load a conversation's address headers")
}

/// Brings an account's mirror counts up to date after a partial write
/// that changed mail: adds the rows it added, or rebuilds when it did
/// more than add, or when the history was already stale.
pub(crate) fn apply_mirror_changes(
    transaction: &Transaction<'_>,
    account_id: i64,
    changes: MirrorChanges,
) -> Result<()> {
    #[cfg(test)]
    let changes = MirrorChanges {
        rebuild: changes.rebuild || ALWAYS_REBUILD.with(std::cell::Cell::get),
        ..changes
    };
    if changes.rebuild || is_stale(transaction, account_id)? {
        return rebuild_account_address_history(transaction, account_id);
    }
    if changes.added.is_empty() {
        return Ok(());
    }

    let own = own_addresses(transaction, Some(account_id))?;
    let mut tallies: HashMap<String, Tally> = HashMap::new();
    for row in &changes.added {
        let at = parse_utc(&row.received_at);
        tally_messages(&mut tallies, &own, &row.sender, &row.recipients, at, 1);
    }
    // The same row the rebuild would write for the mail before and these
    // rows together: counts add up, and the last use and the name (with
    // its time) are the greater of the two. Times are stored as UTC
    // seconds, so they compare as text.
    let mut upsert = transaction.prepare_cached(
        r#"
        INSERT INTO address_history(
            account_id, address, display_name, sent_count, received_count,
            mirror_last_used_at, mirror_named_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(account_id, address) DO UPDATE SET
            display_name = CASE
                WHEN excluded.display_name = '' THEN address_history.display_name
                WHEN address_history.mirror_named_at IS NULL THEN excluded.display_name
                WHEN excluded.mirror_named_at IS NULL THEN address_history.display_name
                WHEN (excluded.mirror_named_at, excluded.display_name)
                    > (address_history.mirror_named_at, address_history.display_name)
                    THEN excluded.display_name
                ELSE address_history.display_name END,
            mirror_named_at = CASE
                WHEN excluded.display_name = '' THEN address_history.mirror_named_at
                ELSE MAX(COALESCE(address_history.mirror_named_at, excluded.mirror_named_at),
                         COALESCE(excluded.mirror_named_at, address_history.mirror_named_at)) END,
            sent_count = address_history.sent_count + excluded.sent_count,
            received_count = address_history.received_count + excluded.received_count,
            mirror_last_used_at = MAX(
                COALESCE(address_history.mirror_last_used_at, excluded.mirror_last_used_at),
                COALESCE(excluded.mirror_last_used_at, address_history.mirror_last_used_at))
        "#,
    )?;
    for (address, tally) in &tallies {
        upsert.execute(params![
            account_id,
            address,
            tally.display_name,
            tally.sent,
            tally.received,
            format_utc(tally.last_used_at),
            format_utc(tally.named_at),
        ])?;
    }
    Ok(())
}

/// Adds `count` messages with these address headers, the newest received
/// at `at`, to `tallies`: sent to each recipient when the sender is one of
/// the account's own addresses, otherwise received from the sender.
fn tally_messages(
    tallies: &mut HashMap<String, Tally>,
    own: &[String],
    sender: &str,
    recipients: &str,
    at: Option<DateTime<Utc>>,
    count: i64,
) {
    let senders = parse_address_list(sender);
    let is_outgoing = senders
        .iter()
        .any(|participant| own.contains(&participant.key()));
    if is_outgoing {
        for recipient in parse_address_list(recipients) {
            tally(tallies, own, &recipient, at, count, 0);
        }
    } else {
        for sender in senders {
            tally(tallies, own, &sender, at, 0, count);
        }
    }
}

/// Adds one use of `participant` to `tallies`, unless it has no address or
/// is one of the account's own.
fn tally(
    tallies: &mut HashMap<String, Tally>,
    own: &[String],
    participant: &zmail_core::participants::Participant,
    at: Option<DateTime<Utc>>,
    sent: i64,
    received: i64,
) {
    let Some(email) = participant.email.as_deref().map(str::trim) else {
        return;
    };
    let address = email.to_ascii_lowercase();
    if !address.contains('@') || own.contains(&address) {
        return;
    }
    tallies
        .entry(address)
        .or_default()
        .add(participant.name.as_deref(), at, sent, received);
}

/// Counts one message delivered from this device to each of `recipients`
/// (address list entries such as `Alice <alice@example.com>`).
pub(crate) fn record_outbox_recipients(
    transaction: &Transaction<'_>,
    account_id: i64,
    recipients: &[String],
    now: &str,
) -> Result<()> {
    let own = own_addresses(transaction, Some(account_id))?;
    let mut seen = Vec::new();
    let mut upsert = transaction.prepare(
        r#"
        INSERT INTO address_history(
            account_id, address, display_name, outbox_sent_count, outbox_last_used_at
        )
        VALUES (?1, ?2, ?3, 1, ?4)
        ON CONFLICT(account_id, address) DO UPDATE SET
            display_name = CASE WHEN excluded.display_name = ''
                THEN address_history.display_name ELSE excluded.display_name END,
            outbox_sent_count = address_history.outbox_sent_count + 1,
            outbox_last_used_at = excluded.outbox_last_used_at
        "#,
    )?;
    for participant in recipients
        .iter()
        .flat_map(|recipient| parse_address_list(recipient))
    {
        let Some(email) = participant.email.as_deref().map(str::trim) else {
            continue;
        };
        let address = email.to_ascii_lowercase();
        if !address.contains('@') || own.contains(&address) || seen.contains(&address) {
            continue;
        }
        upsert.execute(params![
            account_id,
            address,
            participant
                .name
                .as_deref()
                .map(str::trim)
                .unwrap_or_default(),
            now,
        ])?;
        seen.push(address);
    }
    Ok(())
}

/// Escapes `%`, `_` and the escape character itself for a `LIKE … ESCAPE
/// '\'` pattern.
fn escape_like(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

impl LocalStore {
    /// Builds the history from the mirrored mail once, after the migration
    /// that adds it.
    pub(crate) fn rebuild_address_history_if_pending(&self) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin the address history transaction")?;
        let pending = transaction
            .query_row(
                "SELECT 1 FROM app_settings WHERE key = ?1",
                params![REBUILD_PENDING_KEY],
                |_| Ok(()),
            )
            .is_ok();
        if pending {
            let account_ids = {
                let mut statement = transaction.prepare("SELECT id FROM accounts")?;
                let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for account_id in account_ids {
                rebuild_account_address_history(&transaction, account_id)?;
            }
            transaction.execute(
                "DELETE FROM app_settings WHERE key = ?1",
                params![REBUILD_PENDING_KEY],
            )?;
        }
        transaction
            .commit()
            .context("failed to commit the address history build")
    }

    /// Up to `limit` addresses for recipient autocomplete, best first.
    ///
    /// `query` matches the start of the address, of its domain, or of any
    /// word of the display name, ignoring case. An empty query lists the
    /// most used addresses. Every account's history is searched; the
    /// user's own addresses are left out.
    pub fn address_suggestions(&self, query: &str, limit: usize) -> Result<Vec<AddressSuggestion>> {
        self.address_suggestions_at(query, limit, Utc::now())
    }

    /// [`Self::address_suggestions`] ranked as of `now`.
    pub(crate) fn address_suggestions_at(
        &self,
        query: &str,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<AddressSuggestion>> {
        let conn = self.connection()?;
        let query = query.trim().to_lowercase();
        let prefix = format!("{}%", escape_like(&query));
        let word = format!("% {}%", escape_like(&query));
        let domain = format!("%@{}%", escape_like(&query));
        let mut statement = conn.prepare(
            r#"
            SELECT address, display_name, sent_count + outbox_sent_count, received_count,
                   mirror_last_used_at, outbox_last_used_at
            FROM address_history
            WHERE address LIKE ?1 ESCAPE '\'
               OR address LIKE ?3 ESCAPE '\'
               OR lower(display_name) LIKE ?1 ESCAPE '\'
               OR lower(display_name) LIKE ?2 ESCAPE '\'
            "#,
        )?;
        let rows = statement.query_map(params![prefix, word, domain], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;

        // One entry per address across accounts.
        let mut merged: HashMap<String, Tally> = HashMap::new();
        for row in rows {
            let (address, name, sent, received, mirror_at, outbox_at) =
                row.context("failed to read the address history")?;
            let last = mirror_at
                .as_deref()
                .and_then(parse_utc)
                .max(outbox_at.as_deref().and_then(parse_utc));
            merged
                .entry(address)
                .or_default()
                .add(Some(&name), last, sent, received);
        }

        let own = own_addresses(&conn, None)?;
        let mut ranked: Vec<(f64, AddressSuggestion)> = merged
            .into_iter()
            .filter(|(address, _)| !own.contains(address))
            .map(|(address, tally)| {
                let age_days = tally
                    .last_used_at
                    .map_or(i64::MAX, |last| (now - last).num_days());
                let score = (SENT_WEIGHT * tally.sent as f64
                    + RECEIVED_WEIGHT * tally.received as f64)
                    * recency_weight(age_days);
                let name = tally.display_name;
                (
                    score,
                    AddressSuggestion {
                        name: (!name.is_empty()).then_some(name),
                        email: address,
                    },
                )
            })
            .collect();
        ranked.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .total_cmp(left_score)
                .then_with(|| left.email.cmp(&right.email))
        });
        Ok(ranked
            .into_iter()
            .take(limit)
            .map(|(_, suggestion)| suggestion)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use zmail_core::{
        AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, NewDraft, ProtocolKind,
        ThreadSnapshot,
    };

    use crate::LocalStore;

    fn message(remote_id: &str, sender: &str, recipients: &str, at: &str) -> MessageSnapshot {
        MessageSnapshot {
            remote_id: remote_id.to_owned(),
            identity_email: String::new(),
            sender: sender.to_owned(),
            reply_to: None,
            recipients: recipients.to_owned(),
            subject: "Hello".to_owned(),
            body_preview: String::new(),
            body_text: None,
            sanitized_html: None,
            raw_html: None,
            has_remote_content: false,
            is_read: true,
            is_flagged: false,
            received_at: at.to_owned(),
            attachments: Vec::new(),
            headers: Default::default(),
        }
    }

    fn thread(id: &str, messages: Vec<MessageSnapshot>) -> ThreadSnapshot {
        ThreadSnapshot {
            remote_thread_id: id.to_owned(),
            subject: "Hello".to_owned(),
            participants: String::new(),
            preview: String::new(),
            is_flagged: false,
            has_attachments: false,
            unread_count: 0,
            latest_message_at: messages
                .last()
                .map(|message| message.received_at.clone())
                .unwrap_or_default(),
            messages,
        }
    }

    fn mailbox(name: &str, role: &str, threads: Vec<ThreadSnapshot>) -> MailboxSnapshot {
        MailboxSnapshot {
            name: name.to_owned(),
            role: Some(role.to_owned()),
            threads,
            ..Default::default()
        }
    }

    fn snapshot(mailboxes: Vec<MailboxSnapshot>) -> AccountSyncSnapshot {
        AccountSyncSnapshot {
            checkpoint: "history:test".to_owned(),
            detail: "history test sync".to_owned(),
            identities: None,
            mailboxes,
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        value.parse().expect("a valid timestamp")
    }

    // Characterisation (issue #489): how the history reads `received_at`
    // and writes its own times, as it did before the workspace moved off
    // chrono.
    #[test]
    fn last_used_times_are_written_as_before() {
        let cases: &[(&str, Option<&str>)] = &[
            ("2026-10-15T10:42:00+00:00", Some("2026-10-15T10:42:00Z")),
            ("2026-10-15T10:42:00Z", Some("2026-10-15T10:42:00Z")),
            ("2026-10-15 10:42:00", Some("2026-10-15T10:42:00Z")),
            ("2026-10-15T16:12:00+05:30", Some("2026-10-15T10:42:00Z")),
            ("2026-10-15T02:42:00-08:00", Some("2026-10-15T10:42:00Z")),
            ("2026-10-15T00:30:00+01:00", Some("2026-10-14T23:30:00Z")),
            ("2027-01-01T00:30:00+01:00", Some("2026-12-31T23:30:00Z")),
            ("2026-10-15T10:42:00.999Z", Some("2026-10-15T10:42:00Z")),
            ("2026-12-31T23:59:60Z", Some("2026-12-31T23:59:60Z")),
            ("2026-10-15T10:42:60.5+05:30", Some("2026-10-15T05:12:60Z")),
            ("1969-12-31T23:59:59Z", Some("1969-12-31T23:59:59Z")),
            ("0000-01-01T00:00:00Z", Some("0000-01-01T00:00:00Z")),
            ("0000-01-01T00:00:00+01:00", Some("-0001-12-31T23:00:00Z")),
            ("9999-12-31T23:59:59Z", Some("9999-12-31T23:59:59Z")),
            ("9999-12-31T23:59:59-01:00", Some("+10000-01-01T00:59:59Z")),
            ("2026-10-15T10:42:00", None),
            ("Thu, 15 Oct 2026 10:42:00 +0000", None),
            ("", None),
        ];
        for (value, expected) in cases {
            assert_eq!(
                super::format_utc(super::parse_utc(value)).as_deref(),
                *expected,
                "{value:?}"
            );
        }
        // Equal instants are equal however they were written, and a leap
        // second comes after the second before it.
        let order = [
            "2026-12-31T23:59:59Z",
            "2026-12-31T23:59:59.5Z",
            "2026-12-31T23:59:60Z",
            "2027-01-01T00:59:60.5+01:00",
            "2027-01-01T00:00:00Z",
        ];
        for pair in order.windows(2) {
            assert!(
                super::parse_utc(pair[0]) < super::parse_utc(pair[1]),
                "{pair:?}"
            );
        }
        assert_eq!(
            super::parse_utc("2026-10-15T10:42:00Z"),
            super::parse_utc("2026-10-15T12:42:00+02:00")
        );
    }

    #[test]
    fn whole_utc_seconds_are_recognised_as_before() {
        let cases = [
            ("2026-10-15T10:42:00+00:00", true),
            ("2026-10-15t10:42:00+00:00", true),
            ("2026-10-15 10:42:00+00:00", true),
            ("2026-12-31T23:59:60+00:00", true),
            ("0000-01-01T00:00:00+00:00", true),
            ("9999-12-31T23:59:59+00:00", true),
            ("2026-10-15T10:42:00Z", false),
            ("2026-10-15T10:42:00-00:00", false),
            ("2026-10-15T10:42:00+01:00", false),
            ("2026-10-15T10:42:00.000+00:00", false),
            ("2026-10-15T10:42:0.0+00:00", false),
            (" 2026-10-15T10:42:00+00:00", false),
            ("2026-10-15T10:42:61+00:00", false),
            ("2026-02-30T10:42:00+00:00", false),
            ("2026-10-15T24:00:00+00:00", false),
            ("2026-10-15T10:42:00 +00:00", false),
            ("2026-10-15 10:42:00 +00:00", false),
            ("not a timestamp at all+00:00", false),
            ("", false),
        ];
        for (value, expected) in cases {
            assert_eq!(super::is_utc_seconds(value), expected, "{value:?}");
        }
    }

    /// An account whose Inbox has three messages from Alice and one from
    /// Bob, whose Sent has one message to Carol and Alice, and whose Junk
    /// has a spammer. Returns the store, the account and its own address.
    fn store_with_mail() -> (LocalStore, i64, String) {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let me = store.list_identities(account_id).expect("identities load")[0]
            .email_address
            .clone();
        let alice = "Alice Johnson <Alice@Example.com>";
        store
            .replace_account_snapshot(
                account_id,
                &snapshot(vec![
                    mailbox(
                        "INBOX",
                        "inbox",
                        vec![
                            thread(
                                "t1",
                                vec![
                                    message("1", alice, &me, "2026-03-01T09:00:00Z"),
                                    message("2", alice, &me, "2026-03-02T09:00:00Z"),
                                ],
                            ),
                            thread("t2", vec![message("3", alice, &me, "2026-03-03T09:00:00Z")]),
                            thread(
                                "t3",
                                vec![message(
                                    "4",
                                    "bob@example.org",
                                    &format!("{me}, Dan <dan@example.net>"),
                                    "2026-03-04T09:00:00Z",
                                )],
                            ),
                        ],
                    ),
                    mailbox(
                        "Sent",
                        "sent",
                        vec![thread(
                            "t4",
                            vec![message(
                                "5",
                                &format!("Me <{me}>"),
                                "\"Lee, Carol\" <carol@example.com>, alice@example.com",
                                "2026-03-05T09:00:00Z",
                            )],
                        )],
                    ),
                    mailbox(
                        "Junk",
                        "junk",
                        vec![thread(
                            "t5",
                            vec![message(
                                "6",
                                "spam@example.biz",
                                &me,
                                "2026-03-05T09:00:00Z",
                            )],
                        )],
                    ),
                ]),
            )
            .expect("snapshot should apply");
        (store, account_id, me)
    }

    fn emails(store: &LocalStore, query: &str, now: &str) -> Vec<String> {
        store
            .address_suggestions_at(query, 10, at(now))
            .expect("suggestions load")
            .into_iter()
            .map(|suggestion| suggestion.email)
            .collect()
    }

    #[test]
    fn mirrored_mail_ranks_sent_and_frequent_addresses_first() {
        let (store, _, me) = store_with_mail();
        // Alice: 3 received + 1 sent = 7; Carol: 1 sent = 4; Bob: 1.
        // Dan was only copied on Bob's message; the spammer is in Junk and
        // my own address is never offered.
        assert_eq!(
            emails(&store, "", "2026-03-06T00:00:00Z"),
            ["alice@example.com", "carol@example.com", "bob@example.org"]
        );
        assert!(!emails(&store, "", "2026-03-06T00:00:00Z").contains(&me));

        let suggestions = store
            .address_suggestions_at("ali", 10, at("2026-03-06T00:00:00Z"))
            .expect("suggestions load");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name.as_deref(), Some("Alice Johnson"));
        assert_eq!(
            suggestions[0].to_header_value(),
            "Alice Johnson <alice@example.com>"
        );
    }

    #[test]
    fn queries_match_address_name_words_and_domain_only_at_the_start() {
        let (store, _, _) = store_with_mail();
        let now = "2026-03-06T00:00:00Z";
        assert_eq!(emails(&store, "CAR", now), ["carol@example.com"]);
        // The second word of a display name.
        assert_eq!(emails(&store, "johns", now), ["alice@example.com"]);
        assert_eq!(emails(&store, "lee", now), ["carol@example.com"]);
        assert_eq!(emails(&store, "example.org", now), ["bob@example.org"]);
        // Mid-word text does not match.
        assert!(emails(&store, "lice", now).is_empty());
        // LIKE wildcards are plain text.
        assert!(emails(&store, "%", now).is_empty());
        assert!(emails(&store, "_", now).is_empty());
    }

    #[test]
    fn recent_use_outranks_old_frequent_use() {
        let (store, _, _) = store_with_mail();
        // Long after: everyone decays to the same bucket, so frequency
        // decides.
        assert_eq!(
            emails(&store, "", "2027-01-01T00:00:00Z")[0],
            "alice@example.com"
        );
        // A send to Bob today beats Alice's older, more frequent mail.
        let account_id = store.list_accounts().expect("accounts load")[0].id;
        let mut conn = store.connection().expect("connection opens");
        let transaction = conn.transaction().expect("transaction begins");
        super::record_outbox_recipients(
            &transaction,
            account_id,
            &["bob@example.org".to_owned()],
            "2026-12-31T12:00:00Z",
        )
        .expect("the send is recorded");
        transaction.commit().expect("commit");
        assert_eq!(
            emails(&store, "", "2027-01-01T00:00:00Z")[0],
            "bob@example.org"
        );
    }

    #[test]
    fn resyncing_moved_mail_does_not_count_it_twice() {
        let (store, account_id, me) = store_with_mail();
        let before = emails(&store, "", "2026-03-06T00:00:00Z");
        // Bob's message moves to Archive and comes back with a new uid;
        // the Inbox loses it.
        store
            .replace_account_snapshot(
                account_id,
                &snapshot(vec![
                    mailbox(
                        "INBOX",
                        "inbox",
                        vec![
                            thread(
                                "t1",
                                vec![
                                    message(
                                        "1",
                                        "Alice Johnson <Alice@Example.com>",
                                        &me,
                                        "2026-03-01T09:00:00Z",
                                    ),
                                    message(
                                        "2",
                                        "Alice Johnson <Alice@Example.com>",
                                        &me,
                                        "2026-03-02T09:00:00Z",
                                    ),
                                ],
                            ),
                            thread(
                                "t2",
                                vec![message(
                                    "3",
                                    "Alice Johnson <Alice@Example.com>",
                                    &me,
                                    "2026-03-03T09:00:00Z",
                                )],
                            ),
                        ],
                    ),
                    mailbox(
                        "Archive",
                        "archive",
                        vec![thread(
                            "t3",
                            vec![message(
                                "99",
                                "bob@example.org",
                                &me,
                                "2026-03-04T09:00:00Z",
                            )],
                        )],
                    ),
                    mailbox(
                        "Sent",
                        "sent",
                        vec![thread(
                            "t4",
                            vec![message(
                                "5",
                                &format!("Me <{me}>"),
                                "\"Lee, Carol\" <carol@example.com>, alice@example.com",
                                "2026-03-05T09:00:00Z",
                            )],
                        )],
                    ),
                ]),
            )
            .expect("snapshot should apply");
        assert_eq!(emails(&store, "", "2026-03-06T00:00:00Z"), before);
        let bob = store
            .connection()
            .expect("connection opens")
            .query_row(
                "SELECT received_count FROM address_history WHERE address = 'bob@example.org'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("bob is in the history");
        assert_eq!(bob, 1);
    }

    #[test]
    fn sending_from_the_outbox_adds_the_recipients_at_once() {
        let (store, account_id, _) = store_with_mail();
        let identity_id = store.list_identities(account_id).expect("identities load")[0].id;
        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id,
                to: vec!["Dana Scully <dana@example.gov>".to_owned()],
                cc: vec!["carol@example.com".to_owned()],
                bcc: vec!["eve@example.net".to_owned()],
                subject: "Files".to_owned(),
                body: "Attached.".to_owned(),
                threading: Default::default(),
            })
            .expect("draft should be created");
        store.queue_draft(draft_id).expect("draft should queue");
        let job = store
            .claim_due_outbox_job(Some(account_id))
            .expect("claim works")
            .expect("the job is due");
        // Nothing is recorded until the message is actually delivered.
        assert!(emails(&store, "dana", "2026-03-06T00:00:00Z").is_empty());
        store
            .record_outbox_sent(job.job_id)
            .expect("the send is recorded");

        let dana = store
            .address_suggestions("dana", 10)
            .expect("suggestions load");
        assert_eq!(dana.len(), 1);
        assert_eq!(dana[0].to_header_value(), "Dana Scully <dana@example.gov>");
        assert_eq!(
            emails(&store, "eve", "2026-03-06T00:00:00Z"),
            ["eve@example.net"]
        );
        // Carol keeps the name from her mail.
        let carol = store
            .address_suggestions("carol", 10)
            .expect("suggestions load");
        assert_eq!(carol[0].name.as_deref(), Some("Lee, Carol"));
    }

    #[test]
    fn opening_a_store_builds_the_history_from_existing_mail_once() {
        let (store, account_id, _) = store_with_mail();
        // As if the mirror predated the history.
        let conn = store.connection().expect("connection opens");
        conn.execute("DELETE FROM address_history", [])
            .expect("history clears");
        conn.execute(
            "INSERT INTO app_settings(key, value) VALUES ('address_history_rebuild', 'pending')",
            [],
        )
        .expect("marker set");
        drop(conn);

        let reopened =
            LocalStore::open(store.path(), store.store_key().clone()).expect("store reopens");
        assert_eq!(
            emails(&reopened, "", "2026-03-06T00:00:00Z"),
            ["alice@example.com", "carol@example.com", "bob@example.org"]
        );
        let pending: i64 = reopened
            .connection()
            .expect("connection opens")
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key = 'address_history_rebuild'",
                [],
                |row| row.get(0),
            )
            .expect("count loads");
        assert_eq!(pending, 0);

        // Removing the account removes its history.
        reopened
            .connection()
            .expect("connection opens")
            .execute("DELETE FROM accounts WHERE id = ?1", [account_id])
            .expect("account deletes");
        assert!(emails(&reopened, "", "2026-03-06T00:00:00Z").is_empty());
    }
}
