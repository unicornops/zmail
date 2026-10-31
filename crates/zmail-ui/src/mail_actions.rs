//! The GUI's `MailActions`: the one place the panes change mail.
//!
//! The changes, their undo stack and the events they make live in the
//! GPUI-free [`zmail_actions`] crate, which the terminal UI uses too (ADR
//! 0007, section 8). This wraps that service in a GPUI entity: each method
//! runs the change with `cx.emit` as its callback, so the workspace shell
//! subscribes to the [`MailEvent`]s and refreshes the panes from the local
//! store, as before.

use std::{path::Path, sync::Arc};

use anyhow::Result;
use gpui::{Context, EventEmitter};
use zmail_core::{DraftAttachment, DraftUpdate, NewDraft};
use zmail_store::LocalStore;

pub(crate) use zmail_actions::{MARK_READ_DELAY, MailEvent, UndoEntry, UndoId};

/// The mail mutation service. One instance lives for the window, owned by
/// the workspace shell and shared with the panes as an `Entity`.
pub(crate) struct MailActions {
    inner: zmail_actions::MailActions,
}

impl EventEmitter<MailEvent> for MailActions {}

/// The callback a change reports its events to: the entity's subscribers.
fn emit<'a>(cx: &'a mut Context<'_, MailActions>) -> impl FnMut(MailEvent) + 'a {
    move |event| cx.emit(event)
}

impl MailActions {
    /// Creates the service over the local store.
    pub(crate) fn new(store: Arc<LocalStore>) -> Self {
        Self {
            inner: zmail_actions::MailActions::new(store),
        }
    }

    /// The recorded undo entries, oldest first.
    #[cfg(test)]
    pub(crate) fn undo_entries(&self) -> impl ExactSizeIterator<Item = &UndoEntry> {
        self.inner.undo_entries()
    }

    /// The entry [`Self::undo`] would reverse next.
    #[cfg(test)]
    pub(crate) fn last_undo(&self) -> Option<&UndoEntry> {
        self.inner.last_undo()
    }

    // -----------------------------------------------------------------------
    // Thread changes (reversible)
    // -----------------------------------------------------------------------

    /// Moves the threads to their account's archive mailbox.
    pub(crate) fn archive_threads(
        &mut self,
        thread_ids: &[i64],
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.inner.archive_threads(thread_ids, &mut emit(cx))
    }

    /// Moves the threads to another mailbox of the same account.
    pub(crate) fn move_threads(
        &mut self,
        thread_ids: &[i64],
        to_mailbox_id: i64,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.inner
            .move_threads(thread_ids, to_mailbox_id, &mut emit(cx))
    }

    /// Deletes the threads: into the trash mailbox when the account has
    /// one, otherwise (or from the trash itself) for good. Only the first
    /// kind can be undone.
    pub(crate) fn delete_threads(
        &mut self,
        thread_ids: &[i64],
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.inner.delete_threads(thread_ids, &mut emit(cx))
    }

    /// Flags or unflags the threads.
    pub(crate) fn set_threads_flagged(
        &mut self,
        thread_ids: &[i64],
        flagged: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.inner
            .set_threads_flagged(thread_ids, flagged, &mut emit(cx))
    }

    /// Marks the threads read or unread because the user asked to.
    pub(crate) fn set_threads_read(
        &mut self,
        thread_ids: &[i64],
        read: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.inner.set_threads_read(thread_ids, read, &mut emit(cx))
    }

    /// Marks a thread read because it stayed open in the reader. Opening a
    /// message is not something to undo, so no entry is recorded.
    pub(crate) fn mark_opened_thread_read(
        &mut self,
        thread_id: i64,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.inner.mark_opened_thread_read(thread_id, &mut emit(cx))
    }

    /// Reverses the most recent recorded change and returns it, or `None`
    /// when there is nothing to undo.
    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) -> Result<Option<UndoEntry>> {
        self.inner.undo(&mut emit(cx))
    }

    /// Reverses the change `id` if it is still the most recent one, as the
    /// Undo button on its toast does. Returns `None` when it is not (it was
    /// undone already, or a newer change came after it).
    pub(crate) fn undo_entry(
        &mut self,
        id: UndoId,
        cx: &mut Context<Self>,
    ) -> Result<Option<UndoEntry>> {
        self.inner.undo_entry(id, &mut emit(cx))
    }

    // -----------------------------------------------------------------------
    // Drafts and the outbox
    // -----------------------------------------------------------------------

    /// Creates a draft and returns its id.
    pub(crate) fn create_draft(&mut self, draft: &NewDraft, cx: &mut Context<Self>) -> Result<i64> {
        self.inner.create_draft(draft, &mut emit(cx))
    }

    /// Saves a draft's fields.
    pub(crate) fn save_draft(
        &mut self,
        draft_id: i64,
        update: &DraftUpdate,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.inner.save_draft(draft_id, update, &mut emit(cx))
    }

    /// Copies the file at `path` into the draft as an attachment.
    pub(crate) fn add_draft_attachment(
        &mut self,
        draft_id: i64,
        path: impl AsRef<Path>,
        cx: &mut Context<Self>,
    ) -> Result<DraftAttachment> {
        self.inner
            .add_draft_attachment(draft_id, path, &mut emit(cx))
    }

    /// Removes an attachment from a draft.
    pub(crate) fn remove_draft_attachment(
        &mut self,
        draft_id: i64,
        attachment_id: i64,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.inner
            .remove_draft_attachment(draft_id, attachment_id, &mut emit(cx))
    }

    /// Takes a queued message back before it is sent (Undo Send). Returns
    /// `false` when it is too late: delivery has started.
    pub(crate) fn cancel_send(
        &mut self,
        job_id: i64,
        draft_id: i64,
        cx: &mut Context<Self>,
    ) -> Result<bool> {
        self.inner.cancel_send(job_id, draft_id, &mut emit(cx))
    }

    /// Requeues a failed outbox job. The caller asks the sync service to
    /// process the outbox afterwards.
    pub(crate) fn retry_outbox_job(&mut self, job_id: i64, cx: &mut Context<Self>) -> Result<()> {
        self.inner.retry_outbox_job(job_id, &mut emit(cx))
    }
}

/// The wrapper: every change's events reach GPUI subscribers, and the
/// toast's undo by id. The changes themselves are tested in
/// `zmail-actions`.
#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    use gpui::{AppContext as _, Entity, TestAppContext};
    use zmail_core::{
        AccountSyncSnapshot, DraftUpdate, MailboxSnapshot, MessageSnapshot, NewDraft, ProtocolKind,
        ThreadSnapshot,
    };
    use zmail_store::LocalStore;

    use super::{MailActions, MailEvent};

    /// A temporary store with one account holding Inbox, Archive and,
    /// optionally, Trash. The Inbox has two threads: an unread, unflagged
    /// one with one message, and a partly read one with two (the first
    /// read).
    struct Fixture {
        store: Arc<LocalStore>,
        account_id: i64,
        inbox_id: i64,
        archive_id: i64,
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
        let thread = |remote_thread_id: &str, subject: &str, messages: Vec<MessageSnapshot>| {
            ThreadSnapshot {
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
            }
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
        let threads = store.list_threads(inbox_id, 0..2).expect("threads load");
        Fixture {
            store,
            account_id,
            inbox_id,
            archive_id,
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
    }

    /// The service plus a log of every event it emits.
    fn service(
        fixture: &Fixture,
        cx: &mut TestAppContext,
    ) -> (Entity<MailActions>, Rc<RefCell<Vec<MailEvent>>>) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let store = fixture.store.clone();
        let actions = cx.new(|_| MailActions::new(store));
        let log = events.clone();
        cx.update(|cx| {
            cx.subscribe(&actions, move |_, event: &MailEvent, _| {
                log.borrow_mut().push(event.clone());
            })
            .detach();
        });
        (actions, events)
    }

    /// The labels of the recorded entries in `events`.
    fn recorded(events: &RefCell<Vec<MailEvent>>) -> Vec<(String, bool)> {
        events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                MailEvent::UndoRecorded {
                    label, can_undo, ..
                } => Some((label.clone(), *can_undo)),
                _ => None,
            })
            .collect()
    }

    #[gpui::test]
    fn every_thread_change_and_its_undo_reach_subscribers(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);
        let (thread_id, other_id) = (fixture.thread_id, fixture.other_id);
        let archive_id = fixture.archive_id;

        actions
            .update(cx, |actions, cx| {
                actions.archive_threads(&[thread_id], cx)?;
                actions.move_threads(&[thread_id], fixture.inbox_id, cx)?;
                actions.set_threads_flagged(&[thread_id], true, cx)?;
                actions.set_threads_read(&[thread_id], true, cx)?;
                actions.delete_threads(&[other_id], cx)?;
                actions.mark_opened_thread_read(other_id, cx)
            })
            .expect("the changes work");
        assert_eq!(
            recorded(&events),
            [
                ("Conversation archived".to_owned(), true),
                ("Conversation moved to Inbox".to_owned(), true),
                ("Conversation flagged".to_owned(), true),
                ("Marked as read".to_owned(), true),
                ("Conversation moved to Trash".to_owned(), true),
            ]
        );
        assert_eq!(
            events.borrow().first(),
            Some(&MailEvent::ThreadMoved {
                thread_id,
                from_mailbox_id: fixture.inbox_id,
                to_mailbox_id: archive_id,
            })
        );
        assert_eq!(
            events.borrow().last(),
            Some(&MailEvent::ThreadReadChanged {
                thread_id: other_id,
                read: true,
            })
        );
        actions.read_with(cx, |actions, _| assert_eq!(actions.undo_entries().len(), 5));

        events.borrow_mut().clear();
        let undone = actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect("undo works")
            .map(|entry| entry.label);
        assert_eq!(undone.as_deref(), Some("Conversation moved to Trash"));
        assert_eq!(fixture.mailbox_of(other_id), Some(fixture.inbox_id));
        assert!(matches!(
            events.borrow().last(),
            Some(MailEvent::Undone { label, .. }) if label == "Conversation moved to Trash"
        ));
    }

    #[gpui::test]
    fn a_toast_undoes_only_its_own_change(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, _events) = service(&fixture, cx);

        actions
            .update(cx, |actions, cx| {
                actions.archive_threads(&[fixture.thread_id], cx)
            })
            .expect("archive works");
        let archived = actions.read_with(cx, |actions, _| {
            actions.last_undo().expect("the archive is recorded").id
        });
        actions
            .update(cx, |actions, cx| {
                actions.set_threads_flagged(&[fixture.other_id], true, cx)
            })
            .expect("flag works");

        // The archive's toast is stale once the flag is the latest change.
        let undone = actions
            .update(cx, |actions, cx| actions.undo_entry(archived, cx))
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

    #[gpui::test]
    fn draft_changes_write_the_store_and_emit(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);
        let identity_id = fixture
            .store
            .list_identities(fixture.account_id)
            .expect("identities load")[0]
            .id;

        let draft_id = actions
            .update(cx, |actions, cx| {
                actions.create_draft(
                    &NewDraft {
                        account_id: fixture.account_id,
                        identity_id,
                        to: Vec::new(),
                        cc: Vec::new(),
                        bcc: Vec::new(),
                        subject: String::new(),
                        body: String::new(),
                    },
                    cx,
                )
            })
            .expect("draft is created");
        actions
            .update(cx, |actions, cx| {
                actions.save_draft(
                    draft_id,
                    &DraftUpdate {
                        identity_id,
                        to: vec!["kai@example.com".to_owned()],
                        cc: Vec::new(),
                        bcc: Vec::new(),
                        subject: "Hello".to_owned(),
                        body: "Hi Kai".to_owned(),
                    },
                    cx,
                )
            })
            .expect("draft saves");
        let draft = fixture
            .store
            .draft_detail(draft_id)
            .expect("draft loads")
            .expect("draft exists");
        assert_eq!(draft.subject, "Hello");

        let path = std::env::temp_dir().join(format!("zmail-ui-attachment-{draft_id}.txt"));
        std::fs::write(&path, b"attachment").expect("fixture file writes");
        let attachment = actions
            .update(cx, |actions, cx| {
                actions.add_draft_attachment(draft_id, &path, cx)
            })
            .expect("attachment is added");
        actions
            .update(cx, |actions, cx| {
                actions.remove_draft_attachment(draft_id, attachment.id, cx)
            })
            .expect("attachment is removed");
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            events.borrow().as_slice(),
            &[
                MailEvent::DraftCreated {
                    draft_id,
                    account_id: fixture.account_id,
                },
                MailEvent::DraftSaved { draft_id },
                MailEvent::DraftAttachmentsChanged { draft_id },
                MailEvent::DraftAttachmentsChanged { draft_id },
            ]
        );
    }
}
