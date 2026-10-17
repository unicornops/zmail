//! Reconnect backoff for the background mail watchers.

use std::time::Duration;

/// First reconnect delay after a failure.
pub(crate) const RECONNECT_BASE_DELAY: Duration = Duration::from_secs(1);
/// Longest reconnect delay.
pub(crate) const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(60);
/// A connection that stayed up this long counts as healthy, so its loss
/// reconnects quickly instead of continuing an earlier backoff.
pub(crate) const HEALTHY_CONNECTION: Duration = Duration::from_secs(60);

/// Exponential reconnect backoff: each failure doubles the delay up to a
/// cap, and a success (or a connection that stayed up long enough) starts
/// over from the base delay.
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    base: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub(crate) const fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            next: base,
        }
    }

    /// The watcher default: one second doubling to a minute.
    pub(crate) const fn reconnect() -> Self {
        Self::new(RECONNECT_BASE_DELAY, RECONNECT_MAX_DELAY)
    }

    /// Returns the delay to wait now and doubles the following one.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(self.max);
        delay
    }

    /// Starts over from the base delay.
    pub(crate) fn reset(&mut self) {
        self.next = self.base;
    }

    /// Returns the delay before reconnecting a connection that was up for
    /// `connected_for` (`None` when it never opened).
    ///
    /// A long-lived connection that drops reconnects after the base delay;
    /// one that fails to open or drops straight away keeps backing off, so
    /// a server or proxy that closes every stream at once is not hammered.
    pub(crate) fn after_disconnect(&mut self, connected_for: Option<Duration>) -> Duration {
        if connected_for.is_some_and(|uptime| uptime >= HEALTHY_CONNECTION) {
            self.reset();
        }
        self.next_delay()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_double_up_to_the_cap_and_reset() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(5));
        let delays = (0..5).map(|_| backoff.next_delay()).collect::<Vec<_>>();
        assert_eq!(
            delays,
            [1, 2, 4, 5, 5].map(Duration::from_secs),
            "the delay doubles and stops at the cap"
        );
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn healthy_connections_reconnect_quickly() {
        let mut backoff = Backoff::reconnect();
        for _ in 0..4 {
            backoff.after_disconnect(None);
        }
        assert_eq!(
            backoff.after_disconnect(Some(Duration::from_secs(2))),
            Duration::from_secs(16),
            "a connection that dropped at once keeps backing off"
        );
        assert_eq!(
            backoff.after_disconnect(Some(HEALTHY_CONNECTION)),
            RECONNECT_BASE_DELAY,
            "a long-lived connection starts over"
        );
        assert_eq!(backoff.after_disconnect(None), Duration::from_secs(2));
    }

    #[test]
    fn the_cap_saturates_instead_of_overflowing() {
        let mut backoff = Backoff::new(Duration::MAX / 2, Duration::MAX);
        backoff.next_delay();
        backoff.next_delay();
        assert_eq!(backoff.next_delay(), Duration::MAX);
    }
}
