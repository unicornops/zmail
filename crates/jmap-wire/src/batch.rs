//! Fetching more records than a server takes in one call.

use crate::{error::Error, session::Session};

/// Splits a long list of ids into the `ids` of several `/get` calls.
///
/// A server says how many records one `/get` may ask for in its
/// session's `maxObjectsInGet` (RFC 8620 section 2), and answers a call
/// for more with the `requestTooLarge` method error (section 5.1). Some
/// have a lower limit they do not advertise: Apache James refuses an
/// `Email/get` of more than five emails with their bodies while its
/// session says 500. And an answer can be larger than the client is
/// willing to hold ([`ClientBuilder::max_response_bytes`]).
///
/// So batches start at the advertised limit and, when a call is refused
/// for its size or its answer is too large, halve until one goes
/// through. The size that works is kept: it never grows again.
///
/// [`Client::get_in_batches`] is the loop below for a plain `/get`;
/// write it out when each batch needs more than that.
///
/// ```
/// use jmap_wire::{Error, IdBatches};
///
/// # fn fetch(_ids: &[String]) -> Result<(), Error> { Ok(()) }
/// # fn example(ids: Vec<String>, session: &jmap_wire::Session) -> Result<(), Error> {
/// let mut batches = IdBatches::for_session(session);
/// let mut pending = ids.as_slice();
/// while !pending.is_empty() {
///     let batch = batches.next(pending);
///     match fetch(batch) {
///         Ok(()) => pending = &pending[batch.len()..],
///         // Too large, and it can be asked for in smaller pieces.
///         Err(error) if batches.shrink(batch.len(), &error) => {}
///         Err(error) => return Err(error),
///     }
/// }
/// # Ok(())
/// # }
/// ```
///
/// [`ClientBuilder::max_response_bytes`]: crate::ClientBuilder::max_response_bytes
/// [`Client::get_in_batches`]: crate::Client::get_in_batches
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdBatches {
    size: usize,
}

impl IdBatches {
    /// The batch size for a session that states no `maxObjectsInGet`:
    /// 500, the lowest a server may set it to (RFC 8620 section 2).
    pub const DEFAULT_SIZE: usize = 500;

    /// Batches of at most `size` ids (at least one).
    pub fn new(size: usize) -> Self {
        Self { size: size.max(1) }
    }

    /// Batches of the session's `maxObjectsInGet`, or of
    /// [`DEFAULT_SIZE`](Self::DEFAULT_SIZE) when the session states
    /// none (or zero, which no server can mean).
    pub fn for_session(session: &Session) -> Self {
        Self::new(
            session
                .core()
                .and_then(|core| core.max_objects_in_get)
                .and_then(|limit| usize::try_from(limit).ok())
                .filter(|limit| *limit > 0)
                .unwrap_or(Self::DEFAULT_SIZE),
        )
    }

    /// The same batches, of at most `size` ids: for calls whose answers
    /// are large per record, such as emails with their bodies.
    pub fn at_most(self, size: usize) -> Self {
        Self::new(self.size.min(size))
    }

    /// How many ids the next batch has at most.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The next batch: the first ids of `pending`, at most
    /// [`size`](Self::size) of them.
    pub fn next<'a, T>(&self, pending: &'a [T]) -> &'a [T] {
        &pending[..self.size.min(pending.len())]
    }

    /// After a call for `sent` ids failed with `error`: whether to try
    /// again with fewer. True, with the batch size halved, when the
    /// error says the request or its answer was too large
    /// ([`Error::is_request_too_large`],
    /// [`Error::is_response_too_large`]) and more than one id was sent;
    /// false for any other error, and when even one id is too many.
    pub fn shrink(&mut self, sent: usize, error: &Error) -> bool {
        if !(error.is_request_too_large() || error.is_response_too_large()) || sent <= 1 {
            return false;
        }
        self.size = (sent / 2).min(self.size).max(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        MethodError, ProblemDetails, TransportError,
        error::{MethodErrorType, TransportErrorKind},
    };

    fn method_error(error_type: &str) -> Error {
        Error::Method(
            serde_json::from_value::<MethodError>(json!({"type": error_type})).expect("decodes"),
        )
    }

    fn limit_problem(limit: &str) -> Error {
        Error::Problem(Box::new(
            serde_json::from_value::<ProblemDetails>(json!({
                "type": "urn:ietf:params:jmap:error:limit", "status": 400, "limit": limit
            }))
            .expect("decodes"),
        ))
    }

    fn session(core: serde_json::Value) -> Session {
        serde_json::from_value(json!({
            "capabilities": {"urn:ietf:params:jmap:core": core},
            "apiUrl": "https://example.com/api"
        }))
        .expect("a session")
    }

    /// A server that refuses any call for more than `limit` ids with
    /// `refusal`; returns the size of every call made.
    fn fetch_all(
        batches: &mut IdBatches,
        count: usize,
        limit: usize,
        refusal: fn() -> Error,
    ) -> Result<Vec<usize>, Error> {
        let ids = (0..count).collect::<Vec<_>>();
        let mut pending = ids.as_slice();
        let mut calls = Vec::new();
        let mut fetched = Vec::new();
        while !pending.is_empty() {
            let batch = batches.next(pending);
            calls.push(batch.len());
            if batch.len() > limit {
                let error = refusal();
                if batches.shrink(batch.len(), &error) {
                    continue;
                }
                return Err(error);
            }
            fetched.extend_from_slice(batch);
            pending = &pending[batch.len()..];
        }
        assert_eq!(fetched, ids, "every id once, in order");
        Ok(calls)
    }

    #[test]
    fn batches_follow_the_sessions_limit() {
        assert_eq!(
            IdBatches::for_session(&session(json!({"maxObjectsInGet": 40}))).size(),
            40
        );
        for unstated in [
            json!({}),
            json!({"maxObjectsInGet": 0}),
            json!({"maxObjectsInGet": null}),
        ] {
            assert_eq!(
                IdBatches::for_session(&session(unstated)).size(),
                IdBatches::DEFAULT_SIZE
            );
        }
        assert_eq!(IdBatches::new(500).at_most(250).size(), 250);
        assert_eq!(IdBatches::new(40).at_most(250).size(), 40);
        assert_eq!(IdBatches::new(0).size(), 1);

        let calls = fetch_all(&mut IdBatches::new(40), 100, 40, || {
            method_error("serverFail")
        })
        .expect("fetched");
        assert_eq!(calls, [40, 40, 20], "no call is refused");
    }

    /// Apache James: advertises 500, refuses more than five emails with
    /// their bodies.
    #[test]
    fn a_limit_the_server_does_not_advertise_is_found_by_halving_and_kept() {
        let too_large = || method_error("requestTooLarge");
        let mut batches = IdBatches::new(500);
        let calls = fetch_all(&mut batches, 12, 5, too_large).expect("fetched");
        assert_eq!(calls, [12, 6, 3, 3, 3, 3]);
        assert_eq!(batches.size(), 3);

        let calls =
            fetch_all(&mut IdBatches::new(500).at_most(250), 600, 5, too_large).expect("fetched");
        assert_eq!(calls[..7], [250, 125, 62, 31, 15, 7, 3]);
        assert!(
            calls[7..].iter().all(|size| *size <= 3),
            "never grows again"
        );
    }

    #[test]
    fn a_request_size_problem_and_an_answer_over_the_limit_halve_too() {
        let calls = fetch_all(&mut IdBatches::new(500), 5, 2, || {
            limit_problem("maxSizeRequest")
        })
        .expect("fetched");
        assert_eq!(calls, [5, 2, 2, 1]);
        let calls = fetch_all(&mut IdBatches::new(500), 5, 2, || {
            Error::Transport(TransportError::too_large(1024))
        })
        .expect("fetched");
        assert_eq!(calls, [5, 2, 2, 1]);
    }

    #[test]
    fn other_errors_and_a_single_id_that_is_too_much_are_not_retried() {
        let others: [fn() -> Error; 4] = [
            || method_error("serverFail"),
            || limit_problem("maxCallsInRequest"),
            || Error::Http { status: 401 },
            || {
                Error::Transport(TransportError::new(
                    TransportErrorKind::Timeout,
                    "timed out",
                ))
            },
        ];
        for other in others {
            let mut batches = IdBatches::new(500);
            assert!(fetch_all(&mut batches, 8, 0, other).is_err());
            assert_eq!(batches.size(), 500, "{}", other());
        }

        let mut batches = IdBatches::new(500);
        let error = fetch_all(&mut batches, 4, 0, || method_error("requestTooLarge"))
            .expect_err("nothing can be fetched");
        assert!(error.is_request_too_large());
        assert_eq!(
            error.as_method_error().map(|error| &error.error_type),
            Some(&MethodErrorType::RequestTooLarge)
        );
        assert_eq!(batches.size(), 1, "halved down to one id, then given up");
    }
}
