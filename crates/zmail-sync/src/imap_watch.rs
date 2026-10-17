//! Live updates for every mailbox of an IMAP account: one NOTIFY
//! connection where the server offers it, otherwise a bounded pool of IDLE
//! connections for the favourite mailboxes and a `STATUS` poller for the
//! rest. Every report becomes a coalesced sync through
//! [`SyncService::request_sync`].

use std::time::{Duration, Instant};

use tokio::{task::JoinSet, time::sleep};
use zmail_core::SyncTarget;
use zmail_imap::{WatchEnd, WatchEvent, WatchPlan};

use crate::{SyncService, backoff::Backoff, wake::unless_woken};

/// How long one watch plan runs before the mailbox list is read again, so
/// mailboxes created or renamed since are watched too.
const REPLAN_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// The safety resync runs this many poll intervals apart. It catches what
/// the watchers cannot see, such as flag-only changes in polled mailboxes
/// on servers without CONDSTORE.
const SAFETY_RESYNC_POLL_INTERVALS: u32 = 5;

/// One long-lived watcher connection.
#[derive(Debug, Clone)]
enum WatchTask {
    Notify,
    Idle(String),
    Poll(Vec<String>),
}

/// Why a watcher task stopped for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskEnd {
    /// The account no longer exists.
    AccountGone,
    /// The server advertised NOTIFY but refused it; replan without it.
    NotifyRefused,
}

impl SyncService {
    /// Runs one watch plan for an IMAP account until it is due for
    /// replanning, the account disappears, or NOTIFY turns out unusable.
    ///
    /// `notify_allowed` is cleared when the server refuses NOTIFY, so the
    /// next plan uses the IDLE pool.
    pub(crate) async fn watch_imap(
        &self,
        target: &SyncTarget,
        secret: &str,
        poll_interval: Duration,
        backoff: &mut Backoff,
        notify_allowed: &mut bool,
        wake: &mut tokio::sync::watch::Receiver<u64>,
    ) {
        let plan = match zmail_imap::plan_watch(
            target,
            secret,
            zmail_imap::MAX_WATCH_CONNECTIONS,
            *notify_allowed,
        )
        .await
        {
            Ok(plan) => {
                backoff.reset();
                plan
            }
            Err(_) => {
                if unless_woken(wake, sleep(backoff.next_delay()))
                    .await
                    .is_none()
                {
                    backoff.reset();
                }
                return;
            }
        };

        let mut tasks = JoinSet::new();
        let tasks_to_run = match plan {
            WatchPlan::Notify => vec![WatchTask::Notify],
            WatchPlan::Pool { idle, polled } => {
                let mut tasks = idle.into_iter().map(WatchTask::Idle).collect::<Vec<_>>();
                if !polled.is_empty() {
                    tasks.push(WatchTask::Poll(polled));
                }
                tasks
            }
        };
        for task in tasks_to_run {
            tasks.spawn(self.clone().run_watch_task(target.clone(), task));
        }

        let replan = sleep(REPLAN_INTERVAL);
        tokio::pin!(replan);
        let safety_interval = poll_interval
            .saturating_mul(SAFETY_RESYNC_POLL_INTERVALS)
            .max(Duration::from_secs(1));
        let mut safety = tokio::time::interval_at(
            tokio::time::Instant::now() + safety_interval,
            safety_interval,
        );
        loop {
            tokio::select! {
                _ = &mut replan => break,
                // The tasks reconnect on their own; a wake also replans,
                // since the mailbox list may have changed meanwhile.
                Ok(()) = wake.changed() => break,
                _ = safety.tick() => {
                    if !self.account_exists(target.id) {
                        break;
                    }
                    self.request_sync(target.id);
                }
                joined = tasks.join_next(), if !tasks.is_empty() => match joined {
                    Some(Ok(TaskEnd::NotifyRefused)) => {
                        *notify_allowed = false;
                        break;
                    }
                    Some(Ok(TaskEnd::AccountGone)) => break,
                    // A panicked or cancelled task leaves its mailboxes to
                    // the safety resync until the next plan.
                    Some(Err(_)) | None => {}
                },
            }
        }
        tasks.abort_all();
    }

    /// Keeps one watcher connected, reconnecting with backoff, and turns
    /// every report into a sync request.
    async fn run_watch_task(self, target: SyncTarget, mut task: WatchTask) -> TaskEnd {
        let account_id = target.id;
        let mut backoff = Backoff::reconnect();
        let mut wake = self.inner.wake.subscribe();
        loop {
            if !self.account_exists(account_id) {
                return TaskEnd::AccountGone;
            }
            // Credentials are resolved per connection so OAuth2 accounts
            // reconnect with a refreshed access token.
            let Ok(secret) = self.connect_secret(&target).await else {
                if unless_woken(&mut wake, sleep(backoff.next_delay()))
                    .await
                    .is_none()
                {
                    backoff.reset();
                }
                continue;
            };

            let mut connected_at = None;
            let on_event = |event: WatchEvent| {
                // Every connect resyncs: changes made while the watcher was
                // disconnected are not replayed.
                if event == WatchEvent::Connected {
                    connected_at = Some(Instant::now());
                }
                self.request_sync(account_id);
            };
            let watch = async {
                match &task {
                    WatchTask::Notify => zmail_imap::watch_notify(&target, &secret, on_event).await,
                    WatchTask::Idle(mailbox) => {
                        zmail_imap::watch_idle(&target, &secret, mailbox, on_event).await
                    }
                    WatchTask::Poll(mailboxes) => zmail_imap::poll_status(
                        &target,
                        &secret,
                        mailboxes,
                        zmail_imap::STATUS_POLL_INTERVAL,
                        on_event,
                    )
                    .await
                    .map(|()| WatchEnd::Unsupported),
                }
            };
            // A wake drops the connection, which is probably dead, and
            // reconnects at once.
            let Some(result) = unless_woken(&mut wake, watch).await else {
                backoff.reset();
                continue;
            };

            match (result, &task) {
                (Ok(WatchEnd::Unsupported), WatchTask::Notify) => return TaskEnd::NotifyRefused,
                // A mailbox whose IDLE the server refuses is polled instead.
                (Ok(WatchEnd::Unsupported), WatchTask::Idle(mailbox)) => {
                    task = WatchTask::Poll(vec![mailbox.clone()]);
                }
                _ => {
                    let delay =
                        backoff.after_disconnect(connected_at.map(|at: Instant| at.elapsed()));
                    if unless_woken(&mut wake, sleep(delay)).await.is_none() {
                        backoff.reset();
                    }
                }
            }
        }
    }

    pub(crate) fn account_exists(&self, account_id: i64) -> bool {
        self.inner
            .store
            .sync_target(account_id)
            .ok()
            .flatten()
            .is_some()
    }
}
