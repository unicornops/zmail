//! What the requests of one piece of work cost (#478).
//!
//! [`tally`] runs a future and counts the API requests it makes: how many,
//! how many answers were rate limiting (429 or 503), and how long the
//! retries waited. The count is task-local, so requests other tasks make
//! on the same [`Manager`](crate::Manager) at the same time (another
//! account's sync, a message being opened) are not in it, and a tally
//! inside another adds its requests to the outer one too.

use std::{cell::RefCell, future::Future, time::Duration};

use tokio::time::Instant;

/// The requests one piece of work made, from [`tally`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestTally {
    /// Requests made. A request retried after rate limiting or a dropped
    /// connection counts once; a refresh after a 401 and the retried
    /// request count as two more.
    pub requests: u32,
    /// Answers that were rate limiting (429) or the API being unavailable
    /// (503), whether or not they were retried.
    pub rate_limited: u32,
    /// Time spent waiting before retries, of rate limiting or of a
    /// dropped connection.
    pub retry_wait: Duration,
    /// How long the work took, requests and all.
    pub elapsed: Duration,
}

impl RequestTally {
    fn add_counts(&mut self, other: &Self) {
        self.requests = self.requests.saturating_add(other.requests);
        self.rate_limited = self.rate_limited.saturating_add(other.rate_limited);
        self.retry_wait = self.retry_wait.saturating_add(other.retry_wait);
    }
}

tokio::task_local! {
    static TALLY: RefCell<RequestTally>;
}

/// Runs `work` and returns its output with the requests it made.
pub async fn tally<F: Future>(work: F) -> (F::Output, RequestTally) {
    let started = Instant::now();
    let (output, counted) = TALLY
        .scope(RefCell::new(RequestTally::default()), async {
            let output = work.await;
            (output, TALLY.with(|counted| *counted.borrow()))
        })
        .await;
    let counted = RequestTally {
        elapsed: started.elapsed(),
        ..counted
    };
    // An enclosing tally counts these requests too.
    let _ = TALLY.try_with(|outer| outer.borrow_mut().add_counts(&counted));
    (output, counted)
}

/// Counts with the running tally, if any.
fn count(update: impl FnOnce(&mut RequestTally)) {
    let _ = TALLY.try_with(|counted| update(&mut counted.borrow_mut()));
}

/// A request is being sent.
pub(crate) fn count_request() {
    count(|counted| counted.requests = counted.requests.saturating_add(1));
}

/// An answer was rate limiting or the API being unavailable.
pub(crate) fn count_rate_limited() {
    count(|counted| counted.rate_limited = counted.rate_limited.saturating_add(1));
}

/// A retry waits `delay` first.
pub(crate) fn count_retry_wait(delay: Duration) {
    count(|counted| counted.retry_wait = counted.retry_wait.saturating_add(delay));
}
