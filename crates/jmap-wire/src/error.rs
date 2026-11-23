//! Errors: this crate's own [`Error`], and the three kinds a JMAP server
//! reports: request-level problem details ([`ProblemDetails`], RFC 8620
//! section 3.6.1), method-level errors ([`MethodError`], section 3.6.2)
//! and the per-record errors of a `/set` ([`SetError`], section 5.3).
//!
//! The error type enums keep types they do not know (`Other`), with the
//! server's own string, and every error keeps the properties this crate
//! has no field for (`extra`).

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A `Result` whose error is this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Why a JMAP exchange failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The HTTP exchange itself failed: no connection, a timeout, a
    /// response larger than the limit.
    #[error("the JMAP request could not be sent: {0}")]
    Transport(#[from] TransportError),
    /// The server answered with an HTTP error and a problem details body
    /// (RFC 8620 section 3.6.1, RFC 7807).
    #[error("the JMAP server refused the request: {0}")]
    Problem(Box<ProblemDetails>),
    /// The server answered with an HTTP error and no problem details.
    #[error("the JMAP server answered HTTP {status}")]
    Http {
        /// The HTTP status code.
        status: u16,
    },
    /// A redirect was refused, by the [`RedirectPolicy`] or by the rules
    /// that always hold (see [`RedirectPolicy`]).
    ///
    /// [`RedirectPolicy`]: crate::RedirectPolicy
    #[error("the JMAP server redirected to {to}, which was refused: {reason}")]
    Redirect {
        /// Where the redirect led. Its query string, which may carry
        /// secrets, is left out.
        to: String,
        /// Why it was not followed.
        reason: RedirectRefusal,
    },
    /// What the server sent is not the JSON it should be.
    #[error("the JMAP server's {what} could not be read: {source}")]
    Json {
        /// What was being read: `"session"`, `"response"`, or a method
        /// name.
        what: String,
        /// The decoding error.
        #[source]
        source: serde_json::Error,
    },
    /// A request could not be turned into JSON.
    #[error("the JMAP request could not be encoded: {0}")]
    Encode(#[source] serde_json::Error),
    /// The method call failed (RFC 8620 section 3.6.2). Returned when a
    /// method's typed answer is asked for and the server answered `error`.
    #[error("{0}")]
    Method(MethodError),
    /// The response has no answer to a method call.
    #[error("the JMAP server gave no answer to method call {call_id}")]
    MissingResponse {
        /// The call id that went unanswered.
        call_id: String,
    },
    /// The answer to a method call has another name than the call's.
    #[error("the JMAP server answered {expected} with {found}")]
    UnexpectedResponse {
        /// The method that was called.
        expected: String,
        /// The name of the answer.
        found: String,
    },
    /// The credentials cannot be sent: they contain characters an HTTP
    /// header cannot carry. Nothing was sent.
    #[error("the credentials contain characters that cannot be sent in an HTTP header")]
    InvalidCredentials,
    /// The source of bearer tokens failed.
    #[error("no access token: {0}")]
    Token(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A URL is not one a JMAP client can use.
    #[error("{url} is not a usable JMAP URL: {reason}")]
    InvalidUrl {
        /// The URL, as given.
        url: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The session lacks something the call needs, such as an account
    /// for a capability.
    #[error("the JMAP session has no {0}")]
    Session(String),
    /// A result reference in a request cannot resolve (RFC 8620 section
    /// 3.7): it names no earlier call of the same request, or a call of
    /// another method. Nothing was sent.
    #[error("the result reference in {argument} cannot resolve: {reason}")]
    InvalidResultReference {
        /// The argument that holds the reference, with its `#`.
        argument: String,
        /// What is wrong with it.
        reason: String,
    },
}

impl Error {
    pub(crate) fn json(what: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Json {
            what: what.into(),
            source,
        }
    }

    /// The method-level error, when that is what this is.
    pub fn as_method_error(&self) -> Option<&MethodError> {
        match self {
            Self::Method(error) => Some(error),
            _ => None,
        }
    }

    /// Whether the server refused the request for its size, so that the
    /// same call with fewer ids or objects may succeed: the
    /// `requestTooLarge` method error (RFC 8620 sections 5.1 and 5.3),
    /// or the request-level `limit` problem for `maxSizeRequest`
    /// (section 3.6.1).
    pub fn is_request_too_large(&self) -> bool {
        match self {
            Self::Method(error) => error.error_type == MethodErrorType::RequestTooLarge,
            Self::Problem(problem) => {
                problem.kind() == ProblemKind::Limit
                    && problem.limit.as_deref() == Some("maxSizeRequest")
            }
            _ => false,
        }
    }

    /// Whether the answer was larger than the client's response size
    /// limit ([`ClientBuilder::max_response_bytes`]), so that asking
    /// for less in one call may succeed.
    ///
    /// [`ClientBuilder::max_response_bytes`]: crate::ClientBuilder::max_response_bytes
    pub fn is_response_too_large(&self) -> bool {
        matches!(self, Self::Transport(error) if error.kind() == TransportErrorKind::TooLarge)
    }
}

/// Why a redirect was not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RedirectRefusal {
    /// The [`RedirectPolicy`](crate::RedirectPolicy) does not allow its
    /// target.
    Policy,
    /// It leads from HTTPS to HTTP, or away from HTTP altogether.
    Insecure,
    /// It has no `Location` that is a URL.
    BadLocation,
    /// More redirects than the policy's limit.
    TooMany,
    /// It would turn a POST into a GET (301, 302 and 303), which makes no
    /// sense for a JMAP API request.
    MethodChange,
}

impl fmt::Display for RedirectRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Policy => "the redirect policy does not allow it",
            Self::Insecure => "it is not HTTPS",
            Self::BadLocation => "it is not a URL",
            Self::TooMany => "too many redirects",
            Self::MethodChange => "it would turn the POST into a GET",
        })
    }
}

/// What went wrong in an HTTP exchange, as an
/// [`HttpTransport`](crate::HttpTransport) reports it.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    kind: TransportErrorKind,
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

/// The broad kind of a [`TransportError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportErrorKind {
    /// No connection could be made (DNS, TCP, TLS).
    Connect,
    /// The exchange took longer than its timeout.
    Timeout,
    /// The response body is larger than the request allowed.
    TooLarge,
    /// Anything else.
    Other,
}

impl TransportError {
    /// A transport error of `kind`. `message` must not contain secrets
    /// (header values, query strings).
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// The same error with its underlying cause.
    pub fn with_source(
        mut self,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The error for a body over `limit` bytes.
    pub fn too_large(limit: usize) -> Self {
        Self::new(
            TransportErrorKind::TooLarge,
            format!("the response is larger than {limit} bytes"),
        )
    }

    /// The broad kind of failure.
    pub fn kind(&self) -> TransportErrorKind {
        self.kind
    }
}

/// A request-level error: the problem details body of an HTTP error
/// response (RFC 8620 section 3.6.1, RFC 7807 section 3.1).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ProblemDetails {
    /// The problem type, a URI; `about:blank` when the server gave none
    /// (RFC 7807 section 4.2).
    #[serde(rename = "type", default = "about_blank")]
    pub problem_type: String,
    /// The HTTP status code, as the body states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// A short summary of the problem type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// An explanation of this occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A URI for this occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// For `limit` problems, the name of the limit that was hit, such as
    /// `maxSizeRequest` (RFC 8620 section 3.6.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    /// Members this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn about_blank() -> String {
    "about:blank".to_owned()
}

/// The problem types RFC 8620 section 3.6.1 defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProblemKind {
    /// `urn:ietf:params:jmap:error:unknownCapability`: `using` named a
    /// capability the server does not support.
    UnknownCapability,
    /// `urn:ietf:params:jmap:error:notJSON`.
    NotJson,
    /// `urn:ietf:params:jmap:error:notRequest`.
    NotRequest,
    /// `urn:ietf:params:jmap:error:limit`: see [`ProblemDetails::limit`].
    Limit,
    /// Any other type; see [`ProblemDetails::problem_type`].
    Other,
}

impl ProblemDetails {
    /// Which of RFC 8620's problem types this is.
    pub fn kind(&self) -> ProblemKind {
        match self.problem_type.as_str() {
            "urn:ietf:params:jmap:error:unknownCapability" => ProblemKind::UnknownCapability,
            "urn:ietf:params:jmap:error:notJSON" => ProblemKind::NotJson,
            "urn:ietf:params:jmap:error:notRequest" => ProblemKind::NotRequest,
            "urn:ietf:params:jmap:error:limit" => ProblemKind::Limit,
            _ => ProblemKind::Other,
        }
    }
}

impl fmt::Display for ProblemDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.problem_type)?;
        if let Some(limit) = &self.limit {
            write!(f, " ({limit})")?;
        }
        if let Some(status) = self.status {
            write!(f, ", HTTP {status}")?;
        }
        for text in [&self.title, &self.detail].into_iter().flatten() {
            write!(f, ": {text}")?;
        }
        Ok(())
    }
}

/// Declares an open set of error type strings: the known ones as
/// variants, any other kept as `Other`.
macro_rules! error_types {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $text:literal,)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $($(#[$variant_meta])* $variant,)*
            /// A type this crate does not know, as the server gave it.
            Other(String),
        }

        impl $name {
            /// The type as it is written in JSON.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)*
                    Self::Other(other) => other,
                }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $($text => Self::$variant,)*
                    other => Self::Other(other.to_owned()),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                Ok(Self::from(String::deserialize(deserializer)?.as_str()))
            }
        }
    };
}
pub(crate) use error_types;

error_types! {
    /// The type of a method-level error (RFC 8620 sections 3.6.2 and 5).
    ///
    /// A type a client does not understand must be treated as
    /// `serverFail` (section 3.6.2); [`MethodError::is_retryable`] does.
    MethodErrorType {
        /// A server resource was temporarily unavailable; trying later
        /// may work.
        ServerUnavailable => "serverUnavailable",
        /// An unexpected error; nothing was changed.
        ServerFail => "serverFail",
        /// Some but not all changes were made; the client must
        /// resynchronise.
        ServerPartialFail => "serverPartialFail",
        /// The server does not know the method.
        UnknownMethod => "unknownMethod",
        /// An argument is missing, of the wrong type or invalid.
        InvalidArguments => "invalidArguments",
        /// A result reference did not resolve (section 3.7).
        InvalidResultReference => "invalidResultReference",
        /// The call would break an access rule.
        Forbidden => "forbidden",
        /// The `accountId` is no account.
        AccountNotFound => "accountNotFound",
        /// The account does not support the method or data type.
        AccountNotSupportedByMethod => "accountNotSupportedByMethod",
        /// The method changes state and the account is read-only.
        AccountReadOnly => "accountReadOnly",
        /// More ids or objects than the server allows in one call
        /// (`/get`, `/set`; sections 5.1 and 5.3).
        RequestTooLarge => "requestTooLarge",
        /// `ifInState` did not match the current state (section 5.3).
        StateMismatch => "stateMismatch",
        /// The server cannot say what changed since the given state
        /// (`/changes`, `/queryChanges`; sections 5.2 and 5.6).
        CannotCalculateChanges => "cannotCalculateChanges",
        /// More changes than `maxChanges` (`/queryChanges`, section 5.6).
        TooManyChanges => "tooManyChanges",
        /// The `anchor` is not in the results (`/query`, section 5.5).
        AnchorNotFound => "anchorNotFound",
        /// The server cannot sort that way (`/query`).
        UnsupportedSort => "unsupportedSort",
        /// The server cannot filter that way (`/query`).
        UnsupportedFilter => "unsupportedFilter",
        /// `fromAccountId` is no account (`/copy`, section 5.4).
        FromAccountNotFound => "fromAccountNotFound",
        /// The account copied from does not support the data type
        /// (`/copy`).
        FromAccountNotSupportedByMethod => "fromAccountNotSupportedByMethod",
    }
}

/// A method-level error: the arguments of an `error` response (RFC 8620
/// section 3.6.2).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MethodError {
    /// The error's type.
    #[serde(rename = "type")]
    pub error_type: MethodErrorType,
    /// The server's explanation, for debugging rather than for users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl MethodError {
    /// Whether the same call may work later: `serverUnavailable` only.
    /// A type this crate does not know counts as `serverFail`, which is
    /// expected to fail again (RFC 8620 section 3.6.2).
    pub fn is_retryable(&self) -> bool {
        self.error_type == MethodErrorType::ServerUnavailable
    }
}

impl fmt::Display for MethodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the JMAP server refused the method call: ")?;
        match &self.description {
            Some(description) => write!(f, "{} ({description})", self.error_type),
            None => write!(f, "{}", self.error_type),
        }
    }
}

impl std::error::Error for MethodError {}

error_types! {
    /// The type of a [`SetError`]: those of RFC 8620 section 5.3, and
    /// those RFC 8621 adds for mail (sections 2.5, 4.6, 6.3 and 7.5).
    SetErrorType {
        /// The change would break an access rule.
        Forbidden => "forbidden",
        /// It would go over a quota.
        OverQuota => "overQuota",
        /// The record would be larger than the server allows.
        TooLarge => "tooLarge",
        /// Too many records of this type were made recently.
        RateLimit => "rateLimit",
        /// The id is not a record.
        NotFound => "notFound",
        /// The patch is invalid.
        InvalidPatch => "invalidPatch",
        /// The same request destroys the record.
        WillDestroy => "willDestroy",
        /// The record is invalid; see [`SetError::properties`].
        InvalidProperties => "invalidProperties",
        /// The type has exactly one record.
        Singleton => "singleton",
        /// A record like it exists already (`/copy`, and RFC 8621's
        /// `Email/import`).
        AlreadyExists => "alreadyExists",
        /// The mailbox has child mailboxes (RFC 8621 section 2.5).
        MailboxHasChild => "mailboxHasChild",
        /// The mailbox has emails (RFC 8621 section 2.5).
        MailboxHasEmail => "mailboxHasEmail",
        /// A blob the email refers to was not found (RFC 8621 section
        /// 4.6).
        BlobNotFound => "blobNotFound",
        /// Too many keywords (RFC 8621 section 4.6).
        TooManyKeywords => "tooManyKeywords",
        /// Too many mailboxes (RFC 8621 section 4.6).
        TooManyMailboxes => "tooManyMailboxes",
        /// The user may not send from that address (RFC 8621 sections
        /// 6.3 and 7.5).
        ForbiddenFrom => "forbiddenFrom",
        /// The email is not valid for sending (RFC 8621 section 7.5).
        InvalidEmail => "invalidEmail",
        /// More recipients than the server allows (RFC 8621 section
        /// 7.5).
        TooManyRecipients => "tooManyRecipients",
        /// The envelope has no recipients (RFC 8621 section 7.5).
        NoRecipients => "noRecipients",
        /// Some recipient addresses are invalid (RFC 8621 section 7.5).
        InvalidRecipients => "invalidRecipients",
        /// The server refuses the envelope's sender (RFC 8621 section
        /// 7.5).
        ForbiddenMailFrom => "forbiddenMailFrom",
        /// The user may not send at all (RFC 8621 section 7.5).
        ForbiddenToSend => "forbiddenToSend",
        /// The submission can no longer be cancelled (RFC 8621 section
        /// 7.5).
        CannotUnsend => "cannotUnsend",
    }
}

/// Why one record of a `/set` was not created, updated or destroyed
/// (RFC 8620 section 5.3).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SetError {
    /// The error's type.
    #[serde(rename = "type")]
    pub error_type: SetErrorType,
    /// The server's explanation, for debugging rather than for users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// For `invalidProperties`, the properties that are wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<String>>,
    /// Properties this crate has no field for, such as
    /// `invalidRecipients`, `maxRecipients` or `existingId`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.description {
            Some(description) => write!(f, "{} ({description})", self.error_type),
            None => write!(f, "{}", self.error_type),
        }
    }
}

impl std::error::Error for SetError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The two examples of RFC 8620 section 3.6.1.1.
    #[test]
    fn problem_details_of_rfc_8620_section_3_6_1_1_decode() {
        let unknown: ProblemDetails = serde_json::from_value(json!({
            "type": "urn:ietf:params:jmap:error:unknownCapability",
            "status": 400,
            "detail": "The Request object used capability 'https://example.com/apis/foobar', \
                       which is not supported by this server."
        }))
        .expect("decodes");
        assert_eq!(unknown.kind(), ProblemKind::UnknownCapability);
        assert_eq!(unknown.status, Some(400));
        assert!(unknown.to_string().contains("foobar"), "{unknown}");

        let limit: ProblemDetails = serde_json::from_value(json!({
            "type": "urn:ietf:params:jmap:error:limit",
            "limit": "maxSizeRequest",
            "status": 400,
            "detail": "The request is larger than the server is willing to process."
        }))
        .expect("decodes");
        assert_eq!(limit.kind(), ProblemKind::Limit);
        assert_eq!(limit.limit.as_deref(), Some("maxSizeRequest"));
    }

    /// RFC 7807 section 4.2: a missing type is `about:blank`; extension
    /// members are kept.
    #[test]
    fn problem_details_keep_what_they_do_not_know() {
        let problem: ProblemDetails =
            serde_json::from_value(json!({"title": "Nope", "requestId": "r-17"})).expect("decodes");
        assert_eq!(problem.problem_type, "about:blank");
        assert_eq!(problem.kind(), ProblemKind::Other);
        assert_eq!(problem.extra["requestId"], "r-17");
    }

    /// The `error` response of RFC 8620 section 3.6.2, and a type from a
    /// later specification.
    #[test]
    fn method_errors_decode_and_keep_unknown_types() {
        let error: MethodError =
            serde_json::from_value(json!({"type": "unknownMethod"})).expect("decodes");
        assert_eq!(error.error_type, MethodErrorType::UnknownMethod);
        assert!(!error.is_retryable());

        let new: MethodError = serde_json::from_value(json!({
            "type": "somethingNew", "description": "from a later RFC", "retryAfter": 30
        }))
        .expect("decodes");
        assert_eq!(
            new.error_type,
            MethodErrorType::Other("somethingNew".to_owned())
        );
        assert_eq!(new.error_type.as_str(), "somethingNew");
        assert_eq!(new.extra["retryAfter"], 30);
        assert!(!new.is_retryable(), "unknown types count as serverFail");
        assert_eq!(
            serde_json::to_value(&new).expect("encodes")["type"],
            "somethingNew"
        );
    }

    /// SetErrors as RFC 8620 section 5.3 and RFC 8621 section 7.5 give
    /// them.
    #[test]
    fn set_errors_decode_with_their_extra_properties() {
        let invalid: SetError = serde_json::from_value(json!({
            "type": "invalidProperties", "properties": ["name", "email"]
        }))
        .expect("decodes");
        assert_eq!(invalid.error_type, SetErrorType::InvalidProperties);
        assert_eq!(
            invalid.properties.as_deref(),
            Some(&["name".to_owned(), "email".to_owned()][..])
        );

        let recipients: SetError = serde_json::from_value(json!({
            "type": "invalidRecipients", "invalidRecipients": ["ada@example"]
        }))
        .expect("decodes");
        assert_eq!(recipients.error_type, SetErrorType::InvalidRecipients);
        assert_eq!(
            recipients.extra["invalidRecipients"],
            json!(["ada@example"])
        );

        let vendor: SetError =
            serde_json::from_value(json!({"type": "vendorSpecific"})).expect("decodes");
        assert_eq!(
            vendor.error_type,
            SetErrorType::Other("vendorSpecific".to_owned())
        );
        assert_eq!(vendor.to_string(), "vendorSpecific");
    }
}
