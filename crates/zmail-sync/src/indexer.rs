//! Background body indexing.
//!
//! IMAP syncs headers only, and bodies hydrate when a message is opened, so
//! the full-text index starts out knowing only subjects, addresses and
//! previews. The indexer fetches the missing bodies in the background so
//! search finds mail that was never opened:
//!
//! - **Newest first**, from the store's queue ([`LocalStore::index_candidates`]).
//! - **Bounded by the disk budget.** After each batch the account is evicted
//!   back to its budget, and once the budget is full the queue only offers
//!   mail newer than the oldest stored body, so the stored set converges on
//!   the newest mail that fits instead of churning.
//! - **Low priority.** Small batches with a pause between them, one
//!   connection per pass, and it waits while a foreground sync of the
//!   account runs. Messages over [`INDEX_MAX_MESSAGE_BYTES`] are never
//!   downloaded in the background.
//! - **Resumable.** All progress is the stored bodies themselves, so a
//!   restart picks up where the last pass stopped.
//! - **Polite about power.** Passes pause on battery power or a metered
//!   connection unless the user allows it ([`IndexingPreferences`]), where
//!   the platform can tell (see [`crate::power`]).
//!
//! JMAP accounts are skipped: their sync already delivers bodies.
//!
//! [`IndexingPreferences`]: zmail_core::preferences::IndexingPreferences

use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

use anyhow::{Context, Result};
use tokio::time::sleep;
use zmail_core::{ProtocolKind, SyncTarget};
use zmail_store::LocalStore;

use crate::{SyncService, power::PowerConditions};

/// Messages fetched per batch.
pub const INDEX_BATCH_SIZE: usize = 25;
/// Pause between batches, so indexing never saturates the connection.
const INDEX_BATCH_PAUSE: Duration = Duration::from_millis(500);
/// How often idle accounts are re-checked when no sync wakes the indexer.
const INDEX_IDLE_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How often a paused or yielding indexer re-checks its conditions.
const INDEX_RECHECK_INTERVAL: Duration = Duration::from_secs(1);
/// Backoff after a pass fails (for example the server is unreachable).
const INDEX_FAILURE_BACKOFF: Duration = Duration::from_secs(60);
/// Largest message (`RFC822.SIZE`) downloaded in the background; larger
/// ones hydrate when opened.
pub const INDEX_MAX_MESSAGE_BYTES: u32 = 4 * 1024 * 1024;

/// Why an indexing pass stopped before the queue was empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPause {
    /// Background indexing is switched off.
    Disabled,
    /// The machine runs on battery power.
    OnBattery,
    /// The network connection is metered.
    Metered,
}

/// Outcome of one indexing pass over an account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexReport {
    /// Bodies fetched and indexed.
    pub indexed: usize,
    /// Messages skipped (too large, or the fetch failed).
    pub skipped: usize,
    /// Bytes evicted to keep the account within its disk budget.
    pub evicted_bytes: u64,
    /// Set when the pass stopped early for power or settings reasons.
    pub paused: Option<IndexPause>,
}

/// Decides whether background indexing may run under `conditions`.
///
/// Unknown conditions never pause: only a platform that positively reports
/// battery power or a metered link holds indexing back.
pub fn index_pause_reason(
    preferences: &zmail_core::preferences::IndexingPreferences,
    conditions: PowerConditions,
) -> Option<IndexPause> {
    if !preferences.enabled {
        Some(IndexPause::Disabled)
    } else if conditions.on_battery == Some(true) && !preferences.on_battery {
        Some(IndexPause::OnBattery)
    } else if conditions.metered == Some(true) && !preferences.on_metered {
        Some(IndexPause::Metered)
    } else {
        None
    }
}

impl SyncService {
    /// Starts the background indexer, once. It indexes every IMAP account,
    /// re-runs after each successful sync and otherwise every few minutes.
    pub fn start_background_indexing(&self) {
        if self.inner.indexer_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let service = self.clone();
        self.inner.handle.spawn(async move {
            loop {
                let mut failed = false;
                if let Ok(targets) = service.inner.store.list_sync_targets() {
                    for target in targets {
                        if service.index_account_pass(&target).await.is_err() {
                            failed = true;
                        }
                    }
                }
                let wait = if failed {
                    INDEX_FAILURE_BACKOFF
                } else {
                    INDEX_IDLE_INTERVAL
                };
                tokio::select! {
                    () = service.inner.index_wake.notified() => {}
                    () = sleep(wait) => {}
                }
            }
        });
    }

    /// Replaces the source of power and network conditions the indexer
    /// defers to (the default is [`crate::power::SystemPowerMonitor`]).
    pub fn set_power_monitor(&self, monitor: std::sync::Arc<dyn crate::power::PowerMonitor>) {
        *self
            .inner
            .power_monitor
            .lock()
            .expect("power monitor mutex should not be poisoned") = monitor;
    }

    /// Runs one indexing pass over an account and waits for it, for the
    /// CLI and tests. Pauses and power rules apply as in the background.
    pub fn index_account_and_wait(&self, account_id: i64) -> Result<IndexReport> {
        let service = self.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.inner.handle.spawn(async move {
            let result = match service.inner.store.sync_target(account_id) {
                Ok(Some(target)) => service.index_account_pass(&target).await,
                Ok(None) => Err(anyhow::anyhow!("account {account_id} no longer exists")),
                Err(error) => Err(error),
            };
            let _ = tx.send(result.map_err(|error| format!("{error:#}")));
        });
        rx.recv()
            .context("the indexing worker exited before returning a result")?
            .map_err(anyhow::Error::msg)
    }

    /// Whether a foreground sync of the account is running.
    pub(crate) fn is_syncing(&self, account_id: i64) -> bool {
        self.inner
            .active_accounts
            .lock()
            .expect("active account tracking mutex should not be poisoned")
            .contains(&account_id)
    }

    fn index_pause(&self) -> Option<IndexPause> {
        let preferences = self
            .inner
            .store
            .ui_preferences()
            .map(|preferences| preferences.indexing)
            .unwrap_or_default();
        let conditions = self
            .inner
            .power_monitor
            .lock()
            .expect("power monitor mutex should not be poisoned")
            .conditions();
        index_pause_reason(&preferences, conditions)
    }

    /// Indexes one account until its queue is empty or a pause condition
    /// holds. Only one pass runs per account at a time; a second caller
    /// returns an empty report.
    async fn index_account_pass(&self, target: &SyncTarget) -> Result<IndexReport> {
        let mut report = IndexReport::default();
        if target.protocol != ProtocolKind::Imap {
            return Ok(report);
        }
        if !self
            .inner
            .indexing_accounts
            .lock()
            .expect("indexing account mutex should not be poisoned")
            .insert(target.id)
        {
            return Ok(report);
        }
        let result = self.run_index_pass(target, &mut report).await;
        self.inner
            .indexing_accounts
            .lock()
            .expect("indexing account mutex should not be poisoned")
            .remove(&target.id);
        result.map(|()| report)
    }

    async fn run_index_pass(&self, target: &SyncTarget, report: &mut IndexReport) -> Result<()> {
        let store = &self.inner.store;
        let account_id = target.id;
        let mut session = None;
        let outcome = async {
            loop {
                if let Some(pause) = self.index_pause() {
                    report.paused = Some(pause);
                    return Ok(());
                }
                // Foreground sync goes first.
                while self.is_syncing(account_id) {
                    sleep(INDEX_RECHECK_INTERVAL).await;
                }
                let budget = store.account_disk_budget_bytes(account_id)?;
                let candidates = store.index_candidates(account_id, INDEX_BATCH_SIZE, budget)?;
                if candidates.is_empty() {
                    return Ok(());
                }

                if session.is_none() {
                    let secret = self.connect_secret(target).await?;
                    session = Some(zmail_imap::BodyIndexSession::connect(target, &secret).await?);
                }
                let connection = session.as_mut().expect("the session was just opened");
                let batch =
                    index_batch(store, connection, &candidates, INDEX_MAX_MESSAGE_BYTES).await?;
                report.indexed += batch.indexed.len();
                report.skipped += batch.skipped;

                if let Some(budget) = budget {
                    report.evicted_bytes += store.evict_account_blobs(account_id, budget)?;
                    // A body evicted in the batch that fetched it does not
                    // fit at all; count it as a failure so it is not
                    // fetched forever.
                    for message_id in &batch.indexed {
                        if !store.message_has_body(*message_id)? {
                            store.record_body_fetch_failure(*message_id, false)?;
                        }
                    }
                }
                sleep(INDEX_BATCH_PAUSE).await;
            }
        }
        .await;
        if let Some(session) = session {
            session.logout().await;
        }
        outcome
    }
}

/// What one batch did.
struct BatchOutcome {
    /// Message ids whose bodies were stored.
    indexed: Vec<i64>,
    /// Messages recorded as failed or skipped.
    skipped: usize,
}

/// Fetches and stores one batch of candidates, grouped by mailbox.
async fn index_batch(
    store: &LocalStore,
    session: &mut zmail_imap::BodyIndexSession,
    candidates: &[zmail_core::IndexCandidate],
    max_message_bytes: u32,
) -> Result<BatchOutcome> {
    let mut outcome = BatchOutcome {
        indexed: Vec::new(),
        skipped: 0,
    };
    let mut by_mailbox: BTreeMap<&str, Vec<(u32, i64)>> = BTreeMap::new();
    for candidate in candidates {
        match candidate.remote_id.parse::<u32>() {
            Ok(uid) => by_mailbox
                .entry(candidate.mailbox_name.as_str())
                .or_default()
                .push((uid, candidate.message_id)),
            Err(_) => {
                store.record_body_fetch_failure(candidate.message_id, true)?;
                outcome.skipped += 1;
            }
        }
    }

    for (mailbox, messages) in by_mailbox {
        let uids = messages.iter().map(|(uid, _)| *uid).collect::<Vec<_>>();
        let fetched = session.fetch(mailbox, &uids, max_message_bytes).await?;
        let mut bodies = fetched.bodies.into_iter().collect::<BTreeMap<_, _>>();
        for (uid, message_id) in messages {
            if let Some(body) = bodies.remove(&uid) {
                store.hydrate_message_body(message_id, &body)?;
                outcome.indexed.push(message_id);
            } else {
                // Oversized messages stay lazy; anything else the server
                // did not return (expunged, unparseable) is retried a few
                // times before it is skipped.
                store.record_body_fetch_failure(message_id, fetched.oversized.contains(&uid))?;
                outcome.skipped += 1;
            }
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use zmail_core::preferences::IndexingPreferences;

    use super::*;

    #[test]
    fn unknown_conditions_never_pause() {
        let defaults = IndexingPreferences::default();
        assert_eq!(
            index_pause_reason(&defaults, PowerConditions::default()),
            None
        );
        let plugged_in = PowerConditions {
            on_battery: Some(false),
            metered: Some(false),
        };
        assert_eq!(index_pause_reason(&defaults, plugged_in), None);
    }

    #[test]
    fn battery_and_metered_pause_unless_allowed() {
        let defaults = IndexingPreferences::default();
        let on_battery = PowerConditions {
            on_battery: Some(true),
            metered: None,
        };
        let metered = PowerConditions {
            on_battery: None,
            metered: Some(true),
        };
        assert_eq!(
            index_pause_reason(&defaults, on_battery),
            Some(IndexPause::OnBattery)
        );
        assert_eq!(
            index_pause_reason(&defaults, metered),
            Some(IndexPause::Metered)
        );
        let allowed = IndexingPreferences {
            enabled: true,
            on_battery: true,
            on_metered: true,
        };
        assert_eq!(index_pause_reason(&allowed, on_battery), None);
        assert_eq!(index_pause_reason(&allowed, metered), None);
        let disabled = IndexingPreferences {
            enabled: false,
            ..allowed
        };
        assert_eq!(
            index_pause_reason(&disabled, PowerConditions::default()),
            Some(IndexPause::Disabled)
        );
    }
}
