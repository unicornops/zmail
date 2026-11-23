//! Splitting a long list of ids into `*/get` requests the server accepts.
//!
//! A server says how many objects one `*/get` may ask for in its session's
//! `maxObjectsInGet` (RFC 8620 section 2), and answers a larger request
//! with `requestTooLarge`. Some servers have a lower limit they never
//! advertise: Apache James refuses an `Email/get` of more than five emails
//! with their bodies (its `email.get.full.max.size`, default 5) while it
//! advertises 500. So a batch starts at the advertised limit and, when the
//! server still says the request is too large, halves until it is
//! accepted, keeping that size for the rest of the fetch. An answer larger
//! than Zmail reads at once halves the batch in the same way.
//!
//! The halving itself is `jmap-wire`'s ([`IdBatches`]); this adds Zmail's
//! cap of a query page per request and the errors users read.

use anyhow::{Result, anyhow};
use jmap_wire::IdBatches;

use crate::{Connection, QUERY_PAGE_SIZE, wording};

/// How many ids each `*/get` of one fetch asks for.
#[derive(Debug)]
pub(crate) struct GetBatches {
    batches: IdBatches,
    /// What is fetched, for the error when even one is too many.
    what: &'static str,
}

impl GetBatches {
    /// Batches of `what` within the client's session limit.
    pub(crate) fn new(client: &Connection, what: &'static str) -> Self {
        Self::with_batches(IdBatches::for_session(client.wire_session()), what)
    }

    /// `batches`, capped at a query page.
    fn with_batches(batches: IdBatches, what: &'static str) -> Self {
        Self {
            batches: batches.at_most(QUERY_PAGE_SIZE),
            what,
        }
    }

    #[cfg(test)]
    fn with_limit(limit: usize, what: &'static str) -> Self {
        Self::with_batches(IdBatches::new(limit), what)
    }

    #[cfg(test)]
    fn size(&self) -> usize {
        self.batches.size()
    }

    /// Fetches the next batch of `pending` with `get` and moves `pending`
    /// past the ids fetched.
    ///
    /// A `requestTooLarge` answer, or an answer over the size Zmail reads,
    /// retries the same ids in halves, down to one id; the size that works
    /// is kept for the following batches. Any other error fails at once.
    ///
    /// `get` builds the request for a batch and returns the future that
    /// sends it, which must not borrow the batch.
    pub(crate) async fn next<T, F>(
        &mut self,
        pending: &mut &[String],
        mut get: impl FnMut(&[String]) -> F,
    ) -> Result<T>
    where
        F: Future<Output = jmap_wire::Result<T>>,
    {
        loop {
            let batch = self.batches.next(pending);
            let take = batch.len();
            match get(batch).await {
                Ok(found) => {
                    *pending = &pending[take..];
                    return Ok(found);
                }
                Err(error) if self.batches.shrink(take, &error) => {}
                Err(error) if error.is_request_too_large() => {
                    return Err(anyhow!(
                        "the JMAP server refused to return even one {} per request: {}",
                        self.what,
                        wording::reported(error)
                    ));
                }
                Err(jmap_wire::Error::Transport(error))
                    if error.kind() == jmap_wire::TransportErrorKind::TooLarge =>
                {
                    return Err(anyhow!(
                        "the JMAP server's answer for one {} is larger than Zmail reads: {error}",
                        self.what
                    ));
                }
                Err(error) => return Err(wording::reported(error)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use jmap_wire::Error;
    use serde_json::json;

    use super::*;

    fn method_error(error_type: &str) -> Error {
        Error::Method(serde_json::from_value(json!({"type": error_type})).expect("a method error"))
    }

    fn too_large() -> Error {
        method_error("requestTooLarge")
    }

    fn limit_problem(limit: &str) -> Error {
        Error::Problem(Box::new(
            serde_json::from_value(json!({
                "type": "urn:ietf:params:jmap:error:limit", "status": 400, "limit": limit
            }))
            .expect("problem details"),
        ))
    }

    fn ids(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("e{index}")).collect()
    }

    fn session(core: serde_json::Value) -> jmap_wire::Session {
        serde_json::from_value(json!({
            "capabilities": {"urn:ietf:params:jmap:core": core},
            "accounts": {},
            "primaryAccounts": {},
            "username": "me@example.test",
            "apiUrl": "https://example.test/jmap",
            "downloadUrl": "https://example.test/download",
            "uploadUrl": "https://example.test/upload",
            "eventSourceUrl": "https://example.test/events",
            "state": "s0",
        }))
        .expect("a session")
    }

    fn core(max_objects_in_get: usize) -> serde_json::Value {
        json!({
            "maxSizeUpload": 50_000_000,
            "maxConcurrentUpload": 4,
            "maxSizeRequest": 10_000_000,
            "maxConcurrentRequests": 4,
            "maxCallsInRequest": 16,
            "maxObjectsInGet": max_objects_in_get,
            "maxObjectsInSet": 500,
            "collationAlgorithms": [],
        })
    }

    /// The batches a fetch starts with for a session.
    fn for_session(session: &jmap_wire::Session) -> usize {
        GetBatches::with_batches(IdBatches::for_session(session), "email").size()
    }

    /// A server that refuses any request for more than `limit` ids with
    /// `error`, recording the size of every request it is sent.
    struct Server {
        limit: usize,
        error: fn() -> Error,
        requests: RefCell<Vec<usize>>,
    }

    impl Server {
        fn new(limit: usize) -> Self {
            Self {
                limit,
                error: too_large,
                requests: RefCell::new(Vec::new()),
            }
        }

        async fn get(&self, batch: Vec<String>) -> jmap_wire::Result<Vec<String>> {
            self.requests.borrow_mut().push(batch.len());
            if batch.len() > self.limit {
                return Err((self.error)());
            }
            Ok(batch)
        }
    }

    /// Fetches every id through `batches`, as the callers do.
    fn fetch_all(batches: &mut GetBatches, server: &Server, all: &[String]) -> Result<Vec<String>> {
        futures_util::FutureExt::now_or_never(async {
            let mut pending = all;
            let mut fetched = Vec::new();
            while !pending.is_empty() {
                fetched.extend(
                    batches
                        .next(&mut pending, |batch| server.get(batch.to_vec()))
                        .await?,
                );
            }
            Ok(fetched)
        })
        .expect("the fake server never waits")
    }

    #[test]
    fn batches_follow_the_advertised_limit_up_to_a_page() {
        assert_eq!(for_session(&session(core(500))), QUERY_PAGE_SIZE);
        assert_eq!(for_session(&session(core(40))), 40);
        assert_eq!(
            for_session(&session(core(0))),
            QUERY_PAGE_SIZE,
            "zero is no limit a server can mean"
        );
        assert_eq!(
            for_session(&session(json!({"maxObjectsInGet": 40}))),
            40,
            "a core capability with only some of its limits still gives the one it has"
        );
        assert_eq!(
            for_session(&session(json!({}))),
            QUERY_PAGE_SIZE,
            "no advertised limit: a page"
        );

        assert_eq!(GetBatches::with_limit(500, "email").size(), QUERY_PAGE_SIZE);
        assert_eq!(GetBatches::with_limit(40, "email").size(), 40);

        let server = Server::new(40);
        let all = ids(100);
        let fetched = fetch_all(&mut GetBatches::with_limit(40, "email"), &server, &all)
            .expect("every id is fetched");
        assert_eq!(fetched, all);
        assert_eq!(
            *server.requests.borrow(),
            [40, 40, 20],
            "no request is refused"
        );
    }

    #[test]
    fn a_hidden_limit_halves_the_batch_once_and_keeps_it() {
        // Apache James: advertises 500, refuses more than five full emails.
        let server = Server::new(5);
        let all = ids(12);
        let mut batches = GetBatches::with_limit(500, "email");
        let fetched = fetch_all(&mut batches, &server, &all).expect("every id is fetched");

        assert_eq!(fetched, all, "every id once, in order");
        assert_eq!(
            *server.requests.borrow(),
            [12, 6, 3, 3, 3, 3],
            "the batch halves until accepted, then stays at that size"
        );
        assert_eq!(batches.size(), 3);
    }

    #[test]
    fn halving_from_a_full_page_finds_a_size_the_server_takes() {
        let server = Server::new(5);
        let all = ids(600);
        let fetched = fetch_all(&mut GetBatches::with_limit(500, "email"), &server, &all)
            .expect("every id is fetched");
        assert_eq!(fetched, all);
        let requests = server.requests.borrow();
        assert_eq!(
            requests[..7],
            [250, 125, 62, 31, 15, 7, 3],
            "halves from a page"
        );
        assert!(
            requests[7..].iter().all(|size| *size <= 3),
            "never grows again: {requests:?}"
        );
        assert_eq!(requests.iter().filter(|size| **size > 5).count(), 6);
    }

    #[test]
    fn a_server_that_refuses_one_id_fails_with_a_clear_error() {
        let server = Server::new(0);
        let all = ids(4);
        let error = fetch_all(&mut GetBatches::with_limit(500, "email"), &server, &all)
            .expect_err("nothing can be fetched");
        assert_eq!(
            error.to_string(),
            "the JMAP server refused to return even one email per request: \
             Request failed: Request too large"
        );
        assert_eq!(
            *server.requests.borrow(),
            [4, 2, 1],
            "stops after one id is refused"
        );
    }

    #[test]
    fn other_errors_fail_without_a_retry() {
        type Failure = (fn() -> Error, &'static str);
        let others: [Failure; 4] = [
            (|| method_error("serverFail"), "Request failed: Server fail"),
            (
                || Error::Http { status: 401 },
                "Server failed: 401 Unauthorized",
            ),
            (
                || Error::MissingResponse {
                    call_id: "c0".to_owned(),
                },
                "the JMAP server gave no answer to method call c0",
            ),
            (
                || limit_problem("maxCallsInRequest"),
                "Request failed: Limit (status 400)",
            ),
        ];
        for (error, message) in others {
            let server = Server {
                error,
                ..Server::new(0)
            };
            let failure = fetch_all(&mut GetBatches::with_limit(500, "email"), &server, &ids(8))
                .expect_err("the server fails")
                .to_string();
            assert_eq!(
                *server.requests.borrow(),
                [8],
                "{failure} is not retried smaller"
            );
            assert_eq!(failure, message, "the error is passed on, worded as before");
        }
    }

    #[test]
    fn a_request_size_problem_halves_like_request_too_large() {
        let server = Server {
            error: || limit_problem("maxSizeRequest"),
            ..Server::new(2)
        };
        let all = ids(5);
        let fetched = fetch_all(&mut GetBatches::with_limit(500, "email"), &server, &all)
            .expect("every id is fetched");
        assert_eq!(fetched, all);
        assert_eq!(*server.requests.borrow(), [5, 2, 2, 1]);
    }

    /// An answer over the size Zmail reads at once: fewer emails per
    /// request, and a clear error when one email alone is over it.
    #[test]
    fn an_answer_that_is_too_large_halves_the_batch_too() {
        let over = || Error::Transport(jmap_wire::TransportError::too_large(1024));
        let server = Server {
            error: over,
            ..Server::new(2)
        };
        let all = ids(5);
        let fetched = fetch_all(&mut GetBatches::with_limit(500, "email"), &server, &all)
            .expect("every id is fetched");
        assert_eq!(fetched, all);
        assert_eq!(*server.requests.borrow(), [5, 2, 2, 1]);

        let server = Server {
            error: over,
            ..Server::new(0)
        };
        let error = fetch_all(&mut GetBatches::with_limit(500, "email"), &server, &ids(2))
            .expect_err("one email is too large");
        assert_eq!(
            error.to_string(),
            "the JMAP server's answer for one email is larger than Zmail reads: \
             the response is larger than 1024 bytes"
        );
    }
}
