//! Partial writes to the mirror: mail fetched in stretches, such as a
//! Proton account's older mail arriving after its newest, and changes to
//! single messages, such as a Proton event batch (ADR 0004).
//!
//! [`LocalStore::replace_account_snapshot`] mirrors a complete snapshot and
//! removes whatever it does not hold. A stretch of history holds only some
//! of the account's mail, so it is merged instead: nothing is removed, and
//! a conversation whose older messages arrive in a later stretch than its
//! newer ones keeps both. A [`MailDelta`] is merged the same way, and its
//! messages also leave the places they no longer have; nothing else is
//! read or rewritten.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use rusqlite::{Transaction, TransactionBehavior, params};
use zmail_core::{
    AccountSyncSnapshot, MailDelta, MailboxSnapshot, MessageAttachment, MessageSnapshot,
    ThreadSnapshot,
};

use crate::{
    ExistingMailbox, LocalStore, address_history, flush_search_index, identity_id_map,
    load_account_mailbox_ids, load_pending_op_remote_ids, load_stored_message_attachments,
    load_stored_thread, merge_mailbox_threads, primary_identity_id, thread_message_snapshots,
};

impl LocalStore {
    /// Adds a partial snapshot to the local mirror without removing
    /// anything.
    ///
    /// Mailboxes are matched by name and created when missing, threads by
    /// their remote thread id within the mailbox, and messages by their
    /// remote id within the thread. Mailboxes, threads and messages the
    /// snapshot does not hold are left alone. A thread that gains messages
    /// keeps the ones it had, and its counts, latest time, participants
    /// (distinct senders, newest first), subject and preview (from its
    /// newest side) are worked out over all of them; the mailbox counters
    /// follow.
    ///
    /// As with [`Self::replace_account_snapshot`], read and flag state
    /// with a queued write-back survives, and hydrated bodies, attachment
    /// metadata and dismissed sender warnings are kept. Merging the same
    /// snapshot again changes nothing. The snapshot's identities are not
    /// applied.
    pub fn merge_account_snapshot(
        &self,
        account_id: i64,
        snapshot: &AccountSyncSnapshot,
    ) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the snapshot merge transaction")?;
        let context = MergeContext::load(&transaction, account_id)?;
        let changed = merge_mailboxes(&transaction, &context, &snapshot.mailboxes)?;
        finish(&transaction, account_id, &changed)?;
        transaction
            .commit()
            .context("failed to commit the snapshot merge transaction")
    }

    /// Applies changes to some messages of an account, touching only those
    /// messages, their threads and their mailboxes.
    ///
    /// Each message in [`MailDelta::message_ids`] is first removed from
    /// every mailbox and thread the delta does not place it in (all of
    /// them, for a deleted message); a thread left empty is removed, and
    /// the others have their counts, latest time, subject and participants
    /// worked out again. The placed messages are then merged as
    /// [`Self::merge_account_snapshot`] merges them. A message that moves
    /// keeps its hydrated body and attachment metadata, and queued read
    /// and flag state survives as with a snapshot. One transaction.
    ///
    /// Returns the remote ids of the delta's messages the mirror did not
    /// hold before.
    pub fn apply_mail_delta(&self, account_id: i64, delta: &MailDelta) -> Result<Vec<String>> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the mail delta transaction")?;
        let context = MergeContext::load(&transaction, account_id)?;

        let mut places: HashMap<&str, HashSet<(&str, &str)>> = HashMap::new();
        for mailbox in &delta.mailboxes {
            for thread in &mailbox.threads {
                for message in &thread.messages {
                    places
                        .entry(message.remote_id.as_str())
                        .or_default()
                        .insert((mailbox.name.as_str(), thread.remote_thread_id.as_str()));
                }
            }
        }
        let mut covered: Vec<&str> = delta
            .message_ids
            .iter()
            .map(String::as_str)
            .chain(places.keys().copied())
            .filter(|id| !id.is_empty())
            .collect();
        covered.sort_unstable();
        covered.dedup();

        // Leave the old places, remembering each message's hydrated state
        // so a move does not lose it.
        let mut unknown = Vec::new();
        let mut hydration: HashMap<&str, Hydration> = HashMap::new();
        let mut touched_threads: HashSet<i64> = HashSet::new();
        let mut changed: HashSet<i64> = HashSet::new();
        for remote_id in covered {
            let rows = stored_places(&transaction, account_id, remote_id)?;
            if rows.is_empty() {
                unknown.push(remote_id.to_owned());
            }
            let keep = places.get(remote_id);
            for row in rows {
                if row.has_body && !hydration.contains_key(remote_id) {
                    hydration.insert(remote_id, load_hydration(&transaction, row.message_id)?);
                }
                let stays = keep.is_some_and(|keep| {
                    keep.contains(&(row.mailbox_name.as_str(), row.remote_thread_id.as_str()))
                });
                if !stays {
                    transaction
                        .execute(
                            "DELETE FROM messages WHERE id = ?1",
                            params![row.message_id],
                        )
                        .context("failed to remove a message during the mail delta")?;
                    touched_threads.insert(row.thread_id);
                    changed.insert(row.mailbox_id);
                }
            }
        }
        for thread_id in touched_threads {
            settle_thread(&transaction, thread_id)?;
        }

        let mut mailboxes = delta.mailboxes.clone();
        for message in mailboxes
            .iter_mut()
            .flat_map(|mailbox| mailbox.threads.iter_mut())
            .flat_map(|thread| thread.messages.iter_mut())
        {
            if message.body_text.is_none()
                && message.sanitized_html.is_none()
                && let Some(stored) = hydration.get(message.remote_id.as_str())
            {
                stored.apply_to(message);
            }
        }
        changed.extend(merge_mailboxes(&transaction, &context, &mailboxes)?);
        finish(&transaction, account_id, &changed)?;
        transaction
            .commit()
            .context("failed to commit the mail delta transaction")?;
        Ok(unknown)
    }
}

/// What every partial write reads once up front.
struct MergeContext {
    account_id: i64,
    identity_map: HashMap<String, i64>,
    fallback_identity_id: i64,
    existing_mailboxes: HashMap<String, ExistingMailbox>,
    pending_remote_ids: HashSet<String>,
}

impl MergeContext {
    fn load(transaction: &Transaction<'_>, account_id: i64) -> Result<Self> {
        Ok(Self {
            account_id,
            identity_map: identity_id_map(transaction, account_id)?,
            fallback_identity_id: primary_identity_id(transaction, account_id)?
                .context("cannot merge mail without identities")?,
            existing_mailboxes: load_account_mailbox_ids(transaction, account_id)?,
            pending_remote_ids: load_pending_op_remote_ids(transaction, account_id)?,
        })
    }
}

/// Merges partial mailboxes into the mirror, creating missing mailboxes,
/// and returns the ids of the mailboxes whose threads changed.
fn merge_mailboxes(
    transaction: &Transaction<'_>,
    context: &MergeContext,
    mailboxes: &[MailboxSnapshot],
) -> Result<HashSet<i64>> {
    let mut changed_mailboxes = HashSet::new();
    for (sort_order, mailbox) in mailboxes.iter().enumerate() {
        let mailbox_id = match context.existing_mailboxes.get(&mailbox.name) {
            Some(existing) => existing.id,
            None => {
                transaction
                    .execute(
                        r#"
                        INSERT INTO mailboxes(
                            account_id, name, role, sort_order, delimiter, parent_name,
                            total_threads, unread_threads
                        )
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0)
                        "#,
                        params![
                            context.account_id,
                            mailbox.name,
                            mailbox.role,
                            sort_order as i64,
                            mailbox.delimiter,
                            mailbox.parent
                        ],
                    )
                    .context("failed to insert the mailbox during the snapshot merge")?;
                transaction.last_insert_rowid()
            }
        };
        if mailbox.threads.is_empty() {
            continue;
        }

        let combined = MailboxSnapshot {
            name: mailbox.name.clone(),
            role: mailbox.role.clone(),
            delimiter: mailbox.delimiter.clone(),
            parent: mailbox.parent.clone(),
            threads: combine_with_stored_threads(transaction, mailbox_id, &mailbox.threads)?,
        };
        let (_, changed) = merge_mailbox_threads(
            transaction,
            mailbox_id,
            &combined,
            &context.identity_map,
            context.fallback_identity_id,
            &context.pending_remote_ids,
        )?;
        if changed {
            changed_mailboxes.insert(mailbox_id);
        }
    }
    Ok(changed_mailboxes)
}

/// Refreshes the changed mailboxes' counters, the address history when
/// mail changed, and the search index.
fn finish(
    transaction: &Transaction<'_>,
    account_id: i64,
    changed_mailboxes: &HashSet<i64>,
) -> Result<()> {
    for mailbox_id in changed_mailboxes {
        transaction
            .execute(
                r#"
                UPDATE mailboxes
                SET
                    total_threads = (
                        SELECT COUNT(*) FROM threads WHERE mailbox_id = mailboxes.id
                    ),
                    unread_threads = (
                        SELECT COUNT(*) FROM threads
                        WHERE mailbox_id = mailboxes.id AND unread_count > 0
                    )
                WHERE id = ?1
                "#,
                params![mailbox_id],
            )
            .context("failed to refresh the mailbox counters during the snapshot merge")?;
    }
    if !changed_mailboxes.is_empty() {
        address_history::rebuild_account_address_history(transaction, account_id)?;
    }
    flush_search_index(transaction)
}

/// One stored copy of a message: where it is, and whether it has a body.
struct StoredPlace {
    message_id: i64,
    thread_id: i64,
    remote_thread_id: String,
    mailbox_id: i64,
    mailbox_name: String,
    has_body: bool,
}

/// Every stored copy of a message in the account's mailboxes.
fn stored_places(
    transaction: &Transaction<'_>,
    account_id: i64,
    remote_id: &str,
) -> Result<Vec<StoredPlace>> {
    transaction
        .prepare_cached(
            r#"
            SELECT messages.id, threads.id, COALESCE(threads.remote_thread_id, ''),
                   mailboxes.id, mailboxes.name,
                   messages.body_text IS NOT NULL OR messages.sanitized_html IS NOT NULL
            FROM messages
            JOIN threads ON threads.id = messages.thread_id
            JOIN mailboxes ON mailboxes.id = threads.mailbox_id
            WHERE mailboxes.account_id = ?1 AND messages.remote_id = ?2
            ORDER BY messages.id ASC
            "#,
        )?
        .query_map(params![account_id, remote_id], |row| {
            Ok(StoredPlace {
                message_id: row.get(0)?,
                thread_id: row.get(1)?,
                remote_thread_id: row.get(2)?,
                mailbox_id: row.get(3)?,
                mailbox_name: row.get(4)?,
                has_body: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to find a message's stored copies")
}

/// A message's hydrated state, carried to its new place on a move.
struct Hydration {
    body_text: Option<String>,
    sanitized_html: Option<String>,
    raw_html: Option<String>,
    has_remote_content: bool,
    body_preview: String,
    attachments: Vec<MessageAttachment>,
}

impl Hydration {
    fn apply_to(&self, message: &mut MessageSnapshot) {
        message.body_text = self.body_text.clone();
        message.sanitized_html = self.sanitized_html.clone();
        message.raw_html = self.raw_html.clone();
        message.has_remote_content = self.has_remote_content;
        message.body_preview = self.body_preview.clone();
        if message.attachments.is_empty() {
            message.attachments = self.attachments.clone();
        }
    }
}

fn load_hydration(transaction: &Transaction<'_>, message_id: i64) -> Result<Hydration> {
    let (body_text, sanitized_html, has_remote_content, body_preview, raw_html) = transaction
        .query_row(
            "SELECT body_text, sanitized_html, has_remote_content, body_preview, raw_html \
             FROM messages WHERE id = ?1",
            params![message_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .context("failed to load a message body during the mail delta")?;
    let attachments = load_stored_message_attachments(transaction, message_id)?
        .into_iter()
        .map(MessageAttachment::from)
        .collect();
    Ok(Hydration {
        body_text,
        sanitized_html,
        raw_html,
        has_remote_content,
        body_preview,
        attachments,
    })
}

/// Removes a thread that lost its last message, or works out its counts,
/// latest time, subject and participants again from the messages left.
fn settle_thread(transaction: &Transaction<'_>, thread_id: i64) -> Result<()> {
    let messages = thread_message_snapshots(transaction, thread_id)?;
    let Some(newest) = messages.last() else {
        transaction
            .execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
            .context("failed to remove an emptied thread during the mail delta")?;
        return Ok(());
    };
    transaction
        .execute(
            r#"
            UPDATE threads
            SET
                subject = ?2,
                participants = ?3,
                is_flagged = ?4,
                message_count = ?5,
                unread_count = ?6,
                latest_message_at = ?7,
                latest_message_id = (
                    SELECT id FROM messages WHERE thread_id = ?1
                    ORDER BY received_at DESC, id DESC LIMIT 1
                )
            WHERE id = ?1
            "#,
            params![
                thread_id,
                newest.subject,
                newest_first_senders(&messages),
                messages.iter().any(|message| message.is_flagged),
                messages.len() as i64,
                messages.iter().filter(|message| !message.is_read).count() as i64,
                newest.received_at,
            ],
        )
        .context("failed to update a thread during the mail delta")?;
    Ok(())
}

/// The distinct senders of messages sorted oldest first, newest first.
fn newest_first_senders(messages: &[MessageSnapshot]) -> String {
    let mut senders: Vec<&str> = Vec::new();
    for message in messages.iter().rev() {
        if !senders.contains(&message.sender.as_str()) {
            senders.push(&message.sender);
        }
    }
    senders.join(", ")
}

/// Each incoming thread joined with the messages its stored counterpart
/// already holds, so the rewrite in `merge_mailbox_threads` keeps them.
fn combine_with_stored_threads(
    transaction: &Transaction<'_>,
    mailbox_id: i64,
    incoming: &[ThreadSnapshot],
) -> Result<Vec<ThreadSnapshot>> {
    let mut statement = transaction.prepare_cached(
        "SELECT id FROM threads WHERE mailbox_id = ?1 AND remote_thread_id = ?2 \
         ORDER BY id ASC LIMIT 1",
    )?;
    let mut combined = Vec::with_capacity(incoming.len());
    for thread in incoming.iter().filter(|thread| !thread.messages.is_empty()) {
        let stored_id: Option<i64> = statement
            .query_map(params![mailbox_id, thread.remote_thread_id], |row| {
                row.get(0)
            })?
            .next()
            .transpose()
            .context("failed to look up the stored thread during the snapshot merge")?;
        combined.push(match stored_id {
            Some(stored_id) => combine_thread(transaction, stored_id, thread)?,
            None => combine(thread, Vec::new(), None),
        });
    }
    Ok(combined)
}

fn combine_thread(
    transaction: &Transaction<'_>,
    stored_id: i64,
    incoming: &ThreadSnapshot,
) -> Result<ThreadSnapshot> {
    let stored = load_stored_thread(transaction, stored_id)?
        .context("the stored thread vanished during the snapshot merge")?;
    let messages = thread_message_snapshots(transaction, stored_id)?;
    Ok(combine(
        incoming,
        messages,
        Some(StoredSide {
            subject: stored.subject,
            preview: stored.preview,
            has_attachments: stored.has_attachments,
            latest_message_at: stored.latest_message_at,
        }),
    ))
}

/// What a stored thread contributes besides its messages.
struct StoredSide {
    subject: String,
    preview: String,
    has_attachments: bool,
    latest_message_at: String,
}

/// The thread holding the stored messages and the incoming ones, with the
/// incoming copy of a message replacing the stored one (the merge puts a
/// stored body back on it).
fn combine(
    incoming: &ThreadSnapshot,
    stored_messages: Vec<MessageSnapshot>,
    stored: Option<StoredSide>,
) -> ThreadSnapshot {
    let incoming_ids: HashSet<&str> = incoming
        .messages
        .iter()
        .map(|message| message.remote_id.as_str())
        .filter(|id| !id.is_empty())
        .collect();
    let mut messages: Vec<MessageSnapshot> = stored_messages
        .into_iter()
        .filter(|message| {
            message.remote_id.is_empty() || !incoming_ids.contains(message.remote_id.as_str())
        })
        .collect();
    messages.extend(incoming.messages.iter().cloned());
    messages.sort_by(|left, right| {
        left.received_at
            .cmp(&right.received_at)
            .then_with(|| left.remote_id.cmp(&right.remote_id))
    });

    let participants = newest_first_senders(&messages);

    let (subject, preview, latest_message_at, had_attachments) = match stored {
        Some(stored) if stored.latest_message_at > incoming.latest_message_at => (
            stored.subject,
            stored.preview,
            stored.latest_message_at,
            stored.has_attachments,
        ),
        Some(stored) => (
            incoming.subject.clone(),
            incoming.preview.clone(),
            incoming.latest_message_at.clone(),
            stored.has_attachments,
        ),
        None => (
            incoming.subject.clone(),
            incoming.preview.clone(),
            incoming.latest_message_at.clone(),
            false,
        ),
    };
    ThreadSnapshot {
        remote_thread_id: incoming.remote_thread_id.clone(),
        subject,
        participants,
        preview,
        is_flagged: incoming.is_flagged || messages.iter().any(|message| message.is_flagged),
        has_attachments: incoming.has_attachments || had_attachments,
        unread_count: messages.iter().filter(|message| !message.is_read).count(),
        latest_message_at,
        messages,
    }
}

#[cfg(test)]
mod tests {
    use zmail_core::{
        AccountSyncSnapshot, MailDelta, MailboxSnapshot, MessageAttachment, MessageSnapshot,
        ProtocolKind, ThreadSnapshot,
    };

    use crate::LocalStore;

    fn message(id: &str, sender: &str, at: &str, read: bool) -> MessageSnapshot {
        MessageSnapshot {
            remote_id: id.to_owned(),
            identity_email: "phase2@localhost".to_owned(),
            sender: sender.to_owned(),
            reply_to: None,
            recipients: "phase2@localhost".to_owned(),
            subject: format!("Subject {id}"),
            body_preview: String::new(),
            body_text: None,
            sanitized_html: None,
            raw_html: None,
            has_remote_content: false,
            is_read: read,
            is_flagged: false,
            received_at: at.to_owned(),
            attachments: Vec::new(),
        }
    }

    /// A thread as the Proton adapter builds one: the newest message names
    /// it.
    fn thread(id: &str, messages: Vec<MessageSnapshot>) -> ThreadSnapshot {
        let newest = messages
            .iter()
            .max_by(|left, right| left.received_at.cmp(&right.received_at))
            .expect("a message");
        let mut participants: Vec<String> = Vec::new();
        for message in messages.iter().rev() {
            if !participants.contains(&message.sender) {
                participants.push(message.sender.clone());
            }
        }
        ThreadSnapshot {
            remote_thread_id: id.to_owned(),
            subject: newest.subject.clone(),
            participants: participants.join(", "),
            preview: String::new(),
            is_flagged: messages.iter().any(|message| message.is_flagged),
            has_attachments: false,
            unread_count: messages.iter().filter(|message| !message.is_read).count(),
            latest_message_at: newest.received_at.clone(),
            messages,
        }
    }

    fn snapshot(mailboxes: Vec<(&str, Vec<ThreadSnapshot>)>) -> AccountSyncSnapshot {
        AccountSyncSnapshot {
            checkpoint: "checkpoint".to_owned(),
            detail: "detail".to_owned(),
            identities: None,
            mailboxes: mailboxes
                .into_iter()
                .map(|(name, threads)| MailboxSnapshot {
                    name: name.to_owned(),
                    role: (name == "INBOX").then(|| "inbox".to_owned()),
                    threads,
                    ..MailboxSnapshot::default()
                })
                .collect(),
        }
    }

    fn store() -> (LocalStore, i64) {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        (store, account_id)
    }

    fn mailbox_id(store: &LocalStore, account_id: i64, name: &str) -> i64 {
        store
            .list_mailboxes(account_id)
            .expect("mailboxes")
            .into_iter()
            .find(|mailbox| mailbox.name == name)
            .unwrap_or_else(|| panic!("no {name} mailbox"))
            .id
    }

    /// The remote ids of a mailbox's threads, and of each thread's
    /// messages, oldest first.
    fn contents(store: &LocalStore, account_id: i64, name: &str) -> Vec<(String, Vec<String>)> {
        let mirror = store.account_mailbox_snapshots(account_id).expect("mirror");
        let mailbox = mirror
            .iter()
            .find(|mailbox| mailbox.name == name)
            .unwrap_or_else(|| panic!("no {name} mailbox"));
        let mut threads: Vec<(String, Vec<String>)> = mailbox
            .threads
            .iter()
            .map(|thread| {
                (
                    thread.remote_thread_id.clone(),
                    thread
                        .messages
                        .iter()
                        .map(|message| message.remote_id.clone())
                        .collect(),
                )
            })
            .collect();
        threads.sort();
        threads
    }

    #[test]
    fn older_messages_of_a_conversation_join_the_newer_ones() {
        let (store, account_id) = store();
        // The first stretch has the newest message of conversation c1.
        store
            .replace_account_snapshot(
                account_id,
                &snapshot(vec![(
                    "INBOX",
                    vec![thread(
                        "c1",
                        vec![message("m3", "Bea", "2026-10-03T00:00:00+00:00", true)],
                    )],
                )]),
            )
            .expect("first stretch");
        let inbox = mailbox_id(&store, account_id, "INBOX");
        let thread_id = store.list_threads(inbox, 0..10).expect("threads")[0].id;

        // A later stretch brings its two older messages.
        store
            .merge_account_snapshot(
                account_id,
                &snapshot(vec![(
                    "INBOX",
                    vec![thread(
                        "c1",
                        vec![
                            message("m2", "Ann", "2026-10-02T00:00:00+00:00", false),
                            message("m1", "Bea", "2026-10-01T00:00:00+00:00", false),
                        ],
                    )],
                )]),
            )
            .expect("older stretch");

        assert_eq!(
            contents(&store, account_id, "INBOX"),
            [(
                "c1".to_owned(),
                vec!["m1".to_owned(), "m2".to_owned(), "m3".to_owned()]
            )]
        );
        let summary = store.list_threads(inbox, 0..10).expect("threads")[0].clone();
        assert_eq!(summary.id, thread_id, "the thread row is kept");
        assert_eq!(summary.subject, "Subject m3", "the newest message names it");
        assert_eq!(summary.unread_count, 2);
        let mirror = store.account_mailbox_snapshots(account_id).expect("mirror");
        let stored = &mirror[0].threads[0];
        assert_eq!(stored.latest_message_at, "2026-10-03T00:00:00+00:00");
        assert_eq!(stored.participants, "Bea, Ann");
        let mailbox = store.list_mailboxes(account_id).expect("mailboxes")[0].clone();
        assert_eq!((mailbox.total_threads, mailbox.unread_threads), (1, 1));
    }

    #[test]
    fn leaves_unrelated_threads_and_mailboxes_alone() {
        let (store, account_id) = store();
        store
            .replace_account_snapshot(
                account_id,
                &snapshot(vec![
                    (
                        "INBOX",
                        vec![
                            thread(
                                "c1",
                                vec![message("m1", "Ann", "2026-10-05T00:00:00+00:00", false)],
                            ),
                            thread(
                                "c2",
                                vec![message("m2", "Ann", "2026-10-04T00:00:00+00:00", true)],
                            ),
                        ],
                    ),
                    (
                        "Sent",
                        vec![thread(
                            "c3",
                            vec![message("m3", "Me", "2026-10-04T00:00:00+00:00", true)],
                        )],
                    ),
                ]),
            )
            .expect("first stretch");
        let inbox = mailbox_id(&store, account_id, "INBOX");
        let before: Vec<i64> = store
            .list_threads(inbox, 0..10)
            .expect("threads")
            .iter()
            .map(|thread| thread.id)
            .collect();

        // An older stretch with one new thread, in a mailbox the first had
        // and one it did not; Sent is not in it at all.
        store
            .merge_account_snapshot(
                account_id,
                &snapshot(vec![
                    (
                        "INBOX",
                        vec![thread(
                            "c4",
                            vec![message("m4", "Cy", "2026-09-01T00:00:00+00:00", false)],
                        )],
                    ),
                    (
                        "Archive",
                        vec![thread(
                            "c5",
                            vec![message("m5", "Cy", "2026-08-01T00:00:00+00:00", true)],
                        )],
                    ),
                ]),
            )
            .expect("older stretch");

        assert_eq!(
            contents(&store, account_id, "INBOX"),
            [
                ("c1".to_owned(), vec!["m1".to_owned()]),
                ("c2".to_owned(), vec!["m2".to_owned()]),
                ("c4".to_owned(), vec!["m4".to_owned()]),
            ]
        );
        assert_eq!(
            contents(&store, account_id, "Sent"),
            [("c3".to_owned(), vec!["m3".to_owned()])]
        );
        assert_eq!(
            contents(&store, account_id, "Archive"),
            [("c5".to_owned(), vec!["m5".to_owned()])]
        );
        let after: Vec<i64> = store
            .list_threads(inbox, 0..10)
            .expect("threads")
            .iter()
            .map(|thread| thread.id)
            .filter(|id| before.contains(id))
            .collect();
        assert_eq!(after.len(), 2, "the existing threads keep their rows");
        let counters: Vec<(String, usize, usize)> = store
            .list_mailboxes(account_id)
            .expect("mailboxes")
            .into_iter()
            .map(|mailbox| (mailbox.name, mailbox.total_threads, mailbox.unread_threads))
            .collect();
        for expected in [
            ("INBOX".to_owned(), 3, 2),
            ("Sent".to_owned(), 1, 0),
            ("Archive".to_owned(), 1, 0),
        ] {
            assert!(counters.contains(&expected), "{counters:?}");
        }
    }

    #[test]
    fn merging_again_changes_nothing() {
        let (store, account_id) = store();
        store
            .replace_account_snapshot(
                account_id,
                &snapshot(vec![(
                    "INBOX",
                    vec![thread(
                        "c1",
                        vec![message("m2", "Bea", "2026-10-02T00:00:00+00:00", false)],
                    )],
                )]),
            )
            .expect("first stretch");
        let older = snapshot(vec![(
            "INBOX",
            vec![
                thread(
                    "c1",
                    vec![message("m1", "Ann", "2026-10-01T00:00:00+00:00", true)],
                ),
                thread(
                    "c9",
                    vec![message("m9", "Ann", "2026-09-01T00:00:00+00:00", false)],
                ),
            ],
        )]);
        store
            .merge_account_snapshot(account_id, &older)
            .expect("older stretch");
        let inbox = mailbox_id(&store, account_id, "INBOX");
        let view = |store: &LocalStore| {
            store
                .list_threads(inbox, 0..10)
                .expect("threads")
                .into_iter()
                .map(|thread| {
                    let messages: Vec<i64> = store
                        .thread_view(thread.id)
                        .expect("view")
                        .expect("thread")
                        .messages
                        .iter()
                        .map(|message| message.id)
                        .collect();
                    (thread.id, thread.unread_count, messages)
                })
                .collect::<Vec<_>>()
        };
        let before = view(&store);
        let mirror_before = store.account_mailbox_snapshots(account_id).expect("mirror");

        store
            .merge_account_snapshot(account_id, &older)
            .expect("the same stretch again");
        assert_eq!(view(&store), before, "no row was rewritten");
        assert_eq!(
            store.account_mailbox_snapshots(account_id).expect("mirror"),
            mirror_before
        );
        let mailbox = store.list_mailboxes(account_id).expect("mailboxes")[0].clone();
        assert_eq!((mailbox.total_threads, mailbox.unread_threads), (2, 2));
    }

    #[test]
    fn keeps_bodies_attachments_and_queued_local_state() {
        let (store, account_id) = store();
        let mut opened = message("m2", "Bea", "2026-10-02T00:00:00+00:00", false);
        opened.body_text = Some("opened body".to_owned());
        opened.body_preview = "opened body".to_owned();
        opened.attachments = vec![MessageAttachment {
            filename: "notes.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            size_bytes: 5,
            storage_path: "blob-1".to_owned(),
            content_id: None,
            inline_only: false,
        }];
        let mut first = thread("c1", vec![opened]);
        first.has_attachments = true;
        store
            .replace_account_snapshot(account_id, &snapshot(vec![("INBOX", vec![first])]))
            .expect("first stretch");
        let inbox = mailbox_id(&store, account_id, "INBOX");
        let thread_id = store.list_threads(inbox, 0..10).expect("threads")[0].id;
        // Read and flagged locally, not yet written back.
        store.set_thread_read(thread_id, true).expect("read");
        store.set_thread_flagged(thread_id, true).expect("flag");

        // The older stretch lists the conversation again, as the server
        // has it: m2 unread, unflagged and without its body.
        store
            .merge_account_snapshot(
                account_id,
                &snapshot(vec![(
                    "INBOX",
                    vec![thread(
                        "c1",
                        vec![
                            message("m2", "Bea", "2026-10-02T00:00:00+00:00", false),
                            message("m1", "Ann", "2026-10-01T00:00:00+00:00", true),
                        ],
                    )],
                )]),
            )
            .expect("older stretch");

        let view = store.thread_view(thread_id).expect("view").expect("thread");
        assert_eq!(view.messages.len(), 2);
        let m2 = view
            .messages
            .iter()
            .find(|message| message.body_text.is_some())
            .expect("m2 keeps its body");
        assert_eq!(m2.body_text.as_deref(), Some("opened body"));
        assert_eq!(m2.attachments.len(), 1);
        assert_eq!(m2.attachments[0].filename, "notes.txt");
        let summary = store.list_threads(inbox, 0..10).expect("threads")[0].clone();
        assert!(summary.is_flagged, "the queued flag survives");
        assert_eq!(summary.unread_count, 0, "the queued read state survives");
        assert!(summary.has_attachments);
    }

    fn delta(mailboxes: Vec<(&str, Vec<ThreadSnapshot>)>, message_ids: &[&str]) -> MailDelta {
        MailDelta {
            mailboxes: snapshot(mailboxes).mailboxes,
            message_ids: message_ids.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    fn counters(store: &LocalStore, account_id: i64) -> Vec<(String, usize, usize)> {
        let mut counters: Vec<(String, usize, usize)> = store
            .list_mailboxes(account_id)
            .expect("mailboxes")
            .into_iter()
            .map(|mailbox| (mailbox.name, mailbox.total_threads, mailbox.unread_threads))
            .collect();
        counters.sort();
        counters
    }

    /// INBOX and All Mail with conversation c1 (m1, m2) and c2 (m3); m1 is
    /// opened.
    fn mirrored() -> (LocalStore, i64) {
        let (store, account_id) = store();
        let mut m1 = message("m1", "Ann", "2026-10-01T00:00:00+00:00", true);
        m1.body_text = Some("m1 body".to_owned());
        m1.body_preview = "m1 body".to_owned();
        let c1 = thread(
            "c1",
            vec![m1, message("m2", "Bea", "2026-10-02T00:00:00+00:00", false)],
        );
        let c2 = thread(
            "c2",
            vec![message("m3", "Cy", "2026-10-03T00:00:00+00:00", false)],
        );
        store
            .replace_account_snapshot(
                account_id,
                &snapshot(vec![
                    ("INBOX", vec![c1.clone(), c2.clone()]),
                    ("All Mail", vec![c1, c2]),
                    ("Archive", Vec::new()),
                ]),
            )
            .expect("mirror");
        (store, account_id)
    }

    /// Message row ids of every thread, by mailbox and conversation.
    fn rows(store: &LocalStore, account_id: i64) -> Vec<(String, String, Vec<i64>)> {
        let mut rows = Vec::new();
        for mailbox in store.list_mailboxes(account_id).expect("mailboxes") {
            for thread in store.list_threads(mailbox.id, 0..100).expect("threads") {
                let view = store.thread_view(thread.id).expect("view").expect("thread");
                let remote = store
                    .account_mailbox_snapshots(account_id)
                    .expect("mirror")
                    .into_iter()
                    .find(|snapshot| snapshot.name == mailbox.name)
                    .and_then(|snapshot| {
                        snapshot
                            .threads
                            .into_iter()
                            .find(|candidate| candidate.subject == thread.subject)
                    })
                    .map(|candidate| candidate.remote_thread_id)
                    .unwrap_or_default();
                rows.push((
                    mailbox.name.clone(),
                    remote,
                    view.messages.iter().map(|message| message.id).collect(),
                ));
            }
        }
        rows.sort();
        rows
    }

    #[test]
    fn a_message_moves_between_mailboxes_with_its_body() {
        let (store, account_id) = mirrored();
        let before = rows(&store, account_id);
        // m1 is archived: it leaves INBOX, joins Archive, stays in All Mail.
        let unknown = store
            .apply_mail_delta(
                account_id,
                &delta(
                    vec![
                        (
                            "Archive",
                            vec![thread(
                                "c1",
                                vec![message("m1", "Ann", "2026-10-01T00:00:00+00:00", true)],
                            )],
                        ),
                        (
                            "All Mail",
                            vec![thread(
                                "c1",
                                vec![message("m1", "Ann", "2026-10-01T00:00:00+00:00", true)],
                            )],
                        ),
                    ],
                    &["m1"],
                ),
            )
            .expect("delta");
        assert!(unknown.is_empty(), "m1 was mirrored: {unknown:?}");

        assert_eq!(
            contents(&store, account_id, "INBOX"),
            [
                ("c1".to_owned(), vec!["m2".to_owned()]),
                ("c2".to_owned(), vec!["m3".to_owned()]),
            ]
        );
        assert_eq!(
            contents(&store, account_id, "Archive"),
            [("c1".to_owned(), vec!["m1".to_owned()])]
        );
        assert_eq!(
            contents(&store, account_id, "All Mail"),
            [
                ("c1".to_owned(), vec!["m1".to_owned(), "m2".to_owned()]),
                ("c2".to_owned(), vec!["m3".to_owned()]),
            ]
        );
        let archived = store
            .account_mailbox_snapshots(account_id)
            .expect("mirror")
            .into_iter()
            .find(|mailbox| mailbox.name == "Archive")
            .expect("Archive");
        assert_eq!(
            archived.threads[0].messages[0].body_text.as_deref(),
            Some("m1 body"),
            "the body moves with the message"
        );
        // c1 in INBOX now holds only m2: named and counted by it.
        let inbox = mailbox_id(&store, account_id, "INBOX");
        let c1 = store
            .list_threads(inbox, 0..10)
            .expect("threads")
            .into_iter()
            .find(|thread| thread.subject == "Subject m2")
            .expect("c1 in INBOX");
        assert_eq!(c1.unread_count, 1);
        assert_eq!(
            counters(&store, account_id),
            [
                ("All Mail".to_owned(), 2, 2),
                ("Archive".to_owned(), 1, 0),
                ("INBOX".to_owned(), 2, 2),
            ]
        );
        // c2 was not touched at all.
        let untouched = |rows: &[(String, String, Vec<i64>)]| {
            rows.iter()
                .filter(|(_, thread, _)| thread == "c2")
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(untouched(&rows(&store, account_id)), untouched(&before));
    }

    #[test]
    fn a_deleted_message_leaves_every_mailbox() {
        let (store, account_id) = mirrored();
        store
            .apply_mail_delta(account_id, &delta(Vec::new(), &["m2", "m3"]))
            .expect("delta");
        for name in ["INBOX", "All Mail"] {
            assert_eq!(
                contents(&store, account_id, name),
                [("c1".to_owned(), vec!["m1".to_owned()])],
                "{name}"
            );
        }
        let inbox = mailbox_id(&store, account_id, "INBOX");
        let threads = store.list_threads(inbox, 0..10).expect("threads");
        assert_eq!(threads.len(), 1, "c2 lost its only message");
        assert_eq!(threads[0].subject, "Subject m1", "m1 names c1 now");
        assert_eq!(threads[0].unread_count, 0);
        let mirror = store.account_mailbox_snapshots(account_id).expect("mirror");
        assert_eq!(mirror[0].threads[0].participants, "Ann");
        assert_eq!(
            mirror[0].threads[0].latest_message_at,
            "2026-10-01T00:00:00+00:00"
        );
        assert_eq!(
            counters(&store, account_id),
            [
                ("All Mail".to_owned(), 1, 0),
                ("Archive".to_owned(), 0, 0),
                ("INBOX".to_owned(), 1, 0),
            ]
        );
    }

    #[test]
    fn a_flag_change_rewrites_only_its_thread() {
        let (store, account_id) = mirrored();
        let before = rows(&store, account_id);
        let mut flagged = message("m3", "Cy", "2026-10-03T00:00:00+00:00", true);
        flagged.is_flagged = true;
        let c2 = thread("c2", vec![flagged]);
        store
            .apply_mail_delta(
                account_id,
                &delta(
                    vec![("INBOX", vec![c2.clone()]), ("All Mail", vec![c2])],
                    &["m3"],
                ),
            )
            .expect("delta");
        let inbox = mailbox_id(&store, account_id, "INBOX");
        let c2 = store
            .list_threads(inbox, 0..10)
            .expect("threads")
            .into_iter()
            .find(|thread| thread.subject == "Subject m3")
            .expect("c2");
        assert!(c2.is_flagged && c2.unread_count == 0);
        let c1_rows = |rows: &[(String, String, Vec<i64>)]| {
            rows.iter()
                .filter(|(_, thread, _)| thread == "c1")
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(c1_rows(&rows(&store, account_id)), c1_rows(&before));
        assert_eq!(
            counters(&store, account_id),
            [
                ("All Mail".to_owned(), 2, 1),
                ("Archive".to_owned(), 0, 0),
                ("INBOX".to_owned(), 2, 1),
            ]
        );
    }

    #[test]
    fn a_new_message_joins_its_conversation() {
        let (store, account_id) = mirrored();
        let c1 = thread(
            "c1",
            vec![message("m4", "Dee", "2026-10-04T00:00:00+00:00", false)],
        );
        let unknown = store
            .apply_mail_delta(
                account_id,
                &delta(
                    vec![("INBOX", vec![c1.clone()]), ("All Mail", vec![c1])],
                    &["m4"],
                ),
            )
            .expect("delta");
        assert_eq!(unknown, ["m4"]);
        assert_eq!(
            contents(&store, account_id, "INBOX"),
            [
                (
                    "c1".to_owned(),
                    vec!["m1".to_owned(), "m2".to_owned(), "m4".to_owned()]
                ),
                ("c2".to_owned(), vec!["m3".to_owned()]),
            ]
        );
        let mirror = store.account_mailbox_snapshots(account_id).expect("mirror");
        let c1 = mirror[0]
            .threads
            .iter()
            .find(|thread| thread.remote_thread_id == "c1")
            .expect("c1");
        assert_eq!(c1.subject, "Subject m4");
        assert_eq!(c1.unread_count, 2);
        assert_eq!(c1.participants, "Dee, Bea, Ann");
        assert_eq!(
            c1.messages[0].body_text.as_deref(),
            Some("m1 body"),
            "the conversation's opened message keeps its body"
        );
        // Applying the same delta again changes nothing.
        let before = rows(&store, account_id);
        let again = thread(
            "c1",
            vec![message("m4", "Dee", "2026-10-04T00:00:00+00:00", false)],
        );
        let unknown = store
            .apply_mail_delta(
                account_id,
                &delta(
                    vec![("INBOX", vec![again.clone()]), ("All Mail", vec![again])],
                    &["m4"],
                ),
            )
            .expect("delta again");
        assert!(unknown.is_empty());
        assert_eq!(rows(&store, account_id), before);
        assert_eq!(
            counters(&store, account_id),
            [
                ("All Mail".to_owned(), 2, 2),
                ("Archive".to_owned(), 0, 0),
                ("INBOX".to_owned(), 2, 2),
            ]
        );
    }
}
