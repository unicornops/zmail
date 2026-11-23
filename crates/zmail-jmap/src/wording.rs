//! How a failed `jmap-wire` call is worded for the user.
//!
//! The reads moved from `jmap-client` to `jmap-wire` (ADR 0008, step 2).
//! What a server's refusal looked like in a sync error, an account's
//! "last error" or a search stays as it was: a method error
//! (`Request failed: Request too large`), a problem details answer
//! (`Request failed: Limit (status 400). Details: ...`) and a bare HTTP
//! error status (`Server failed: 503 Service Unavailable`) read as
//! `jmap-client` worded them. Failures of the exchange itself (no
//! connection, a timeout, an answer that is not JSON) are worded by
//! `jmap-wire`.

use jmap_wire::{Error, MethodError, ProblemDetails, ProblemKind, SetError};

/// `error` as an [`anyhow::Error`] with the wording above.
pub(crate) fn reported(error: Error) -> anyhow::Error {
    match &error {
        Error::Method(refusal) => anyhow::anyhow!("Request failed: {}", method_error(refusal)),
        Error::Problem(problem) => anyhow::anyhow!("Request failed: {}", problem_details(problem)),
        Error::Http { status } => match reqwest::StatusCode::from_u16(*status) {
            Ok(status) => anyhow::anyhow!("Server failed: {status}"),
            Err(_) => anyhow::anyhow!("Server failed: {status}"),
        },
        // The message alone: `jmap-wire` words its errors with their
        // causes, which an error chain would print a second time.
        _ => anyhow::anyhow!("{error}"),
    }
}

/// A server's refusal to create or change one record, worded as
/// `jmap-client` worded it: `Set failed: invalidProperties: no such
/// mailbox (properties: mailboxIds)`. A type `jmap-client` did not know
/// reads the same way, where the whole answer used to fail to parse.
pub(crate) fn set_refused(error: &SetError) -> anyhow::Error {
    let mut text = format!("Set failed: {}", error.error_type);
    if let Some(description) = &error.description {
        text.push_str(&format!(": {description}"));
    }
    if let Some(properties) = error.properties.as_ref().filter(|names| !names.is_empty()) {
        text.push_str(&format!(" (properties: {})", properties.join(", ")));
    }
    anyhow::anyhow!(text)
}

/// A method error's type in words: `requestTooLarge` as
/// `Request too large`. The server's description is left out, as it was.
fn method_error(error: &MethodError) -> String {
    let mut words = String::new();
    for (index, letter) in error.error_type.as_str().chars().enumerate() {
        if index == 0 {
            words.extend(letter.to_uppercase());
        } else if letter.is_uppercase() {
            words.push(' ');
            words.extend(letter.to_lowercase());
        } else {
            words.push(letter);
        }
    }
    words
}

/// Problem details as one line: the type, the status, the title and the
/// detail.
fn problem_details(problem: &ProblemDetails) -> String {
    let mut text = match problem.kind() {
        ProblemKind::UnknownCapability => "Unknown capability",
        ProblemKind::NotJson => "Not JSON",
        ProblemKind::NotRequest => "Not request",
        ProblemKind::Limit => "Limit",
        _ => problem.problem_type.as_str(),
    }
    .to_owned();
    if let Some(status) = problem.status {
        text.push_str(&format!(" (status {status})"));
    }
    if let Some(title) = &problem.title {
        text.push_str(&format!(": {title}"));
    }
    if let Some(detail) = &problem.detail {
        text.push_str(&format!(". Details: {detail}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn wire_method_error(error_type: &str) -> Error {
        Error::Method(
            serde_json::from_value(json!({"type": error_type, "description": "not shown"}))
                .expect("a method error"),
        )
    }

    /// Every method error type `jmap-client` knows reads as it did.
    /// Goes with `jmap-client` in step 4.
    #[test]
    fn method_errors_read_as_jmap_client_worded_them() {
        use jmap_client::core::error::{MethodError, MethodErrorType};

        let types = [
            (MethodErrorType::ServerUnavailable, "serverUnavailable"),
            (MethodErrorType::ServerFail, "serverFail"),
            (MethodErrorType::ServerPartialFail, "serverPartialFail"),
            (MethodErrorType::UnknownMethod, "unknownMethod"),
            (MethodErrorType::InvalidArguments, "invalidArguments"),
            (
                MethodErrorType::InvalidResultReference,
                "invalidResultReference",
            ),
            (MethodErrorType::Forbidden, "forbidden"),
            (MethodErrorType::AccountNotFound, "accountNotFound"),
            (
                MethodErrorType::AccountNotSupportedByMethod,
                "accountNotSupportedByMethod",
            ),
            (MethodErrorType::AccountReadOnly, "accountReadOnly"),
            (MethodErrorType::RequestTooLarge, "requestTooLarge"),
            (
                MethodErrorType::CannotCalculateChanges,
                "cannotCalculateChanges",
            ),
            (MethodErrorType::StateMismatch, "stateMismatch"),
            (MethodErrorType::AlreadyExists, "alreadyExists"),
            (MethodErrorType::FromAccountNotFound, "fromAccountNotFound"),
            (
                MethodErrorType::FromAccountNotSupportedByMethod,
                "fromAccountNotSupportedByMethod",
            ),
            (MethodErrorType::AnchorNotFound, "anchorNotFound"),
            (MethodErrorType::UnsupportedSort, "unsupportedSort"),
            (MethodErrorType::UnsupportedFilter, "unsupportedFilter"),
            (MethodErrorType::TooManyChanges, "tooManyChanges"),
        ];
        for (old, name) in types {
            assert_eq!(
                reported(wire_method_error(name)).to_string(),
                jmap_client::Error::Method(MethodError { p_type: old }).to_string(),
                "{name}"
            );
        }
        // A type of a later specification, which `jmap-client` could
        // not read at all, is worded the same way.
        assert_eq!(
            reported(wire_method_error("rateLimited")).to_string(),
            "Request failed: Rate limited"
        );
    }

    #[test]
    fn problems_and_statuses_read_as_jmap_client_worded_them() {
        let problems = [
            json!({"type": "urn:ietf:params:jmap:error:limit", "status": 400,
                   "limit": "maxSizeRequest", "detail": "Too big"}),
            json!({"type": "urn:ietf:params:jmap:error:unknownCapability", "status": 400,
                   "title": "Unknown", "detail": "No such capability"}),
            json!({"type": "urn:ietf:params:jmap:error:notJSON"}),
            json!({"type": "urn:ietf:params:jmap:error:notRequest", "title": "Not a request"}),
            json!({"type": "https://example.com/problems/maintenance", "status": 503}),
        ];
        for problem in problems {
            let old: jmap_client::core::error::ProblemDetails =
                serde_json::from_value(problem.clone()).expect("jmap-client reads it");
            let new: ProblemDetails = serde_json::from_value(problem).expect("jmap-wire reads it");
            assert_eq!(
                reported(Error::Problem(Box::new(new))).to_string(),
                jmap_client::Error::Problem(Box::new(old)).to_string()
            );
        }

        for (status, text) in [
            (401, "Server failed: 401 Unauthorized"),
            (404, "Server failed: 404 Not Found"),
            (502, "Server failed: 502 Bad Gateway"),
            (503, "Server failed: 503 Service Unavailable"),
            (599, "Server failed: 599 <unknown status code>"),
        ] {
            assert_eq!(reported(Error::Http { status }).to_string(), text);
            assert_eq!(
                jmap_client::Error::Server(
                    reqwest::StatusCode::from_u16(status)
                        .expect("a status")
                        .to_string()
                )
                .to_string(),
                text
            );
        }
    }

    #[test]
    fn failures_of_the_exchange_keep_jmap_wires_wording_once() {
        let error = reported(Error::MissingResponse {
            call_id: "c0".to_owned(),
        });
        assert_eq!(
            format!("{error:#}"),
            "the JMAP server gave no answer to method call c0"
        );
        let not_json = serde_json::from_str::<jmap_wire::Response>("<html>")
            .map_err(|source| Error::Json {
                what: "response".to_owned(),
                source,
            })
            .expect_err("not JSON");
        assert_eq!(
            format!("{:#}", reported(not_json)),
            "the JMAP server's response could not be read: expected value at line 1 column 1",
            "the cause is said once"
        );
    }
}
