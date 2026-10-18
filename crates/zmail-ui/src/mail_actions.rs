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
//!    inverse operations.
//!
//! Thread changes take a list of threads, so a multi-row selection is one
//! change with one undo entry ("3 conversations archived"). The inverses
//! go through the same store calls, and the store folds them into the
//! write-back still queued, so undoing a change before it syncs leaves the
//! server nothing to do.

use std::{collections::VecDeque, path::Path, sync::Arc};

use anyhow::{Context as _, Result, bail};
use gpui::{Context, EventEmitter};
use zmail_core::{
    DraftAttachment, DraftUpdate, MessageFlagState, NewDraft, ThreadState, text::count_noun,
};
use zmail_store::LocalStore;

/// How many undo entries are kept; older ones are dropped.
pub(crate) const UNDO_LIMIT: usize = 50;

/// Identifies one recorded change, so the undo toast for it can undo
/// exactly that change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct UndoId(u64);

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
    /// A change was recorded; the label describes it, for example
    /// "3 conversations archived". `can_undo` is false for a change that
    /// cannot be reversed (a permanent delete).
    UndoRecorded {
        id: UndoId,
        label: String,
        can_undo: bool,
    },
    /// A recorded change was undone.
    Undone { id: UndoId, label: String },
}

/// An operation that reverses part of a recorded change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InverseOp {
    /// Move the thread back to the mailbox it came from.
    MoveThread { thread_id: i64, to_mailbox_id: i64 },
    /// Put each message's read and flag state back.
    RestoreFlags {
        thread_id: i64,
        messages: Vec<MessageFlagState>,
    },
}

impl InverseOp {
    fn thread_id(&self) -> i64 {
        match self {
            Self::MoveThread { thread_id, .. } | Self::RestoreFlags { thread_id, .. } => *thread_id,
        }
    }
}

/// One change the user can undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UndoEntry {
    pub(crate) id: UndoId,
    /// Short description for the undo toast, for example
    /// "Conversation archived".
    pub(crate) label: String,
    /// How to reverse the change, one operation per thread. Empty when it
    /// cannot be reversed (threads deleted without a trash mailbox are
    /// gone locally). Such an entry still sits on the stack so Undo never
    /// silently reverses an older change instead.
    pub(crate) inverses: Vec<InverseOp>,
}

impl UndoEntry {
    /// Every thread the change touched, in the order it was made.
    pub(crate) fn thread_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.inverses.iter().map(InverseOp::thread_id)
    }

    /// The threads the change moved back into `mailbox_id` when undone.
    pub(crate) fn restores_into(&self, mailbox_id: i64) -> impl Iterator<Item = i64> + '_ {
        self.inverses
            .iter()
            .filter_map(move |inverse| match inverse {
                InverseOp::MoveThread {
                    thread_id,
                    to_mailbox_id,
                } if *to_mailbox_id == mailbox_id => Some(*thread_id),
                _ => None,
            })
    }
}

/// The mail mutation service. One instance lives for the window, owned by
/// the workspace shell and shared with the panes as an `Entity`.
pub(crate) struct MailActions {
    store: Arc<LocalStore>,
    undo_stack: VecDeque<UndoEntry>,
    next_undo_id: u64,
}

impl EventEmitter<MailEvent> for MailActions {}

impl MailActions {
    /// Creates the service over the local store.
    pub(crate) fn new(store: Arc<LocalStore>) -> Self {
        Self {
            store,
            undo_stack: VecDeque::new(),
            next_undo_id: 0,
        }
    }

    /// The recorded undo entries, oldest first.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn undo_entries(&self) -> impl ExactSizeIterator<Item = &UndoEntry> {
        self.undo_stack.iter()
    }

    /// The entry [`Self::undo`] would reverse next.
    pub(crate) fn last_undo(&self) -> Option<&UndoEntry> {
        self.undo_stack.back()
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
        let mut change = Change::default();
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            self.store.archive_thread(thread_id)?;
            let to_mailbox_id = self.current_mailbox_id(thread_id)?;
            change.moved(&before, to_mailbox_id, cx);
            Ok(())
        });
        let count = change.inverses.len();
        self.record(
            change,
            if count == 1 {
                "Conversation archived".to_owned()
            } else {
                format!("{} archived", conversations(count))
            },
            cx,
        );
        result
    }

    /// Moves the threads to another mailbox of the same account.
    pub(crate) fn move_threads(
        &mut self,
        thread_ids: &[i64],
        to_mailbox_id: i64,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let mut change = Change::default();
        let mut destination = None;
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            if before.mailbox_id == to_mailbox_id {
                return Ok(());
            }
            self.store.move_thread(thread_id, to_mailbox_id)?;
            if destination.is_none() {
                destination = self.thread(thread_id)?.map(|after| after.mailbox_name);
            }
            change.moved(&before, to_mailbox_id, cx);
            Ok(())
        });
        let count = change.inverses.len();
        let destination = destination.unwrap_or_else(|| "another folder".to_owned());
        self.record(
            change,
            if count == 1 {
                format!("Conversation moved to {destination}")
            } else {
                format!("{} moved to {destination}", conversations(count))
            },
            cx,
        );
        result
    }

    /// Deletes the threads: into the trash mailbox when the account has
    /// one, otherwise (or from the trash itself) for good. Only the first
    /// kind can be undone.
    pub(crate) fn delete_threads(
        &mut self,
        thread_ids: &[i64],
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let mut change = Change::default();
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            self.store.delete_thread(thread_id)?;
            match self.thread(thread_id)? {
                Some(after) if after.mailbox_id != before.mailbox_id => {
                    change.moved(&before, after.mailbox_id, cx);
                }
                _ => {
                    change.destroyed += 1;
                    cx.emit(MailEvent::ThreadDestroyed {
                        thread_id,
                        mailbox_id: before.mailbox_id,
                    });
                }
            }
            Ok(())
        });
        let count = change.inverses.len() + change.destroyed;
        let label = match (count, change.destroyed) {
            (1, 0) => "Conversation moved to Trash".to_owned(),
            (_, 0) => format!("{} moved to Trash", conversations(count)),
            (1, _) => "Conversation deleted".to_owned(),
            _ => format!("{} deleted", conversations(count)),
        };
        self.record(change, label, cx);
        result
    }

    /// Flags or unflags the threads.
    pub(crate) fn set_threads_flagged(
        &mut self,
        thread_ids: &[i64],
        flagged: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let mut change = Change::default();
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            self.store.set_thread_flagged(thread_id, flagged)?;
            change.flags_changed(before);
            cx.emit(MailEvent::ThreadFlagChanged { thread_id, flagged });
            Ok(())
        });
        let count = change.inverses.len();
        let label = match (count, flagged) {
            (1, true) => "Conversation flagged".to_owned(),
            (_, true) => format!("{} flagged", conversations(count)),
            (1, false) => "Flag removed".to_owned(),
            (_, false) => format!("Flags removed from {}", conversations(count)),
        };
        self.record(change, label, cx);
        result
    }

    /// Marks the threads read or unread because the user asked to.
    pub(crate) fn set_threads_read(
        &mut self,
        thread_ids: &[i64],
        read: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let mut change = Change::default();
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            self.store.set_thread_read(thread_id, read)?;
            change.flags_changed(before);
            cx.emit(MailEvent::ThreadReadChanged { thread_id, read });
            Ok(())
        });
        let count = change.inverses.len();
        let state = if read { "read" } else { "unread" };
        let label = if count == 1 {
            format!("Marked as {state}")
        } else {
            format!("{} marked as {state}", conversations(count))
        };
        self.record(change, label, cx);
        result
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

    /// Reverses the most recent recorded change and returns it, or `None`
    /// when there is nothing to undo.
    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) -> Result<Option<UndoEntry>> {
        let Some(entry) = self.undo_stack.pop_back() else {
            return Ok(None);
        };
        self.apply_undo(entry, cx).map(Some)
    }

    /// Reverses the change `id` if it is still the most recent one, as the
    /// Undo button on its toast does. Returns `None` when it is not (it was
    /// undone already, or a newer change came after it).
    pub(crate) fn undo_entry(
        &mut self,
        id: UndoId,
        cx: &mut Context<Self>,
    ) -> Result<Option<UndoEntry>> {
        if self.last_undo().is_none_or(|entry| entry.id != id) {
            return Ok(None);
        }
        self.undo(cx)
    }

    fn apply_undo(&mut self, entry: UndoEntry, cx: &mut Context<Self>) -> Result<UndoEntry> {
        if entry.inverses.is_empty() {
            bail!("{} can't be undone.", entry.label);
        }
        // Undo what can still be undone; a thread that has gone since
        // (deleted on another device and synced) is skipped.
        let mut missing = 0;
        for inverse in &entry.inverses {
            let Some(current) = self.thread(inverse.thread_id())? else {
                missing += 1;
                continue;
            };
            match inverse {
                &InverseOp::MoveThread {
                    thread_id,
                    to_mailbox_id,
                } => {
                    if current.mailbox_id == to_mailbox_id {
                        continue;
                    }
                    self.store.move_thread(thread_id, to_mailbox_id)?;
                    cx.emit(MailEvent::ThreadMoved {
                        thread_id,
                        from_mailbox_id: current.mailbox_id,
                        to_mailbox_id,
                    });
                }
                InverseOp::RestoreFlags {
                    thread_id,
                    messages,
                } => {
                    self.store
                        .restore_message_flag_states(*thread_id, messages)?;
                    let restored = self
                        .thread(*thread_id)?
                        .context("the conversation disappeared while undoing")?;
                    if restored.is_flagged() != current.is_flagged() {
                        cx.emit(MailEvent::ThreadFlagChanged {
                            thread_id: *thread_id,
                            flagged: restored.is_flagged(),
                        });
                    }
                    let read_states = |thread: &ThreadState| {
                        thread
                            .messages
                            .iter()
                            .map(|message| message.is_read)
                            .collect::<Vec<_>>()
                    };
                    if read_states(&restored) != read_states(&current) {
                        cx.emit(MailEvent::ThreadReadChanged {
                            thread_id: *thread_id,
                            read: restored.is_read(),
                        });
                    }
                }
            }
        }
        if missing == entry.inverses.len() {
            bail!(
                "{} can't be undone: the mail is no longer here.",
                entry.label
            );
        }
        cx.emit(MailEvent::Undone {
            id: entry.id,
            label: entry.label.clone(),
        });
        Ok(entry)
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

    fn thread(&self, thread_id: i64) -> Result<Option<ThreadState>> {
        self.store.thread_state(thread_id)
    }

    fn current_mailbox_id(&self, thread_id: i64) -> Result<i64> {
        Ok(self
            .thread(thread_id)?
            .context("the conversation disappeared while moving it")?
            .mailbox_id)
    }

    /// Records `change` under `label` if it touched any thread.
    fn record(&mut self, change: Change, label: String, cx: &mut Context<Self>) {
        if change.inverses.is_empty() && change.destroyed == 0 {
            return;
        }
        if self.undo_stack.len() == UNDO_LIMIT {
            self.undo_stack.pop_front();
        }
        let id = UndoId(self.next_undo_id);
        self.next_undo_id += 1;
        cx.emit(MailEvent::UndoRecorded {
            id,
            label: label.clone(),
            can_undo: !change.inverses.is_empty(),
        });
        self.undo_stack.push_back(UndoEntry {
            id,
            label,
            inverses: change.inverses,
        });
    }
}

/// The inverses collected while a change runs over its threads.
#[derive(Default)]
struct Change {
    inverses: Vec<InverseOp>,
    /// Threads deleted for good, which have no inverse.
    destroyed: usize,
}

impl Change {
    fn moved(&mut self, before: &ThreadState, to_mailbox_id: i64, cx: &mut Context<MailActions>) {
        self.inverses.push(InverseOp::MoveThread {
            thread_id: before.id,
            to_mailbox_id: before.mailbox_id,
        });
        cx.emit(MailEvent::ThreadMoved {
            thread_id: before.id,
            from_mailbox_id: before.mailbox_id,
            to_mailbox_id,
        });
    }

    fn flags_changed(&mut self, before: ThreadState) {
        self.inverses.push(InverseOp::RestoreFlags {
            thread_id: before.id,
            messages: before.messages,
        });
    }
}

/// "3 conversations".
fn conversations(count: usize) -> String {
    count_noun(count, "conversation", "conversations")
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    use gpui::{AppContext as _, Entity, TestAppContext};
    use zmail_core::{
        AccountSyncSnapshot, DraftUpdate, MailboxSnapshot, MessageSnapshot, NewDraft,
        PendingOpKind, ProtocolKind, ThreadSnapshot,
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

    fn undo(actions: &Entity<MailActions>, cx: &mut TestAppContext) -> Option<String> {
        actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect("undo works")
            .map(|entry| entry.label)
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
    fn archive_moves_the_thread_emits_and_undoes(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);

        actions
            .update(cx, |actions, cx| {
                actions.archive_threads(&[fixture.thread_id], cx)
            })
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
        let id = actions.read_with(cx, |actions, _| {
            let entry = actions.last_undo().expect("an entry is recorded");
            assert_eq!(
                entry.inverses,
                [InverseOp::MoveThread {
                    thread_id: fixture.thread_id,
                    to_mailbox_id: fixture.inbox_id,
                }]
            );
            entry.id
        });
        assert_eq!(
            events.borrow().as_slice(),
            &[
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

        events.borrow_mut().clear();
        assert_eq!(undo(&actions, cx).as_deref(), Some("Conversation archived"));
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
            events.borrow().as_slice(),
            &[
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
        actions.read_with(cx, |actions, _| assert_eq!(actions.undo_entries().len(), 0));
        assert_eq!(undo(&actions, cx), None, "an empty stack is not an error");
    }

    #[gpui::test]
    fn a_batch_is_one_entry_and_one_undo(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);
        let both = [fixture.thread_id, fixture.other_id];

        actions
            .update(cx, |actions, cx| actions.archive_threads(&both, cx))
            .expect("archive works");
        actions
            .update(cx, |actions, cx| {
                actions.set_threads_flagged(&both, true, cx)
            })
            .expect("flag works");
        actions
            .update(cx, |actions, cx| actions.set_threads_read(&both, true, cx))
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
            undo(&actions, cx).as_deref(),
            Some("2 conversations marked as read")
        );
        // The partly read thread is partly read again.
        assert_eq!(
            fixture.flags_of(fixture.other_id),
            [(true, true), (false, true)]
        );
        undo(&actions, cx);
        undo(&actions, cx);
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

    #[gpui::test]
    fn move_records_the_source_mailbox(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);
        let archive_id = fixture.archive_id;

        actions
            .update(cx, |actions, cx| {
                actions.move_threads(&[fixture.thread_id], archive_id, cx)
            })
            .expect("move works");
        assert_eq!(fixture.mailbox_of(fixture.thread_id), Some(archive_id));
        assert_eq!(
            recorded(&events),
            [("Conversation moved to Archive".to_owned(), true)]
        );

        // Moving to the mailbox it is already in changes nothing.
        events.borrow_mut().clear();
        actions
            .update(cx, |actions, cx| {
                actions.move_threads(&[fixture.thread_id], archive_id, cx)
            })
            .expect("a no-op move works");
        assert!(events.borrow().is_empty());

        undo(&actions, cx);
        assert_eq!(
            fixture.mailbox_of(fixture.thread_id),
            Some(fixture.inbox_id)
        );
    }

    #[gpui::test]
    fn undoing_a_delete_before_it_syncs_leaves_no_server_delete(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, events) = service(&fixture, cx);
        let trash_id = fixture.trash_id.expect("trash exists");

        actions
            .update(cx, |actions, cx| {
                actions.delete_threads(&[fixture.thread_id, fixture.other_id], cx)
            })
            .expect("delete works");
        assert_eq!(fixture.mailbox_of(fixture.thread_id), Some(trash_id));
        assert_eq!(fixture.queued().len(), 3, "one move per message");
        assert_eq!(
            recorded(&events),
            [("2 conversations moved to Trash".to_owned(), true)]
        );

        undo(&actions, cx);
        assert_eq!(
            fixture.mailbox_of(fixture.thread_id),
            Some(fixture.inbox_id)
        );
        assert_eq!(fixture.mailbox_of(fixture.other_id), Some(fixture.inbox_id));
        assert_eq!(fixture.queued(), []);
    }

    #[gpui::test]
    fn undoing_after_the_write_back_queues_the_reverse(cx: &mut TestAppContext) {
        let fixture = fixture(true);
        let (actions, _events) = service(&fixture, cx);
        let account_id = fixture.account_id;

        actions
            .update(cx, |actions, cx| {
                actions.set_threads_flagged(&[fixture.thread_id], true, cx)
            })
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
        undo(&actions, cx);
        assert_eq!(fixture.flags_of(fixture.thread_id), [(false, false)]);
        assert_eq!(fixture.queued(), [PendingOpKind::ClearFlagged]);
    }

    #[gpui::test]
    fn permanent_delete_records_an_entry_that_cannot_be_undone(cx: &mut TestAppContext) {
        let fixture = fixture(false);
        let (actions, events) = service(&fixture, cx);

        // Flag first, so there is an older reversible entry underneath.
        actions
            .update(cx, |actions, cx| {
                actions.set_threads_flagged(&[fixture.thread_id], true, cx)
            })
            .expect("flag works");
        actions
            .update(cx, |actions, cx| {
                actions.delete_threads(&[fixture.thread_id], cx)
            })
            .expect("delete works");
        assert_eq!(
            fixture.mailbox_of(fixture.thread_id),
            None,
            "the thread is gone locally"
        );
        assert!(events.borrow().contains(&MailEvent::ThreadDestroyed {
            thread_id: fixture.thread_id,
            mailbox_id: fixture.inbox_id,
        }));
        assert_eq!(
            recorded(&events).last(),
            Some(&("Conversation deleted".to_owned(), false))
        );

        let error = actions
            .update(cx, |actions, cx| actions.undo(cx))
            .expect_err("a permanent delete cannot be undone");
        assert!(error.to_string().contains("can't be undone"), "{error}");
        // The older flag entry was not reversed in its place.
        actions.read_with(cx, |actions, _| assert_eq!(actions.undo_entries().len(), 1));
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
                    actions.set_threads_flagged(&[fixture.thread_id], index % 2 == 0, cx)
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
