//! How a failed `jmap-wire` call is worded for the user.
//!
//! Zmail's JMAP calls moved from `jmap-client` to `jmap-wire` (ADR
//! 0008). What a server's refusal looked like in a sync error, an
//! account's "last error", a search or a failed sign-in stayed as it
//! was: a method error (`Request failed: Request too large`), a problem
//! details answer (`Request failed: Limit (status 400). Details: ...`)
//! and a bare HTTP error status (`Server failed: 503 Service
//! Unavailable`) read as they always did; "Unauthorized" in the last is
//! what the add-account flow takes for refused credentials. Failures of
//! the exchange itself (no connection, a timeout, an answer that is not
//! JSON) are worded by `jmap-wire`.

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

/// A server's refusal to create or change one record: `Set failed:
/// invalidProperties: no such mailbox (properties: mailboxIds)`. A type
/// from no RFC reads the same way.
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

    /// Every method error type of RFC 8620 and RFC 8621 reads as it
    /// always did (the wording was held to `jmap-client`'s while both
    /// libraries were in the tree).
    #[test]
    fn method_errors_read_as_they_always_did() {
        for (name, words) in [
            ("serverUnavailable", "Server unavailable"),
            ("serverFail", "Server fail"),
            ("serverPartialFail", "Server partial fail"),
            ("unknownMethod", "Unknown method"),
            ("invalidArguments", "Invalid arguments"),
            ("invalidResultReference", "Invalid result reference"),
            ("forbidden", "Forbidden"),
            ("accountNotFound", "Account not found"),
            (
                "accountNotSupportedByMethod",
                "Account not supported by method",
            ),
            ("accountReadOnly", "Account read only"),
            ("requestTooLarge", "Request too large"),
            ("cannotCalculateChanges", "Cannot calculate changes"),
            ("stateMismatch", "State mismatch"),
            ("alreadyExists", "Already exists"),
            ("fromAccountNotFound", "From account not found"),
            (
                "fromAccountNotSupportedByMethod",
                "From account not supported by method",
            ),
            ("anchorNotFound", "Anchor not found"),
            ("unsupportedSort", "Unsupported sort"),
            ("unsupportedFilter", "Unsupported filter"),
            ("tooManyChanges", "Too many changes"),
            // A type of a later specification is worded the same way.
            ("rateLimited", "Rate limited"),
        ] {
            assert_eq!(
                reported(wire_method_error(name)).to_string(),
                format!("Request failed: {words}"),
                "{name}"
            );
        }
    }

    #[test]
    fn problems_and_statuses_read_as_they_always_did() {
        for (problem, text) in [
            (
                json!({"type": "urn:ietf:params:jmap:error:limit", "status": 400,
                       "limit": "maxSizeRequest", "detail": "Too big"}),
                "Request failed: Limit (status 400). Details: Too big",
            ),
            (
                json!({"type": "urn:ietf:params:jmap:error:unknownCapability", "status": 400,
                       "title": "Unknown", "detail": "No such capability"}),
                "Request failed: Unknown capability (status 400): Unknown. \
                 Details: No such capability",
            ),
            (
                json!({"type": "urn:ietf:params:jmap:error:notJSON"}),
                "Request failed: Not JSON",
            ),
            (
                json!({"type": "urn:ietf:params:jmap:error:notRequest", "title": "Not a request"}),
                "Request failed: Not request: Not a request",
            ),
            (
                json!({"type": "https://example.com/problems/maintenance", "status": 503}),
                "Request failed: https://example.com/problems/maintenance (status 503)",
            ),
        ] {
            let problem: ProblemDetails = serde_json::from_value(problem).expect("reads");
            assert_eq!(
                reported(Error::Problem(Box::new(problem))).to_string(),
                text
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
