//! The service's changes and their undo, over an ephemeral store, with a
//! plain callback logging every event. The GUI's wrapper has its own tests
//! (`zmail-ui`'s `mail_actions.rs`).

use std::sync::Arc;

use zmail_core::{
    AccountSyncSnapshot, DraftUpdate, MailboxSnapshot, MessageSnapshot, NewDraft, PendingOpKind,
    ProtocolKind, ThreadSnapshot,
};
use zmail_store::LocalStore;

use super::{InverseOp, MailActions, MailEvent, UNDO_LIMIT};

/// A temporary store with one account holding Inbox, Archive and,
/// optionally, Trash. The Inbox has two threads: an unread, unflagged
/// one with one message, and a partly read one with two (the first
/// read).
struct Fixture {
    store: Arc<LocalStore>,
    account_id: i64,
    inbox_id: i64,
    archive_id: i64,
    trash_id: Option<i64>,
    /// The single-message thread.
    thread_id: i64,
    /// The partly read thread.
    other_id: i64,
}

fn message(remote_id: &str, is_read: bool, at: &str) -> MessageSnapshot {
    MessageSnapshot {
        remote_id: remote_id.to_owned(),
        identity_email: "ops1@zmail.dev".to_owned(),
        sender: "Kai Santos <kai@example.com>".to_owned(),
        reply_to: None,
        recipients: "ops1@zmail.dev".to_owned(),
        subject: "Quarterly report".to_owned(),
        body_preview: "Numbers attached".to_owned(),
        body_text: Some("Numbers attached".to_owned()),
        sanitized_html: None,
        raw_html: None,
        has_remote_content: false,
        is_read,
        is_flagged: false,
        received_at: at.to_owned(),
        attachments: Vec::new(),
    }
}

fn fixture(with_trash: bool) -> Fixture {
    let store = Arc::new(LocalStore::open_ephemeral().expect("store opens"));
    let account_id = store
        .create_demo_account(ProtocolKind::Imap)
        .expect("account is created");
    let thread =
        |remote_thread_id: &str, subject: &str, messages: Vec<MessageSnapshot>| ThreadSnapshot {
            remote_thread_id: remote_thread_id.to_owned(),
            subject: subject.to_owned(),
            participants: "Kai Santos".to_owned(),
            preview: "Numbers attached".to_owned(),
            is_flagged: false,
            has_attachments: false,
            unread_count: messages.iter().filter(|message| !message.is_read).count(),
            latest_message_at: messages
                .last()
                .map(|message| message.received_at.clone())
                .unwrap_or_default(),
            messages,
        };
    let mut mailboxes = vec![
        MailboxSnapshot {
            name: "Inbox".to_owned(),
            role: Some("inbox".to_owned()),
            threads: vec![
                thread(
                    "thread-1",
                    "Quarterly report",
                    vec![message("1", false, "2026-03-21T09:15:00Z")],
                ),
                thread(
                    "thread-2",
                    "Offsite plans",
                    vec![
                        message("2", true, "2026-03-20T09:15:00Z"),
                        message("3", false, "2026-03-20T10:15:00Z"),
                    ],
                ),
            ],
            ..Default::default()
        },
        MailboxSnapshot {
            name: "Archive".to_owned(),
            role: Some("archive".to_owned()),
            threads: Vec::new(),
            ..Default::default()
        },
    ];
    if with_trash {
        mailboxes.push(MailboxSnapshot {
            name: "Trash".to_owned(),
            role: Some("trash".to_owned()),
            threads: Vec::new(),
            ..Default::default()
        });
    }
    store
        .replace_account_snapshot(
            account_id,
            &AccountSyncSnapshot {
                checkpoint: "test".to_owned(),
                detail: "test".to_owned(),
                identities: None,
                mailboxes,
            },
        )
        .expect("snapshot applies");
    let role = |role: &str| store.mailbox_id_by_role(account_id, role).expect("lookup");
    let inbox_id = role("inbox").expect("inbox exists");
    let archive_id = role("archive").expect("archive exists");
    let trash_id = role("trash");
    let threads = store.list_threads(inbox_id, 0..2).expect("threads load");
    Fixture {
        store,
        account_id,
        inbox_id,
        archive_id,
        trash_id,
        thread_id: threads[0].id,
        other_id: threads[1].id,
    }
}

impl Fixture {
    fn mailbox_of(&self, thread_id: i64) -> Option<i64> {
        self.store
            .thread_state(thread_id)
            .expect("thread state loads")
            .map(|thread| thread.mailbox_id)
    }

    /// Each message's (read, flagged) state.
    fn flags_of(&self, thread_id: i64) -> Vec<(bool, bool)> {
        self.store
            .thread_state(thread_id)
            .expect("thread state loads")
            .expect("thread exists")
            .messages
            .iter()
            .map(|message| (message.is_read, message.is_flagged))
            .collect()
    }

    fn queued(&self) -> Vec<PendingOpKind> {
        self.store
            .list_pending_ops(self.account_id)
            .expect("ops load")
            .into_iter()
            .map(|op| op.kind)
            .collect()
    }
}

/// A callback that logs every event into `events`.
fn log(events: &mut Vec<MailEvent>) -> impl FnMut(MailEvent) + '_ {
    move |event| events.push(event)
}

/// A callback for changes whose events a test doesn't look at.
fn ignore(_: MailEvent) {}

fn undo(actions: &mut MailActions) -> Option<String> {
    actions
        .undo(&mut ignore)
        .expect("undo works")
        .map(|entry| entry.label)
}

/// The labels of the recorded entries in `events`.
fn recorded(events: &[MailEvent]) -> Vec<(String, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            MailEvent::UndoRecorded {
                label, can_undo, ..
            } => Some((label.clone(), *can_undo)),
            _ => None,
        })
        .collect()
}

#[test]
fn archive_moves_the_thread_emits_and_undoes() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();

    actions
        .archive_threads(&[fixture.thread_id], &mut log(&mut events))
        .expect("archive works");
    assert_eq!(
        fixture.mailbox_of(fixture.thread_id),
        Some(fixture.archive_id)
    );
    assert_eq!(
        fixture.queued(),
        [PendingOpKind::Move],
        "the write-back is queued"
    );
    let entry = actions.last_undo().expect("an entry is recorded");
    assert_eq!(
        entry.inverses,
        [InverseOp::MoveThread {
            thread_id: fixture.thread_id,
            to_mailbox_id: fixture.inbox_id,
        }]
    );
    let id = entry.id;
    assert_eq!(
        events,
        [
            MailEvent::ThreadMoved {
                thread_id: fixture.thread_id,
                from_mailbox_id: fixture.inbox_id,
                to_mailbox_id: fixture.archive_id,
            },
            MailEvent::UndoRecorded {
                id,
                label: "Conversation archived".to_owned(),
                can_undo: true,
            },
        ]
    );

    events.clear();
    let undone = actions
        .undo(&mut log(&mut events))
        .expect("undo works")
        .map(|entry| entry.label);
    assert_eq!(undone.as_deref(), Some("Conversation archived"));
    assert_eq!(
        fixture.mailbox_of(fixture.thread_id),
        Some(fixture.inbox_id)
    );
    assert_eq!(
        fixture.queued(),
        [],
        "undone before it synced: nothing to write back"
    );
    assert_eq!(
        events,
        [
            MailEvent::ThreadMoved {
                thread_id: fixture.thread_id,
                from_mailbox_id: fixture.archive_id,
                to_mailbox_id: fixture.inbox_id,
            },
            MailEvent::Undone {
                id,
                label: "Conversation archived".to_owned()
            },
        ]
    );
    assert_eq!(actions.undo_entries().len(), 0);
    assert_eq!(undo(&mut actions), None, "an empty stack is not an error");
}

#[test]
fn a_batch_is_one_entry_and_one_undo() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();
    let both = [fixture.thread_id, fixture.other_id];

    actions
        .archive_threads(&both, &mut log(&mut events))
        .expect("archive works");
    actions
        .set_threads_flagged(&both, true, &mut log(&mut events))
        .expect("flag works");
    actions
        .set_threads_read(&both, true, &mut log(&mut events))
        .expect("mark read works");
    assert_eq!(
        recorded(&events),
        [
            ("2 conversations archived".to_owned(), true),
            ("2 conversations flagged".to_owned(), true),
            ("2 conversations marked as read".to_owned(), true),
        ]
    );

    assert_eq!(
        undo(&mut actions).as_deref(),
        Some("2 conversations marked as read")
    );
    // The partly read thread is partly read again.
    assert_eq!(
        fixture.flags_of(fixture.other_id),
        [(true, true), (false, true)]
    );
    undo(&mut actions);
    undo(&mut actions);
    for thread_id in both {
        assert_eq!(fixture.mailbox_of(thread_id), Some(fixture.inbox_id));
    }
    assert_eq!(fixture.flags_of(fixture.thread_id), [(false, false)]);
    assert_eq!(
        fixture.flags_of(fixture.other_id),
        [(true, false), (false, false)]
    );
    // Each message keeps a harmless last-wins read and flag write-back,
    // but no move: the server never sees the archive.
    assert!(
        !fixture.queued().contains(&PendingOpKind::Move),
        "{:?}",
        fixture.queued()
    );
}

#[test]
fn move_records_the_source_mailbox() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();
    let archive_id = fixture.archive_id;

    actions
        .move_threads(&[fixture.thread_id], archive_id, &mut log(&mut events))
        .expect("move works");
    assert_eq!(fixture.mailbox_of(fixture.thread_id), Some(archive_id));
    assert_eq!(
        recorded(&events),
        [("Conversation moved to Archive".to_owned(), true)]
    );

    // Moving to the mailbox it is already in changes nothing.
    events.clear();
    actions
        .move_threads(&[fixture.thread_id], archive_id, &mut log(&mut events))
        .expect("a no-op move works");
    assert!(events.is_empty());

    undo(&mut actions);
    assert_eq!(
        fixture.mailbox_of(fixture.thread_id),
        Some(fixture.inbox_id)
    );
}

#[test]
fn move_leaves_threads_of_other_accounts_where_they_are() {
    let fixture = fixture(true);
    // A second account with a mailbox of its own.
    let second_account = fixture
        .store
        .create_demo_account(ProtocolKind::Imap)
        .expect("a second account");
    fixture
        .store
        .replace_account_snapshot(
            second_account,
            &AccountSyncSnapshot {
                checkpoint: "test".to_owned(),
                detail: "test".to_owned(),
                identities: None,
                mailboxes: vec![MailboxSnapshot {
                    name: "Elsewhere".to_owned(),
                    ..Default::default()
                }],
            },
        )
        .expect("snapshot applies");
    let elsewhere = fixture
        .store
        .list_mailboxes(second_account)
        .expect("mailboxes load")[0]
        .id;
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();

    actions
        .move_threads(&[fixture.thread_id], elsewhere, &mut log(&mut events))
        .expect("the move runs");
    assert_eq!(
        fixture.mailbox_of(fixture.thread_id),
        Some(fixture.inbox_id)
    );
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(actions.undo_entries().len(), 0);
}

#[test]
fn undoing_a_delete_before_it_syncs_leaves_no_server_delete() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();
    let trash_id = fixture.trash_id.expect("trash exists");

    actions
        .delete_threads(
            &[fixture.thread_id, fixture.other_id],
            &mut log(&mut events),
        )
        .expect("delete works");
    assert_eq!(fixture.mailbox_of(fixture.thread_id), Some(trash_id));
    assert_eq!(fixture.queued().len(), 3, "one move per message");
    assert_eq!(
        recorded(&events),
        [("2 conversations moved to Trash".to_owned(), true)]
    );

    undo(&mut actions);
    assert_eq!(
        fixture.mailbox_of(fixture.thread_id),
        Some(fixture.inbox_id)
    );
    assert_eq!(fixture.mailbox_of(fixture.other_id), Some(fixture.inbox_id));
    assert_eq!(fixture.queued(), []);
}

#[test]
fn undoing_after_the_write_back_queues_the_reverse() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());
    let account_id = fixture.account_id;

    actions
        .set_threads_flagged(&[fixture.thread_id], true, &mut ignore)
        .expect("flag works");
    // The sync service writes it back.
    for op in fixture
        .store
        .list_pending_ops(account_id)
        .expect("ops load")
    {
        fixture
            .store
            .complete_pending_op(op.id)
            .expect("op completes");
    }
    undo(&mut actions);
    assert_eq!(fixture.flags_of(fixture.thread_id), [(false, false)]);
    assert_eq!(fixture.queued(), [PendingOpKind::ClearFlagged]);
}

#[test]
fn unflag_and_mark_unread_undo_to_the_state_before() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();
    fixture
        .store
        .set_thread_flagged(fixture.other_id, true)
        .expect("flagged");

    actions
        .set_threads_flagged(&[fixture.other_id], false, &mut log(&mut events))
        .expect("unflag works");
    actions
        .set_threads_read(&[fixture.other_id], false, &mut log(&mut events))
        .expect("mark unread works");
    assert_eq!(
        recorded(&events),
        [
            ("Flag removed".to_owned(), true),
            ("Marked as unread".to_owned(), true),
        ]
    );
    assert_eq!(
        fixture.flags_of(fixture.other_id),
        [(false, false), (false, false)]
    );

    events.clear();
    actions
        .undo(&mut log(&mut events))
        .expect("undo works")
        .expect("there is an entry");
    assert!(events.contains(&MailEvent::ThreadReadChanged {
        thread_id: fixture.other_id,
        read: false,
    }));
    assert_eq!(
        fixture.flags_of(fixture.other_id),
        [(true, false), (false, false)],
        "the first message is read again, the second still unread"
    );
    undo(&mut actions);
    assert_eq!(
        fixture.flags_of(fixture.other_id),
        [(true, true), (false, true)]
    );
}

#[test]
fn permanent_delete_records_an_entry_that_cannot_be_undone() {
    let fixture = fixture(false);
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();

    // Flag first, so there is an older reversible entry underneath.
    actions
        .set_threads_flagged(&[fixture.thread_id], true, &mut log(&mut events))
        .expect("flag works");
    actions
        .delete_threads(&[fixture.thread_id], &mut log(&mut events))
        .expect("delete works");
    assert_eq!(
        fixture.mailbox_of(fixture.thread_id),
        None,
        "the thread is gone locally"
    );
    assert!(events.contains(&MailEvent::ThreadDestroyed {
        thread_id: fixture.thread_id,
        mailbox_id: fixture.inbox_id,
    }));
    assert_eq!(
        recorded(&events).last(),
        Some(&("Conversation deleted".to_owned(), false))
    );

    let error = actions
        .undo(&mut ignore)
        .expect_err("a permanent delete cannot be undone");
    assert!(error.to_string().contains("can't be undone"), "{error}");
    // The older flag entry was not reversed in its place.
    assert_eq!(actions.undo_entries().len(), 1);
}

#[test]
fn undo_entry_undoes_only_its_own_change() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());

    actions
        .archive_threads(&[fixture.thread_id], &mut ignore)
        .expect("archive works");
    let archived = actions.last_undo().expect("the archive is recorded").id;
    actions
        .set_threads_flagged(&[fixture.other_id], true, &mut ignore)
        .expect("flag works");

    // The archive's offer is stale once the flag is the latest change.
    let undone = actions
        .undo_entry(archived, &mut ignore)
        .expect("undo works");
    assert_eq!(undone, None);
    assert_eq!(
        fixture.mailbox_of(fixture.thread_id),
        Some(fixture.archive_id)
    );
    assert_eq!(
        fixture.flags_of(fixture.other_id),
        [(true, true), (false, true)]
    );
}

#[test]
fn undo_skips_threads_that_have_gone_and_fails_when_all_have() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());

    actions
        .archive_threads(&[fixture.thread_id], &mut ignore)
        .expect("archive works");
    // Trashed, then deleted from the trash for good, before the undo.
    actions
        .delete_threads(&[fixture.thread_id], &mut ignore)
        .expect("trash works");
    actions
        .delete_threads(&[fixture.thread_id], &mut ignore)
        .expect("delete from the trash works");
    assert_eq!(fixture.mailbox_of(fixture.thread_id), None);
    // The permanent delete can't be undone, nor the move to the trash of
    // a thread that has gone; the archive's entry is next.
    let _ = actions.undo(&mut ignore);
    let _ = actions.undo(&mut ignore);
    let error = actions
        .undo(&mut ignore)
        .expect_err("nothing is left to undo");
    assert!(error.to_string().contains("no longer here"), "{error}");
}

#[test]
fn opening_a_thread_marks_it_read_without_an_undo_entry() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();

    actions
        .mark_opened_thread_read(fixture.thread_id, &mut log(&mut events))
        .expect("mark read works");
    assert_eq!(
        events,
        [MailEvent::ThreadReadChanged {
            thread_id: fixture.thread_id,
            read: true,
        }]
    );
    assert_eq!(actions.undo_entries().len(), 0);
}

#[test]
fn the_undo_stack_is_capped() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());

    for index in 0..UNDO_LIMIT + 5 {
        actions
            .set_threads_flagged(&[fixture.thread_id], index % 2 == 0, &mut ignore)
            .expect("flag works");
    }
    assert_eq!(actions.undo_entries().len(), UNDO_LIMIT);
}

#[test]
fn draft_changes_write_the_store_and_emit() {
    let fixture = fixture(true);
    let mut actions = MailActions::new(fixture.store.clone());
    let mut events = Vec::new();
    let identity_id = fixture
        .store
        .list_identities(fixture.account_id)
        .expect("identities load")[0]
        .id;

    let draft_id = actions
        .create_draft(
            &NewDraft {
                account_id: fixture.account_id,
                identity_id,
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: String::new(),
                body: String::new(),
            },
            &mut log(&mut events),
        )
        .expect("draft is created");
    actions
        .save_draft(
            draft_id,
            &DraftUpdate {
                identity_id,
                to: vec!["kai@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Hello".to_owned(),
                body: "Hi Kai".to_owned(),
            },
            &mut log(&mut events),
        )
        .expect("draft saves");
    let draft = fixture
        .store
        .draft_detail(draft_id)
        .expect("draft loads")
        .expect("draft exists");
    assert_eq!(draft.subject, "Hello");
    assert_eq!(draft.to, vec!["kai@example.com".to_owned()]);

    let path = std::env::temp_dir().join(format!(
        "zmail-actions-attachment-{}-{draft_id}.txt",
        std::process::id()
    ));
    std::fs::write(&path, b"attachment").expect("fixture file writes");
    let attachment = actions
        .add_draft_attachment(draft_id, &path, &mut log(&mut events))
        .expect("attachment is added");
    actions
        .remove_draft_attachment(draft_id, attachment.id, &mut log(&mut events))
        .expect("attachment is removed");
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        events,
        [
            MailEvent::DraftCreated {
                draft_id,
                account_id: fixture.account_id,
            },
            MailEvent::DraftSaved { draft_id },
            MailEvent::DraftAttachmentsChanged { draft_id },
            MailEvent::DraftAttachmentsChanged { draft_id },
        ]
    );
    assert_eq!(
        actions.undo_entries().len(),
        0,
        "draft edits have their own undo in the text inputs"
    );
}
