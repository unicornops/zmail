//! Zmail's mail actions: the one place a frontend changes mail.
//!
//! Every mailbox and draft mutation the GUI's panes and the terminal UI's
//! screens make goes through [`MailActions`] (ADR 0007, section 8). Each
//! method:
//!
//! 1. writes the local store (which also queues the server write-back, so
//!    the change is visible at once and syncs later: the local-first rule),
//! 2. reports a [`MailEvent`] describing what changed to the callback it is
//!    given, so the frontend refreshes only from local state, and
//! 3. for reversible thread changes, records an [`UndoEntry`] holding the
//!    inverse operations.
//!
//! Thread changes take a list of threads, so a multi-row selection is one
//! change with one undo entry ("3 conversations archived"). The inverses
//! go through the same store calls, and the store folds them into the
//! write-back still queued, so undoing a change before it syncs leaves the
//! server nothing to do.
//!
//! Because every change is a store write with a queued write-back, it works
//! the same whichever process holds the store's sync lock: the holder sends
//! the queued changes at its next sync.
//!
//! The service knows nothing of a UI toolkit. The GUI wraps it in a GPUI
//! entity whose callback is `cx.emit`; the terminal UI keeps one for the
//! session and shows the recorded label on its status line.
//!
//! [`reply`] works out what a reply or forward draft starts with, for both
//! frontends.

use std::{collections::VecDeque, path::Path, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail};
use zmail_core::{
    DraftAttachment, DraftUpdate, MessageFlagState, NewDraft, ThreadState, text::count_noun,
};
use zmail_store::LocalStore;

pub mod reply;

/// How many undo entries are kept; older ones are dropped.
pub const UNDO_LIMIT: usize = 50;

/// How long a conversation stays open before it is marked read, so
/// skimming past a message does not mark it read. The GUI's reader and the
/// terminal's Message Text both wait this long.
pub const MARK_READ_DELAY: Duration = Duration::from_millis(1500);

/// How long typing must pause before an open draft autosaves. The GUI's
/// compose and the terminal's both wait this long.
pub const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);

/// Identifies one recorded change, so the undo offer for it (the GUI's
/// toast) can undo exactly that change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UndoId(u64);

/// What changed in the local store. Frontends refresh from the store when
/// they hear of it; nothing here carries remote state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailEvent {
    /// A thread moved between two mailboxes of one account: archive, move,
    /// delete into the trash, or the undo of one of those.
    ThreadMoved {
        /// The thread.
        thread_id: i64,
        /// The mailbox it left.
        from_mailbox_id: i64,
        /// The mailbox it is in now.
        to_mailbox_id: i64,
    },
    /// A thread was removed from the local store (deleted where the account
    /// has no trash mailbox, or deleted from the trash itself).
    ThreadDestroyed {
        /// The thread.
        thread_id: i64,
        /// The mailbox it was in.
        mailbox_id: i64,
    },
    /// A thread's flag changed.
    ThreadFlagChanged {
        /// The thread.
        thread_id: i64,
        /// Whether it is flagged now.
        flagged: bool,
    },
    /// A thread's read state changed.
    ThreadReadChanged {
        /// The thread.
        thread_id: i64,
        /// Whether it is read now.
        read: bool,
    },
    /// A new draft was created.
    DraftCreated {
        /// The draft.
        draft_id: i64,
        /// The account it belongs to.
        account_id: i64,
    },
    /// A draft's fields were saved.
    DraftSaved {
        /// The draft.
        draft_id: i64,
    },
    /// An attachment was added to or removed from a draft.
    DraftAttachmentsChanged {
        /// The draft.
        draft_id: i64,
    },
    /// A draft was deleted (the terminal's Cancel).
    DraftDeleted {
        /// The draft.
        draft_id: i64,
    },
    /// An outbox job was requeued for another delivery attempt.
    OutboxJobRetried {
        /// The job.
        job_id: i64,
    },
    /// A queued message was taken back before it was sent (Undo Send);
    /// its draft can be edited again.
    SendCancelled {
        /// The draft.
        draft_id: i64,
    },
    /// A change was recorded; the label describes it, for example
    /// "3 conversations archived". `can_undo` is false for a change that
    /// cannot be reversed (a permanent delete).
    UndoRecorded {
        /// The change.
        id: UndoId,
        /// What it did.
        label: String,
        /// Whether it can be undone.
        can_undo: bool,
    },
    /// A recorded change was undone.
    Undone {
        /// The change.
        id: UndoId,
        /// What it had done.
        label: String,
    },
}

/// An operation that reverses part of a recorded change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InverseOp {
    /// Move the thread back to the mailbox it came from.
    MoveThread {
        /// The thread.
        thread_id: i64,
        /// The mailbox it came from.
        to_mailbox_id: i64,
    },
    /// Put each message's read and flag state back.
    RestoreFlags {
        /// The thread.
        thread_id: i64,
        /// Each message's state before the change.
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
pub struct UndoEntry {
    /// The change.
    pub id: UndoId,
    /// Short description for the undo offer, for example
    /// "Conversation archived".
    pub label: String,
    /// How to reverse the change, one operation per thread. Empty when it
    /// cannot be reversed (threads deleted without a trash mailbox are
    /// gone locally). Such an entry still sits on the stack so Undo never
    /// silently reverses an older change instead.
    pub inverses: Vec<InverseOp>,
}

impl UndoEntry {
    /// Every thread the change touched, in the order it was made.
    pub fn thread_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.inverses.iter().map(InverseOp::thread_id)
    }

    /// The threads the change moved back into `mailbox_id` when undone.
    pub fn restores_into(&self, mailbox_id: i64) -> impl Iterator<Item = i64> + '_ {
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

/// The mail mutation service. One instance lives as long as the frontend's
/// window or session, so Undo reaches changes made from any screen.
///
/// Each change takes `events`, a callback that hears every [`MailEvent`]
/// the change makes, in order.
pub struct MailActions {
    store: Arc<LocalStore>,
    undo_stack: VecDeque<UndoEntry>,
    next_undo_id: u64,
}

impl MailActions {
    /// Creates the service over the local store.
    pub fn new(store: Arc<LocalStore>) -> Self {
        Self {
            store,
            undo_stack: VecDeque::new(),
            next_undo_id: 0,
        }
    }

    /// The recorded undo entries, oldest first.
    pub fn undo_entries(&self) -> impl ExactSizeIterator<Item = &UndoEntry> {
        self.undo_stack.iter()
    }

    /// The entry [`Self::undo`] would reverse next.
    pub fn last_undo(&self) -> Option<&UndoEntry> {
        self.undo_stack.back()
    }

    // -----------------------------------------------------------------------
    // Thread changes (reversible)
    // -----------------------------------------------------------------------

    /// Moves the threads to their account's archive mailbox.
    pub fn archive_threads(
        &mut self,
        thread_ids: &[i64],
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        let mut change = Change::default();
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            self.store.archive_thread(thread_id)?;
            let to_mailbox_id = self.current_mailbox_id(thread_id)?;
            change.moved(&before, to_mailbox_id, events);
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
            events,
        );
        result
    }

    /// Moves the threads to another mailbox of the same account.
    pub fn move_threads(
        &mut self,
        thread_ids: &[i64],
        to_mailbox_id: i64,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        let mut change = Change::default();
        let mut destination = None;
        // A folder belongs to one account; threads of other accounts in a
        // cross-account selection stay where they are.
        let destination_account = self
            .store
            .list_all_mailboxes()?
            .into_iter()
            .find(|mailbox| mailbox.id == to_mailbox_id)
            .map(|mailbox| mailbox.account_id);
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            if before.mailbox_id == to_mailbox_id || Some(before.account_id) != destination_account
            {
                return Ok(());
            }
            self.store.move_thread(thread_id, to_mailbox_id)?;
            if destination.is_none() {
                destination = self.thread(thread_id)?.map(|after| after.mailbox_name);
            }
            change.moved(&before, to_mailbox_id, events);
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
            events,
        );
        result
    }

    /// Deletes the threads: into the trash mailbox when the account has
    /// one, otherwise (or from the trash itself) for good. Only the first
    /// kind can be undone.
    pub fn delete_threads(
        &mut self,
        thread_ids: &[i64],
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        let mut change = Change::default();
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            self.store.delete_thread(thread_id)?;
            match self.thread(thread_id)? {
                Some(after) if after.mailbox_id != before.mailbox_id => {
                    change.moved(&before, after.mailbox_id, events);
                }
                _ => {
                    change.destroyed += 1;
                    events(MailEvent::ThreadDestroyed {
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
        self.record(change, label, events);
        result
    }

    /// Flags or unflags the threads.
    pub fn set_threads_flagged(
        &mut self,
        thread_ids: &[i64],
        flagged: bool,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        let mut change = Change::default();
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            self.store.set_thread_flagged(thread_id, flagged)?;
            change.flags_changed(before);
            events(MailEvent::ThreadFlagChanged { thread_id, flagged });
            Ok(())
        });
        let count = change.inverses.len();
        let label = match (count, flagged) {
            (1, true) => "Conversation flagged".to_owned(),
            (_, true) => format!("{} flagged", conversations(count)),
            (1, false) => "Flag removed".to_owned(),
            (_, false) => format!("Flags removed from {}", conversations(count)),
        };
        self.record(change, label, events);
        result
    }

    /// Marks the threads read or unread because the user asked to.
    pub fn set_threads_read(
        &mut self,
        thread_ids: &[i64],
        read: bool,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        let mut change = Change::default();
        let result = thread_ids.iter().try_for_each(|&thread_id| {
            let Some(before) = self.thread(thread_id)? else {
                return Ok(());
            };
            self.store.set_thread_read(thread_id, read)?;
            change.flags_changed(before);
            events(MailEvent::ThreadReadChanged { thread_id, read });
            Ok(())
        });
        let count = change.inverses.len();
        let state = if read { "read" } else { "unread" };
        let label = if count == 1 {
            format!("Marked as {state}")
        } else {
            format!("{} marked as {state}", conversations(count))
        };
        self.record(change, label, events);
        result
    }

    /// Marks a thread read because it stayed open for
    /// [`MARK_READ_DELAY`]. Opening a message is not something to undo, so
    /// no entry is recorded.
    pub fn mark_opened_thread_read(
        &mut self,
        thread_id: i64,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        self.store.set_thread_read(thread_id, true)?;
        events(MailEvent::ThreadReadChanged {
            thread_id,
            read: true,
        });
        Ok(())
    }

    /// Reverses the most recent recorded change and returns it, or `None`
    /// when there is nothing to undo.
    pub fn undo(&mut self, events: &mut impl FnMut(MailEvent)) -> Result<Option<UndoEntry>> {
        let Some(entry) = self.undo_stack.pop_back() else {
            return Ok(None);
        };
        self.apply_undo(entry, events).map(Some)
    }

    /// Reverses the change `id` if it is still the most recent one, as the
    /// Undo button on its toast does. Returns `None` when it is not (it was
    /// undone already, or a newer change came after it).
    pub fn undo_entry(
        &mut self,
        id: UndoId,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<Option<UndoEntry>> {
        if self.last_undo().is_none_or(|entry| entry.id != id) {
            return Ok(None);
        }
        self.undo(events)
    }

    fn apply_undo(
        &mut self,
        entry: UndoEntry,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<UndoEntry> {
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
                    events(MailEvent::ThreadMoved {
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
                        events(MailEvent::ThreadFlagChanged {
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
                        events(MailEvent::ThreadReadChanged {
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
        events(MailEvent::Undone {
            id: entry.id,
            label: entry.label.clone(),
        });
        Ok(entry)
    }

    // -----------------------------------------------------------------------
    // Drafts and the outbox
    // -----------------------------------------------------------------------

    /// Creates a draft and returns its id.
    pub fn create_draft(
        &mut self,
        draft: &NewDraft,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<i64> {
        let draft_id = self.store.create_draft(draft)?;
        events(MailEvent::DraftCreated {
            draft_id,
            account_id: draft.account_id,
        });
        Ok(draft_id)
    }

    /// Saves a draft's fields.
    pub fn save_draft(
        &mut self,
        draft_id: i64,
        update: &DraftUpdate,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        self.store.update_draft(draft_id, update)?;
        events(MailEvent::DraftSaved { draft_id });
        Ok(())
    }

    /// Copies the file at `path` into the draft as an attachment.
    pub fn add_draft_attachment(
        &mut self,
        draft_id: i64,
        path: impl AsRef<Path>,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<DraftAttachment> {
        let attachment = self.store.add_draft_attachment(draft_id, path)?;
        events(MailEvent::DraftAttachmentsChanged { draft_id });
        Ok(attachment)
    }

    /// Removes an attachment from a draft.
    pub fn remove_draft_attachment(
        &mut self,
        draft_id: i64,
        attachment_id: i64,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        self.store.remove_draft_attachment(attachment_id)?;
        events(MailEvent::DraftAttachmentsChanged { draft_id });
        Ok(())
    }

    /// Deletes a draft the user gave up on, with its attachments. Fails
    /// for a draft that is queued, sending or sent.
    pub fn delete_draft(
        &mut self,
        draft_id: i64,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        self.store.delete_draft(draft_id)?;
        events(MailEvent::DraftDeleted { draft_id });
        Ok(())
    }

    /// Takes a queued message back before it is sent (Undo Send). Returns
    /// `false` when it is too late: delivery has started.
    pub fn cancel_send(
        &mut self,
        job_id: i64,
        draft_id: i64,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<bool> {
        let cancelled = self.store.cancel_queued_outbox_job(job_id)?;
        if cancelled {
            events(MailEvent::SendCancelled { draft_id });
        }
        Ok(cancelled)
    }

    /// Requeues a failed outbox job. The caller asks the sync service to
    /// process the outbox afterwards.
    pub fn retry_outbox_job(
        &mut self,
        job_id: i64,
        events: &mut impl FnMut(MailEvent),
    ) -> Result<()> {
        self.store.retry_outbox_job(job_id)?;
        events(MailEvent::OutboxJobRetried { job_id });
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
    fn record(&mut self, change: Change, label: String, events: &mut impl FnMut(MailEvent)) {
        if change.inverses.is_empty() && change.destroyed == 0 {
            return;
        }
        if self.undo_stack.len() == UNDO_LIMIT {
            self.undo_stack.pop_front();
        }
        let id = UndoId(self.next_undo_id);
        self.next_undo_id += 1;
        events(MailEvent::UndoRecorded {
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
    fn moved(
        &mut self,
        before: &ThreadState,
        to_mailbox_id: i64,
        events: &mut impl FnMut(MailEvent),
    ) {
        self.inverses.push(InverseOp::MoveThread {
            thread_id: before.id,
            to_mailbox_id: before.mailbox_id,
        });
        events(MailEvent::ThreadMoved {
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
mod tests;
