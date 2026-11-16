//! Where a write to the mirror spends its time, for the backfill
//! benchmark (#478, `benches/backfill.rs`, `docs/performance.md`).
//!
//! [`profile_merge`] runs a store call and returns how long each part of
//! [`LocalStore::merge_account_snapshot`](crate::LocalStore::merge_account_snapshot),
//! [`LocalStore::apply_mail_delta`](crate::LocalStore::apply_mail_delta) or
//! [`LocalStore::replace_account_snapshot`](crate::LocalStore::replace_account_snapshot)
//! took on the calling thread. Outside it the laps cost one thread-local
//! read each.

use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

/// The parts of a write to the mirror, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergePhase {
    /// Reading the account's identities, mailboxes and queued write-backs.
    Context,
    /// A delta's messages leaving the places they no longer have.
    Leave,
    /// Finding the stored conversations the incoming ones belong to.
    Match,
    /// Comparing and writing conversations and messages.
    Write,
    /// The changed mailboxes' thread and unread counts.
    Counters,
    /// The address history for recipient autocomplete.
    AddressHistory,
    /// The full-text index of the changed conversations.
    SearchIndex,
    /// Committing the transaction.
    Commit,
}

impl MergePhase {
    /// Every phase, in the order they run.
    pub const ALL: [Self; 8] = [
        Self::Context,
        Self::Leave,
        Self::Match,
        Self::Write,
        Self::Counters,
        Self::AddressHistory,
        Self::SearchIndex,
        Self::Commit,
    ];

    /// A short name for tables.
    pub fn name(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Leave => "leave old places",
            Self::Match => "match stored threads",
            Self::Write => "compare and write",
            Self::Counters => "mailbox counters",
            Self::AddressHistory => "address history",
            Self::SearchIndex => "search index",
            Self::Commit => "commit",
        }
    }
}

/// How long each phase of the profiled writes took.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeProfile {
    phases: [Duration; MergePhase::ALL.len()],
}

impl MergeProfile {
    /// The time spent in `phase`.
    pub fn phase(&self, phase: MergePhase) -> Duration {
        self.phases[phase as usize]
    }

    /// The time spent in every phase together.
    pub fn total(&self) -> Duration {
        self.phases.iter().sum()
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<MergeProfile>> = const { RefCell::new(None) };
}

/// Runs `write` and returns its result with the time each phase of the
/// mirror writes it made on this thread took.
pub fn profile_merge<T>(write: impl FnOnce() -> T) -> (T, MergeProfile) {
    ACTIVE.with(|active| *active.borrow_mut() = Some(MergeProfile::default()));
    let result = write();
    let profile = ACTIVE
        .with(|active| active.borrow_mut().take())
        .unwrap_or_default();
    (result, profile)
}

/// Times consecutive parts of a write: each [`Lap::mark`] books the time
/// since the one before to a phase.
pub(crate) struct Lap(Option<Instant>);

impl Lap {
    pub(crate) fn start() -> Self {
        let active = ACTIVE.with(|active| active.borrow().is_some());
        Self(active.then(Instant::now))
    }

    pub(crate) fn mark(&mut self, phase: MergePhase) {
        let Some(since) = self.0 else {
            return;
        };
        let now = Instant::now();
        ACTIVE.with(|active| {
            if let Some(profile) = active.borrow_mut().as_mut() {
                profile.phases[phase as usize] += now - since;
            }
        });
        self.0 = Some(now);
    }
}
