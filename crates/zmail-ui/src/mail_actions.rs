//! The `MailActions` service: the one place the UI changes mail.
//!
//! Every mailbox and draft mutation the panes make goes through
//! [`MailActions`]. Each method:
//!
//! 1. writes the local store (which also queues the server write-back, so
//!    the change is visible at once and syncs later: the local-first rule),
//! 2. emits a [`MailEvent`] describing what changed, so the workspace shell
//!    and the panes refresh only from local state, and
//! 3. for reversible thread changes, records an [`UndoEntry`] holding the
//!    inverse operation.
//!
//! The undo toast and its Secondary+Z binding are Phase 3 of the UI plan;
//! the inverse operations are recorded now so that work only adds UI.
//! [`MailActions::undo`] already applies them.

use std::{collections::VecDeque, path::Path, sync::Arc};

use anyhow::{Context as _, Result, bail};
use gpui::{Context, EventEmitter};
use zmail_core::{DraftAttachment, DraftUpdate, NewDraft, ThreadView};
use zmail_store::LocalStore;

/// How many undo entries are kept; older ones are dropped.
pub(crate) const UNDO_LIMIT: usize = 50;

/// What changed in the local store. The shell subscribes and refreshes the
/// panes from the store; nothing here carries remote state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MailEvent {
    /// A thread moved between two mailboxes of one account: archive, move,
    /// delete into the trash, or the undo of one of those.
    ThreadMoved {
        thread_id: i64,
        from_mailbox_id: i64,
        to_mailbox_id: i64,
    },
    /// A thread was removed from the local store (deleted where the account
    /// has no trash mailbox, or deleted from the trash itself).
    ThreadDestroyed { thread_id: i64, mailbox_id: i64 },
    /// A thread's flag changed.
    ThreadFlagChanged { thread_id: i64, flagged: bool },
    /// A thread's read state changed.
    ThreadReadChanged { thread_id: i64, read: bool },
    /// A new draft was created.
    DraftCreated { draft_id: i64, account_id: i64 },
    /// A draft's fields were saved.
    DraftSaved { draft_id: i64 },
    /// An attachment was added to or removed from a draft.
    DraftAttachmentsChanged { draft_id: i64 },
    /// An outbox job was requeued for another delivery attempt.
    OutboxJobRetried { job_id: i64 },
    /// A reversible change was recorded; the label describes it, for
    /// example "Conversation archived".
    UndoRecorded { label: String },
    /// The most recent change was undone.
    Undone { label: String },
}

/// The operation that reverses a recorded change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InverseOp {
    /// Move the thread back to the mailbox it came from.
    MoveThread { thread_id: i64, to_mailbox_id: i64 },
    /// Restore the thread's flag.
    SetFlagged { thread_id: i64, flagged: bool },
    /// Restore the thread's read state.
    SetRead { thread_id: i64, read: bool },
}

/// One change the user can undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UndoEntry {
    /// Short description for the undo toast, for example
    /// "Conversation archived".
    pub(crate) label: String,
    /// How to reverse the change, or `None` when it cannot be reversed
    /// (a thread deleted without a trash mailbox is gone locally). Such an
    /// entry still sits on the stack so Undo never silently reverses an
    /// older change instead.
    pub(crate) inverse: Option<InverseOp>,
}

/// The mail mutation service. One instance lives for the window, owned by
/// the workspace shell and shared with the panes as an `Entity`.
pub(crate) struct MailActions {
    store: Arc<LocalStore>,
    undo_stack: VecDeque<UndoEntry>,
}

impl EventEmitter<MailEvent> for MailActions {}

impl MailActions {
    /// Creates the service over the local store.
    pub(crate) fn new(store: Arc<LocalStore>) -> Self {
        Self {
            store,
            undo_stack: VecDeque::new(),
        }
    }

    /// The recorded undo entries, oldest first. (The undo toast in Phase 3
    /// reads these.)
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn undo_entries(&self) -> impl ExactSizeIterator<Item = &UndoEntry> {
        self.undo_stack.iter()
    }

    /// The entry [`Self::undo`] would reverse next.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn last_undo(&self) -> Option<&UndoEntry> {
        self.undo_stack.back()
    }

    // -----------------------------------------------------------------------
    // Thread changes (reversible)
    // -----------------------------------------------------------------------

    /// Moves the thread to the account's archive mailbox.
    pub(crate) fn archive_thread(&mut self, thread_id: i64, cx: &mut Context<Self>) -> Result<()> {
        let Some(thread) = self.thread(thread_id)? else {
            return Ok(());
        };
        self.store.archive_thread(thread_id)?;
        let to_mailbox_id = self.current_mailbox_id(thread_id)?;
        self.finish_move(&thread, to_mailbox_id, "Conversation archived", cx);
        Ok(())
    }

    /// Moves the thread to another mailbox of the same account.
    pub(crate) fn move_thread(
        &mut self,
        thread_id: i64,
        to_mailbox_id: i64,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let Some(thread) = self.thread(thread_id)? else {
            return Ok(());
        };
        if thread.mailbox_id == to_mailbox_id {
            return Ok(());
        }
        self.store.move_thread(thread_id, to_mailbox_id)?;
        self.finish_move(&thread, to_mailbox_id, "Conversation moved", cx);
        Ok(())
    }

    /// Deletes the thread: into the trash mailbox when the account has one,
    /// otherwise (or from the trash itself) for good. Only the first kind
    /// can be undone.
    pub(crate) fn delete_thread(&mut self, thread_id: i64, cx: &mut Context<Self>) -> Result<()> {
        let Some(thread) = self.thread(thread_id)? else {
            return Ok(());
        };
        self.store.delete_thread(thread_id)?;
        match self.store.thread_view(thread_id)? {
            Some(moved) if moved.mailbox_id != thread.mailbox_id => {
                self.finish_move(&thread, moved.mailbox_id, "Conversation moved to Trash", cx);
            }
            _ => {
                self.record_undo(
                    UndoEntry {
                        label: "Conversation deleted".to_owned(),
                        inverse: None,
                    },
                    cx,
                );
                cx.emit(MailEvent::ThreadDestroyed {
                    thread_id,
                    mailbox_id: thread.mailbox_id,
                });
            }
        }
        Ok(())
    }

    /// Flags or unflags the thread.
    pub(crate) fn set_thread_flagged(
        &mut self,
        thread_id: i64,
        flagged: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let Some(thread) = self.thread(thread_id)? else {
            return Ok(());
        };
        self.store.set_thread_flagged(thread_id, flagged)?;
        self.record_undo(
            UndoEntry {
                label: if flagged {
                    "Conversation flagged"
                } else {
                    "Flag removed"
                }
                .to_owned(),
                inverse: Some(InverseOp::SetFlagged {
                    thread_id,
                    flagged: thread.is_flagged,
                }),
            },
            cx,
        );
        cx.emit(MailEvent::ThreadFlagChanged { thread_id, flagged });
        Ok(())
    }

    /// Marks the thread read or unread because the user asked to.
    pub(crate) fn set_thread_read(
        &mut self,
        thread_id: i64,
        read: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let Some(thread) = self.thread(thread_id)? else {
            return Ok(());
        };
        self.store.set_thread_read(thread_id, read)?;
        self.record_undo(
            UndoEntry {
                label: if read {
                    "Marked as read"
                } else {
                    "Marked as unread"
                }
                .to_owned(),
                inverse: Some(InverseOp::SetRead {
                    thread_id,
                    read: thread.unread_count == 0,
                }),
            },
            cx,
        );
        cx.emit(MailEvent::ThreadReadChanged { thread_id, read });
        Ok(())
    }

    /// Marks a thread read because it stayed open in the reader. Opening a
    /// message is not something to undo, so no entry is recorded.
    pub(crate) fn mark_opened_thread_read(
        &mut self,
        thread_id: i64,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.store.set_thread_read(thread_id, true)?;
        cx.emit(MailEvent::ThreadReadChanged {
            thread_id,
            read: true,
        });
        Ok(())
    }

    /// Reverses the most recent recorded change and returns its label, or
    /// `None` when there is nothing to undo.
    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) -> Result<Option<String>> {
        let Some(entry) = self.undo_stack.pop_back() else {
            return Ok(None);
        };
        let Some(inverse) = entry.inverse else {
            bail!("{} can't be undone.", entry.label);
        };
        match inverse {
            InverseOp::MoveThread {
                thread_id,
                to_mailbox_id,
            } => {
                let from_mailbox_id = self
                    .thread(thread_id)?
                    .context("the conversation no longer exists")?
                    .mailbox_id;
                self.store.move_thread(thread_id, to_mailbox_id)?;
                cx.emit(MailEvent::ThreadMoved {
                    thread_id,
                    from_mailbox_id,
                    to_mailbox_id,
                });
            }
            InverseOp::SetFlagged { thread_id, flagged } => {
                self.store.set_thread_flagged(thread_id, flagged)?;
                cx.emit(MailEvent::ThreadFlagChanged { thread_id, flagged });
            }
            InverseOp::SetRead { thread_id, read } => {
                self.store.set_thread_read(thread_id, read)?;
                cx.emit(MailEvent::ThreadReadChanged { thread_id, read });
            }
        }
        cx.emit(MailEvent::Undone {
            label: entry.label.clone(),
        });
        Ok(Some(entry.label))
    }

    // -----------------------------------------------------------------------
    // Drafts and the outbox
    // -----------------------------------------------------------------------

    /// Creates a draft and returns its id.
    pub(crate) fn create_draft(&mut self, draft: &NewDraft, cx: &mut Context<Self>) -> Result<i64> {
        let draft_id = self.store.create_draft(draft)?;
        cx.emit(MailEvent::DraftCreated {
            draft_id,
            account_id: draft.account_id,
        });
        Ok(draft_id)
    }

    /// Saves a draft's fields.
    pub(crate) fn save_draft(
        &mut self,
        draft_id: i64,
        update: &DraftUpdate,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.store.update_draft(draft_id, update)?;
        cx.emit(MailEvent::DraftSaved { draft_id });
        Ok(())
    }

    /// Copies the file at `path` into the draft as an attachment.
    pub(crate) fn add_draft_attachment(
        &mut self,
        draft_id: i64,
        path: impl AsRef<Path>,
        cx: &mut Context<Self>,
    ) -> Result<DraftAttachment> {
        let attachment = self.store.add_draft_attachment(draft_id, path)?;
        cx.emit(MailEvent::DraftAttachmentsChanged { draft_id });
        Ok(attachment)
    }

    /// Removes an attachment from a draft.
    pub(crate) fn remove_draft_attachment(
        &mut self,
        draft_id: i64,
        attachment_id: i64,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.store.remove_draft_attachment(attachment_id)?;
        cx.emit(MailEvent::DraftAttachmentsChanged { draft_id });
        Ok(())
    }

    /// Requeues a failed outbox job. The caller asks the sync service to
    /// process the outbox afterwards.
    pub(crate) fn retry_outbox_job(&mut self, job_id: i64, cx: &mut Context<Self>) -> Result<()> {
        self.store.retry_outbox_job(job_id)?;
        cx.emit(MailEvent::OutboxJobRetried { job_id });
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn thread(&self, thread_id: i64) -> Result<Option<ThreadView>> {
        self.store.thread_view(thread_id)
    }

    fn current_mailbox_id(&self, thread_id: i64) -> Result<i64> {
        Ok(self
            .thread(thread_id)?
            .context("the conversation disappeared while moving it")?
            .mailbox_id)
    }

    fn finish_move(
        &mut self,
        before: &ThreadView,
        to_mailbox_id: i64,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        self.record_undo(
            UndoEntry {
                label: label.to_owned(),
                inverse: Some(InverseOp::MoveThread {
                    thread_id: before.id,
                    to_mailbox_id: before.mailbox_id,
                }),
            },
            cx,
        );
        cx.emit(MailEvent::ThreadMoved {
            thread_id: before.id,
            from_mailbox_id: before.mailbox_id,
            to_mailbox_id,
        });
    }

    fn record_undo(&mut self, entry: UndoEntry, cx: &mut Context<Self>) {
        if self.undo_stack.len() == UNDO_LIMIT {
            self.undo_stack.pop_front();
        }
        cx.emit(MailEvent::UndoRecorded {
            label: entry.label.clone(),
        });
        self.undo_stack.push_back(entry);
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    use gpui::{AppContext as _, Entity, TestAppContext};
    use zmail_core::{
        AccountSyncSnapshot, DraftUpdate, MailboxSnapshot, MessageSnapshot, NewDraft, ProtocolKind,
        ThreadSnapshot,
    };
    use zmail_store::LocalStore;

    use super::{InverseOp, MailActions, MailEvent, UNDO_LIMIT};

    /// A temporary store with one account holding Inbox (one unread,
    /// unflagged thread), Archive and, optionally, Trash.
    struct Fixture {
        store: Arc<LocalStore>,
        account_id: i64,
        inbox_id: i64,
        archive_id: i64,
        trash_id: Option<i64>,
        thread_id: i64,
    }

    fn fixture(with_trash: bool) -> Fixture {
        let store = Arc::new(LocalStore::open_ephemeral().expect("store opens"));
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account is created");
        let mut mailboxes = vec![
            MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: "thread-1".to_owned(),
                    subject: "Quarterly report".to_owned(),
                    participants: "Kai Santos".to_owned(),
                    preview: "Numbers attached".to_owned(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 1,
                    latest_message_at: "2026-03-21T09:15:00Z".to_owned(),
                    messages: vec![MessageSnapshot {
                        remote_id: "message-1".to_owned(),
                        identity_email: "ops1@zmail.dev".to_owned(),
                        sender: "Kai Santos <kai@example.com>".to_owned(),
                        reply_to: None,
                        recipients: "ops1@zmail.dev".to_owned(),
                        subject: "Quarterly report".to_owned(),
                        body_preview: "Numbers attached".to_owned(),
                        body_text: Some("Numbers attached".to_owned()),
                        sanitized_html: None,
                        has_remote_content: false,
                        is_read: false,
                        is_flagged: false,
                        received_at: "2026-03-21T09:15:00Z".to_owned(),
                        attachments: Vec::new(),
                    }],
                }],
            },
            MailboxSnapshot {
                name: "Archive".to_owned(),
                role: Some("archive".to_owned()),
                threads: Vec::new(),
            },
        ];
        if with_trash {
            mailboxes.push(MailboxSnapshot {
                name: "Trash".to_owned(),
                role: Some("trash".to_owned()),
                threads: Vec::new(),
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
        let thread_id = store
            .first_thread_id(inbox_id)
            .expect("lookup")
            .expect("thread exists");
        Fixture {
            store,
            account_id,
            inbox_id,
            archive_id,
            trash_id,
            thread_id,
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

    fn mailbox_of(fixture: &Fixture) -> Option<i64> {
        fixture
            .store
            .thread_view(fixture.thread_id)
            .expect("thread view loads")
            .map(|thread| thread.mailbox_id)
    }

    #[gpui::test]
    fn archive_moves_the_thread_emits_and_undoes(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);

        actions
            .update(cx, |actions, cx| {
                actions.archive_thread(fixture.thread_id, cx)
            })
            .expect("archive works");
        assert_eq!(mailbox_of(&fixture), Some(fixture.archive_id));
        assert!(
            !fixture
                .store
                .list_pending_ops(fixture.account_id)
                .expect("ops load")
                .is_empty(),
            "the store queued the server write-back"
        );
        assert_eq!(
            events.borrow().as_slice(),
            &[
                MailEvent::UndoRecorded {
                    label: "Conversation archived".to_owned()
                },
                MailEvent::ThreadMoved {
                    thread_id: fixture.thread_id,
                    from_mailbox_id: fixture.inbox_id,
                    to_mailbox_id: fixture.archive_id,
                },
            ]
        );
        actions.read_with(cx, |actions, _| {
            assert_eq!(
                actions.last_undo().and_then(|entry| entry.inverse),
                Some(InverseOp::MoveThread {
                    thread_id: fixture.thread_id,
                    to_mailbox_id: fixture.inbox_id,
                })
            );
        });

        events.borrow_mut().clear();
        let undone = actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect("undo works");
        assert_eq!(undone.as_deref(), Some("Conversation archived"));
        assert_eq!(mailbox_of(&fixture), Some(fixture.inbox_id));
        assert_eq!(
            events.borrow().as_slice(),
            &[
                MailEvent::ThreadMoved {
                    thread_id: fixture.thread_id,
                    from_mailbox_id: fixture.archive_id,
                    to_mailbox_id: fixture.inbox_id,
                },
                MailEvent::Undone {
                    label: "Conversation archived".to_owned()
                },
            ]
        );
        actions.read_with(cx, |actions, _| assert_eq!(actions.undo_entries().len(), 0));
        assert_eq!(
            actions
                .update(cx, |actions, cx| actions.undo(cx))
                .expect("an empty stack is not an error"),
            None
        );
    }

    #[gpui::test]
    fn move_records_the_source_mailbox(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);
        let trash_id = fixture.trash_id.expect("trash exists");

        actions
            .update(cx, |actions, cx| {
                actions.move_thread(fixture.thread_id, trash_id, cx)
            })
            .expect("move works");
        assert_eq!(mailbox_of(&fixture), Some(trash_id));
        assert!(events.borrow().contains(&MailEvent::ThreadMoved {
            thread_id: fixture.thread_id,
            from_mailbox_id: fixture.inbox_id,
            to_mailbox_id: trash_id,
        }));

        // Moving to the mailbox it is already in changes nothing.
        events.borrow_mut().clear();
        actions
            .update(cx, |actions, cx| {
                actions.move_thread(fixture.thread_id, trash_id, cx)
            })
            .expect("a no-op move works");
        assert!(events.borrow().is_empty());

        actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect("undo works");
        assert_eq!(mailbox_of(&fixture), Some(fixture.inbox_id));
    }

    #[gpui::test]
    fn delete_to_trash_can_be_undone(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);
        let trash_id = fixture.trash_id.expect("trash exists");

        actions
            .update(cx, |actions, cx| {
                actions.delete_thread(fixture.thread_id, cx)
            })
            .expect("delete works");
        assert_eq!(mailbox_of(&fixture), Some(trash_id));
        assert!(events.borrow().contains(&MailEvent::ThreadMoved {
            thread_id: fixture.thread_id,
            from_mailbox_id: fixture.inbox_id,
            to_mailbox_id: trash_id,
        }));

        actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect("undo works");
        assert_eq!(mailbox_of(&fixture), Some(fixture.inbox_id));
    }

    #[gpui::test]
    fn permanent_delete_records_an_entry_that_cannot_be_undone(cx: &mut TestAppContext) {
        let fixture = fixture(false);
        let (actions, events) = service(&fixture, cx);

        // Flag first, so there is an older reversible entry underneath.
        actions
            .update(cx, |actions, cx| {
                actions.set_thread_flagged(fixture.thread_id, true, cx)
            })
            .expect("flag works");
        actions
            .update(cx, |actions, cx| {
                actions.delete_thread(fixture.thread_id, cx)
            })
            .expect("delete works");
        assert_eq!(mailbox_of(&fixture), None, "the thread is gone locally");
        assert!(events.borrow().contains(&MailEvent::ThreadDestroyed {
            thread_id: fixture.thread_id,
            mailbox_id: fixture.inbox_id,
        }));

        let error = actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect_err("a permanent delete cannot be undone");
        assert!(error.to_string().contains("can't be undone"), "{error}");
        // The older flag entry was not reversed in its place.
        actions.read_with(cx, |actions, _| assert_eq!(actions.undo_entries().len(), 1));
    }

    #[gpui::test]
    fn flag_and_read_record_the_previous_state(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);
        let thread = |fixture: &Fixture| {
            fixture
                .store
                .thread_view(fixture.thread_id)
                .expect("loads")
                .expect("exists")
        };

        actions
            .update(cx, |actions, cx| {
                actions.set_thread_flagged(fixture.thread_id, true, cx)
            })
            .expect("flag works");
        actions
            .update(cx, |actions, cx| {
                actions.set_thread_read(fixture.thread_id, true, cx)
            })
            .expect("mark read works");
        assert!(thread(&fixture).is_flagged);
        assert_eq!(thread(&fixture).unread_count, 0);
        assert!(events.borrow().contains(&MailEvent::ThreadFlagChanged {
            thread_id: fixture.thread_id,
            flagged: true,
        }));
        assert!(events.borrow().contains(&MailEvent::ThreadReadChanged {
            thread_id: fixture.thread_id,
            read: true,
        }));

        // Undo runs newest first: read state, then the flag.
        actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect("undo works");
        assert_eq!(thread(&fixture).unread_count, 1);
        assert!(thread(&fixture).is_flagged);
        actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect("undo works");
        assert!(!thread(&fixture).is_flagged);
    }

    #[gpui::test]
    fn opening_a_thread_marks_it_read_without_an_undo_entry(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);

        actions
            .update(cx, |actions, cx| {
                actions.mark_opened_thread_read(fixture.thread_id, cx)
            })
            .expect("mark read works");
        assert_eq!(
            events.borrow().as_slice(),
            &[MailEvent::ThreadReadChanged {
                thread_id: fixture.thread_id,
                read: true,
            }]
        );
        actions.read_with(cx, |actions, _| assert_eq!(actions.undo_entries().len(), 0));
    }

    #[gpui::test]
    fn the_undo_stack_is_capped(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, _events) = service(&fixture, cx);

        for index in 0..UNDO_LIMIT + 5 {
            actions
                .update(cx, |actions, cx| {
                    actions.set_thread_flagged(fixture.thread_id, index % 2 == 0, cx)
                })
                .expect("flag works");
        }
        actions.read_with(cx, |actions, _| {
            assert_eq!(actions.undo_entries().len(), UNDO_LIMIT);
        });
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
        assert_eq!(draft.to, vec!["kai@example.com".to_owned()]);

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
        actions.read_with(cx, |actions, _| {
            assert_eq!(
                actions.undo_entries().len(),
                0,
                "draft edits have their own undo in the text inputs"
            );
        });
    }
}
