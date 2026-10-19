//! Address history for recipient autocomplete.
//!
//! Compose suggests addresses the user has corresponded with, ranked by
//! frecency: how often and how recently. The history is built only from
//! local state and never leaves the device.
//!
//! Each `address_history` row is one address of one account, with two
//! groups of counts:
//!
//! - **Mirror counts** are rebuilt from the mirrored mail whenever a sync
//!   changes it ([`rebuild_account_address_history`]). A message whose
//!   sender is one of the account's identities counts as *sent* to each of
//!   its recipients; any other message counts as *received* from its
//!   sender. Junk is skipped, so spam senders are never suggested. The
//!   mirror is rebuilt rather than counted message by message because
//!   moved and re-synced messages come back with new remote ids and would
//!   otherwise count twice.
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
use zmail_core::{AddressSuggestion, participants::parse_address_list, time::parse_timestamp};

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
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty())
            && (self.display_name.is_empty() || at > self.named_at)
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
            let senders = parse_address_list(&sender);
            let is_outgoing = senders
                .iter()
                .any(|participant| own.contains(&participant.key()));
            if is_outgoing {
                for recipient in parse_address_list(&recipients) {
                    tally(&mut tallies, &own, &recipient, at, count, 0);
                }
            } else {
                for sender in senders {
                    tally(&mut tallies, &own, &sender, at, 0, count);
                }
            }
        }
    }

    transaction.execute(
        r#"
        UPDATE address_history
        SET sent_count = 0, received_count = 0, mirror_last_used_at = NULL
        WHERE account_id = ?1
        "#,
        params![account_id],
    )?;
    {
        let mut upsert = transaction.prepare(
            r#"
            INSERT INTO address_history(
                account_id, address, display_name, sent_count, received_count, mirror_last_used_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(account_id, address) DO UPDATE SET
                display_name = CASE WHEN excluded.display_name = ''
                    THEN address_history.display_name ELSE excluded.display_name END,
                sent_count = excluded.sent_count,
                received_count = excluded.received_count,
                mirror_last_used_at = excluded.mirror_last_used_at
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
    Ok(())
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
            has_remote_content: false,
            is_read: true,
            is_flagged: false,
            received_at: at.to_owned(),
            attachments: Vec::new(),
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

        let reopened = LocalStore::open(store.path()).expect("store reopens");
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
