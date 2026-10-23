//! Additive snapshot writes: mail fetched in stretches, such as a Proton
//! account's older mail arriving after its newest (ADR 0004).
//!
//! [`LocalStore::replace_account_snapshot`] mirrors a complete snapshot and
//! removes whatever it does not hold. A stretch of history holds only some
//! of the account's mail, so it is merged instead: nothing is removed, and
//! a conversation whose older messages arrive in a later stretch than its
//! newer ones keeps both.

use std::collections::HashSet;

use anyhow::{Context, Result};
use rusqlite::{Transaction, TransactionBehavior, params};
use zmail_core::{AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, ThreadSnapshot};

use crate::{
    LocalStore, address_history, flush_search_index, identity_id_map, load_account_mailbox_ids,
    load_pending_op_remote_ids, load_stored_thread, merge_mailbox_threads, primary_identity_id,
    thread_message_snapshots,
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

        let identity_map = identity_id_map(&transaction, account_id)?;
        let fallback_identity_id = primary_identity_id(&transaction, account_id)?
            .context("cannot merge a mailbox snapshot without identities")?;
        let existing_mailboxes = load_account_mailbox_ids(&transaction, account_id)?;
        let pending_remote_ids = load_pending_op_remote_ids(&transaction, account_id)?;

        let mut mail_changed = false;
        for (sort_order, mailbox) in snapshot.mailboxes.iter().enumerate() {
            let mailbox_id = match existing_mailboxes.get(&mailbox.name) {
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
                                account_id,
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
                threads: combine_with_stored_threads(&transaction, mailbox_id, &mailbox.threads)?,
            };
            let (_, changed) = merge_mailbox_threads(
                &transaction,
                mailbox_id,
                &combined,
                &identity_map,
                fallback_identity_id,
                &pending_remote_ids,
            )?;
            if changed {
                mail_changed = true;
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
        }

        if mail_changed {
            address_history::rebuild_account_address_history(&transaction, account_id)?;
        }
        flush_search_index(&transaction)?;
        transaction
            .commit()
            .context("failed to commit the snapshot merge transaction")
    }
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

    let mut participants: Vec<&str> = Vec::new();
    for message in messages.iter().rev() {
        if !participants.contains(&message.sender.as_str()) {
            participants.push(&message.sender);
        }
    }
    let participants = participants.join(", ");

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
        AccountSyncSnapshot, MailboxSnapshot, MessageAttachment, MessageSnapshot, ProtocolKind,
        ThreadSnapshot,
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
}
