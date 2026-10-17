//! Sleep/wake and network-change recovery for the mail watchers.
//!
//! After a laptop sleeps or changes network, every long-lived IDLE, NOTIFY
//! and push connection is most likely dead, but nothing says so until a
//! keepalive or renewal fails minutes later. A watchdog compares the wall
//! clock with the monotonic clock, which stops while the machine sleeps on
//! Linux and macOS, and also notices when its own timer fires far too late
//! (a suspended or frozen process on any platform). Either way it raises a
//! wake signal: every watcher drops its connection, reconnects at once
//! without waiting out its backoff, and the reconnect resyncs.

use std::{
    future::Future,
    time::{Duration, Instant, SystemTime},
};

use tokio::sync::watch;

use crate::SyncService;

/// How often the watchdog compares the clocks.
const WAKE_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// Unexplained time beyond which the machine is assumed to have slept (or
/// the process to have been frozen).
const WAKE_JUMP_THRESHOLD: Duration = Duration::from_secs(30);

/// Whether one watchdog tick saw a sleep or freeze.
///
/// `expected` is the tick interval, `monotonic` how much monotonic time
/// passed and `wall` how much wall-clock time passed (`None` when the wall
/// clock went backwards, which says nothing about sleep). Wall time far
/// beyond monotonic time means the monotonic clock was paused by a sleep;
/// monotonic time far beyond the interval means the timer could not fire,
/// because the process (or the whole machine, on platforms whose monotonic
/// clock keeps counting through sleep) was suspended.
pub(crate) fn woke_up(expected: Duration, monotonic: Duration, wall: Option<Duration>) -> bool {
    let slept = wall.is_some_and(|wall| wall.saturating_sub(monotonic) > WAKE_JUMP_THRESHOLD);
    let frozen = monotonic.saturating_sub(expected) > WAKE_JUMP_THRESHOLD;
    slept || frozen
}

/// Broadcasts wake signals to the watchers.
#[derive(Debug)]
pub(crate) struct WakeSignal {
    sender: watch::Sender<u64>,
}

impl WakeSignal {
    pub(crate) fn new() -> Self {
        Self {
            sender: watch::channel(0).0,
        }
    }

    /// Wakes every current subscriber.
    pub(crate) fn raise(&self) {
        self.sender.send_modify(|generation| *generation += 1);
    }

    /// A receiver that only sees signals raised after this call.
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.sender.subscribe()
    }
}

/// Runs `future` unless a wake signal arrives first, returning `None` when
/// woken. Dropping the future closes whatever connection it held.
pub(crate) async fn unless_woken<F: Future>(
    wake: &mut watch::Receiver<u64>,
    future: F,
) -> Option<F::Output> {
    tokio::select! {
        output = future => Some(output),
        changed = wake.changed() => match changed {
            Ok(()) => None,
            // The signal can no longer fire; finish the work normally.
            Err(_) => std::future::pending().await,
        },
    }
}

impl SyncService {
    /// Tells the watchers that the machine woke up or the network changed,
    /// so they reconnect and resync now instead of after their keepalives
    /// fail.
    ///
    /// The built-in clock watchdog calls this after a detected sleep; the
    /// app can also call it from platform wake or network notifications.
    pub fn signal_wake(&self) {
        self.inner.wake.raise();
    }

    /// Starts the clock watchdog once.
    pub(crate) fn start_wake_watchdog(&self) {
        if self
            .inner
            .wake_watchdog_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let service = self.clone();
        self.inner.handle.spawn(async move {
            let mut last_monotonic = Instant::now();
            let mut last_wall = SystemTime::now();
            loop {
                tokio::time::sleep(WAKE_CHECK_INTERVAL).await;
                let now_monotonic = Instant::now();
                let now_wall = SystemTime::now();
                let monotonic = now_monotonic.duration_since(last_monotonic);
                let wall = now_wall.duration_since(last_wall).ok();
                if woke_up(WAKE_CHECK_INTERVAL, monotonic, wall) {
                    service.signal_wake();
                }
                last_monotonic = now_monotonic;
                last_wall = now_wall;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_secs(5);

    #[test]
    fn a_normal_tick_is_not_a_wake() {
        assert!(!woke_up(
            TICK,
            Duration::from_millis(5_010),
            Some(Duration::from_millis(5_020))
        ));
        assert!(
            !woke_up(TICK, TICK, Some(Duration::from_secs(20))),
            "small clock adjustments are ignored"
        );
    }

    #[test]
    fn wall_time_beyond_monotonic_time_is_a_sleep() {
        // Linux and macOS: the monotonic clock paused for an hour.
        assert!(woke_up(TICK, TICK, Some(Duration::from_secs(3_605))));
    }

    #[test]
    fn a_late_timer_is_a_freeze() {
        // Platforms whose monotonic clock counts through sleep, or a
        // process stopped by the OS.
        assert!(woke_up(
            TICK,
            Duration::from_secs(600),
            Some(Duration::from_secs(600))
        ));
    }

    #[test]
    fn a_wall_clock_set_backwards_is_not_a_wake() {
        assert!(!woke_up(TICK, TICK, None));
    }

    #[tokio::test]
    async fn wake_signals_cancel_running_work() {
        let signal = WakeSignal::new();
        let mut wake = signal.subscribe();
        let raise = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            signal.raise();
        };
        let (woken, ()) =
            tokio::join!(unless_woken(&mut wake, std::future::pending::<()>()), raise);
        assert_eq!(woken, None);

        // Without a signal the work completes.
        assert_eq!(unless_woken(&mut wake, async { 7 }).await, Some(7));
    }

    #[tokio::test]
    async fn signals_raised_before_subscribing_are_not_seen() {
        let signal = WakeSignal::new();
        signal.raise();
        let mut wake = signal.subscribe();
        assert_eq!(unless_woken(&mut wake, async { 1 }).await, Some(1));
    }
}
