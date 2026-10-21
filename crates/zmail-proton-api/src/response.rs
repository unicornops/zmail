// Ported from go-proton-api `response.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust. API codes map to
// typed `Error` variants instead of resty middleware; the retry rules
// (`catchRetryAfter`, `catchTooManyRequests`, `catchDialError`) live in
// `retry_delay` and `manager.rs`.

//! API errors and the retry rules for Proton responses.

use std::{fmt, time::Duration};

use rand::Rng;
use reqwest::StatusCode;
use serde::Deserialize;

use tokio_rustls::rustls::{self, CertificateError};

use crate::pinning::PinMismatch;

/// A Proton API response code (the `Code` field of every JSON response).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct Code(pub i64);

impl Code {
    /// The request succeeded.
    pub const SUCCESS: Self = Self(1000);
    /// A batch request returned one response per item.
    pub const MULTI: Self = Self(1001);
    /// A request field was invalid.
    pub const INVALID_VALUE: Self = Self(2001);
    /// The `x-pm-appversion` header was missing.
    pub const APP_VERSION_MISSING: Self = Self(5001);
    /// Proton does not accept this `x-pm-appversion`.
    pub const APP_VERSION_BAD: Self = Self(5003);
    /// The username is invalid (deprecated upstream, but still sent).
    pub const USERNAME_INVALID: Self = Self(6003);
    /// The password (or the SRP proof derived from it) was wrong.
    pub const PASSWORD_WRONG: Self = Self(8002);
    /// Proton wants the user to complete human verification first.
    pub const HUMAN_VERIFICATION_REQUIRED: Self = Self(9001);
    /// The account needs a paid plan to use this client.
    pub const PAID_PLAN_REQUIRED: Self = Self(10004);
    /// The refresh token is no longer valid; the user must sign in again.
    pub const AUTH_REFRESH_TOKEN_INVALID: Self = Self(10013);
    /// The human verification token was not accepted.
    pub const HUMAN_VALIDATION_INVALID_TOKEN: Self = Self(12087);
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An error response from the API (go-proton-api's `APIError`).
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    /// The HTTP status of the response.
    pub status: u16,
    /// The API code (0 when the body was not a Proton error).
    pub code: Code,
    /// The human-readable message from the `Error` field.
    pub message: String,
    /// Request-specific details, as sent.
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    /// Parses an error body, falling back to the HTTP status text when the
    /// body is not a Proton error (go-proton-api's `catchAPIError`).
    pub(crate) fn from_response(status: StatusCode, body: &[u8]) -> Self {
        #[derive(Deserialize)]
        struct Body {
            #[serde(rename = "Code")]
            code: Code,
            #[serde(rename = "Error", default)]
            error: String,
            #[serde(rename = "Details", default)]
            details: Option<serde_json::Value>,
        }

        match serde_json::from_slice::<Body>(body) {
            Ok(parsed) => Self {
                status: status.as_u16(),
                code: parsed.code,
                message: parsed.error,
                details: parsed.details,
            },
            Err(_) => Self {
                status: status.as_u16(),
                code: Code(0),
                message: status
                    .canonical_reason()
                    .unwrap_or("unexpected response")
                    .to_owned(),
                details: None,
            },
        }
    }

    /// Returns the details as text, or an empty string.
    pub fn details_to_string(&self) -> String {
        self.details
            .as_ref()
            .map(serde_json::Value::to_string)
            .unwrap_or_default()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (Code={}, Status={})",
            self.message, self.code, self.status
        )
    }
}

/// Errors returned by the Proton API client.
///
/// The API codes that change what Zmail does next have their own variants;
/// every other error response is [`Error::Api`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Proton refused this app version (codes 5001 and 5003). Zmail sends
    /// `zmail@<version>`; switching identity is not a fallback (ADR 0004).
    #[error("Proton did not accept this version of Zmail: {0}")]
    AppVersion(ApiError),
    /// Wrong password (code 8002).
    #[error("wrong Proton password: {0}")]
    PasswordWrong(ApiError),
    /// Human verification is required (code 9001); the details carry the
    /// verification methods and token.
    #[error("Proton requires human verification: {0}")]
    HumanVerificationRequired(ApiError),
    /// The human verification token was rejected (code 12087).
    #[error("Proton rejected the human verification: {0}")]
    HumanVerificationInvalid(ApiError),
    /// The account needs a paid Proton plan (code 10004).
    #[error("a paid Proton plan is required: {0}")]
    PaidPlanRequired(ApiError),
    /// The refresh token is invalid (code 10013); the user must sign in.
    #[error("the Proton session has ended: {0}")]
    RefreshTokenInvalid(ApiError),
    /// The session was signed out, either here or after a failed refresh
    /// that Proton rejected (go-proton-api's de-auth).
    #[error("the Proton session was signed out")]
    Deauthenticated(#[source] Box<Error>),
    /// Any other API error response.
    #[error("Proton API error: {0}")]
    Api(ApiError),
    /// The server's TLS key is not one of the pinned keys.
    #[error("Proton's TLS certificate did not match the pinned keys")]
    PinMismatch(#[source] reqwest::Error),
    /// The server's certificate failed verification (untrusted chain,
    /// wrong name or expired).
    #[error("Proton's TLS certificate is not trusted")]
    Certificate(#[source] reqwest::Error),
    /// The API could not be reached, or the connection dropped.
    #[error("could not reach Proton: {0}")]
    Network(#[source] reqwest::Error),
    /// A response body could not be decoded.
    #[error("unexpected Proton response: {0}")]
    Decode(#[from] serde_json::Error),
    /// The SRP challenge could not be answered: a bad or unsigned modulus,
    /// an invalid salt or server ephemeral, or an unsupported version.
    #[error("Proton sign-in failed: {0}")]
    Srp(#[source] proton_srp::SRPError),
    /// The server's SRP proof did not match: it does not know the
    /// password verifier, so the session is not trusted
    /// (go-proton-api's `ErrInvalidProof`).
    #[error("unexpected server proof")]
    InvalidServerProof,
    /// The account has no salt for the key (`SaltForKey`).
    #[error("no key salt found for key {0}")]
    NoSaltForKey(String),
    /// The key password did not unlock the user keys.
    #[error("the Proton mailbox password is wrong")]
    KeyPasswordWrong,
    /// The account uses two-password mode and needs its mailbox password.
    #[error("this Proton account needs its mailbox password")]
    MailboxPasswordRequired,
    /// A key could not be read, unlocked, checked or used.
    #[error("Proton key error: {0}")]
    Key(String),
    /// The client was configured with invalid values.
    #[error("invalid Proton client configuration: {0}")]
    Config(String),
}

impl Error {
    /// Maps an API error response to its typed variant.
    pub fn from_api(error: ApiError) -> Self {
        match error.code {
            Code::APP_VERSION_MISSING | Code::APP_VERSION_BAD => Self::AppVersion(error),
            Code::PASSWORD_WRONG => Self::PasswordWrong(error),
            Code::HUMAN_VERIFICATION_REQUIRED => Self::HumanVerificationRequired(error),
            Code::HUMAN_VALIDATION_INVALID_TOKEN => Self::HumanVerificationInvalid(error),
            Code::PAID_PLAN_REQUIRED => Self::PaidPlanRequired(error),
            Code::AUTH_REFRESH_TOKEN_INVALID => Self::RefreshTokenInvalid(error),
            _ => Self::Api(error),
        }
    }

    /// Classifies a transport error, telling certificate failures apart:
    /// those are never retried.
    pub(crate) fn from_transport(error: reqwest::Error) -> Self {
        match certificate_error(&error) {
            Some(CertificateError::Other(other))
                if other.0.downcast_ref::<PinMismatch>().is_some() =>
            {
                Self::PinMismatch(error)
            }
            Some(_) => Self::Certificate(error),
            None => Self::Network(error),
        }
    }

    /// Returns the API error behind this error, if it came from a response.
    pub fn api_error(&self) -> Option<&ApiError> {
        match self {
            Self::AppVersion(error)
            | Self::PasswordWrong(error)
            | Self::HumanVerificationRequired(error)
            | Self::HumanVerificationInvalid(error)
            | Self::PaidPlanRequired(error)
            | Self::RefreshTokenInvalid(error)
            | Self::Api(error) => Some(error),
            Self::Deauthenticated(inner) => inner.api_error(),
            _ => None,
        }
    }

    /// Returns the API code, if the error came from a response.
    pub fn code(&self) -> Option<Code> {
        self.api_error().map(|error| error.code)
    }

    /// Returns the HTTP status, if the error came from a response.
    pub fn status(&self) -> Option<u16> {
        self.api_error().map(|error| error.status)
    }

    /// Returns whether the API could not be reached (go-proton-api's
    /// `NetError`).
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Network(_))
    }
}

/// Finds the rustls certificate error behind a transport error, if any.
///
/// The rustls error reaches reqwest wrapped in an `io::Error`, whose
/// `source()` skips the wrapped error itself, so each layer is unwrapped
/// by hand.
fn certificate_error(error: &reqwest::Error) -> Option<&CertificateError> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(rustls::Error::InvalidCertificate(certificate)) =
            current.downcast_ref::<rustls::Error>()
        {
            return Some(certificate);
        }
        source = match current.downcast_ref::<std::io::Error>() {
            Some(io) => io
                .get_ref()
                .map(|inner| inner as &(dyn std::error::Error + 'static)),
            None => current.source(),
        };
    }
    None
}

/// When to give up on retries, and how long to wait between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryPolicy {
    /// Retries after the first attempt (resty's `SetRetryCount(3)`).
    pub(crate) max_retries: u32,
    /// Longest single wait (`SetRetryMaxWaitTime(time.Minute)`).
    pub(crate) max_wait: Duration,
    /// Wait for 429/503 without a usable `Retry-After` (10 s upstream).
    pub(crate) fallback_after: Duration,
    /// Upper bound of the random jitter added to 429/503 waits (10 s).
    pub(crate) jitter: Duration,
    /// First backoff step for connection errors (resty's 100 ms default).
    pub(crate) backoff_base: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            max_wait: Duration::from_secs(60),
            fallback_after: Duration::from_secs(10),
            jitter: Duration::from_secs(10),
            backoff_base: Duration::from_millis(100),
        }
    }
}

impl RetryPolicy {
    /// The wait before retrying a 429 or 503 response (`catchRetryAfter`):
    /// `Retry-After` seconds, or the fallback, plus jitter, capped at
    /// `max_wait`.
    pub(crate) fn retry_after_delay(&self, retry_after: Option<&str>) -> Duration {
        let after = retry_after
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(self.fallback_after);
        let jitter = random_up_to(self.jitter);
        (after + jitter).min(self.max_wait)
    }

    /// The wait before retrying after a connection error: resty's default
    /// exponential backoff with jitter, capped at `max_wait`.
    pub(crate) fn backoff_delay(&self, attempt: u32) -> Duration {
        let ceiling = self
            .backoff_base
            .saturating_mul(2u32.saturating_pow(attempt))
            .min(self.max_wait);
        let floor = self.backoff_base.min(ceiling);
        floor + random_up_to(ceiling - floor)
    }
}

/// Returns whether a response status is retried (`catchTooManyRequests`).
pub(crate) fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
}

fn random_up_to(limit: Duration) -> Duration {
    if limit.is_zero() {
        return Duration::ZERO;
    }
    let millis = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(rand::thread_rng().gen_range(0..=millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_parses_proton_body() {
        let error = ApiError::from_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            br#"{"Code":9001,"Error":"Human verification required","Details":{"HumanVerificationToken":"t","HumanVerificationMethods":["captcha"]}}"#,
        );
        assert_eq!(error.status, 422);
        assert_eq!(error.code, Code::HUMAN_VERIFICATION_REQUIRED);
        assert_eq!(error.message, "Human verification required");
        assert!(error.details_to_string().contains("HumanVerificationToken"));
        assert_eq!(
            error.to_string(),
            "Human verification required (Code=9001, Status=422)"
        );
    }

    #[test]
    fn api_error_falls_back_to_status_text() {
        let error = ApiError::from_response(StatusCode::BAD_GATEWAY, b"<html>bad gateway</html>");
        assert_eq!(error.code, Code(0));
        assert_eq!(error.status, 502);
        assert_eq!(error.message, "Bad Gateway");
    }

    #[test]
    fn codes_map_to_typed_errors() {
        let make = |code| ApiError {
            status: 422,
            code: Code(code),
            message: String::new(),
            details: None,
        };
        assert!(matches!(Error::from_api(make(5001)), Error::AppVersion(_)));
        assert!(matches!(Error::from_api(make(5003)), Error::AppVersion(_)));
        assert!(matches!(
            Error::from_api(make(8002)),
            Error::PasswordWrong(_)
        ));
        assert!(matches!(
            Error::from_api(make(9001)),
            Error::HumanVerificationRequired(_)
        ));
        assert!(matches!(
            Error::from_api(make(10004)),
            Error::PaidPlanRequired(_)
        ));
        assert!(matches!(
            Error::from_api(make(10013)),
            Error::RefreshTokenInvalid(_)
        ));
        assert!(matches!(
            Error::from_api(make(12087)),
            Error::HumanVerificationInvalid(_)
        ));
        let other = Error::from_api(make(2001));
        assert!(matches!(other, Error::Api(_)));
        assert_eq!(other.code(), Some(Code::INVALID_VALUE));
        assert_eq!(other.status(), Some(422));
    }

    #[test]
    fn retry_after_honours_header_with_jitter_and_cap() {
        let policy = RetryPolicy::default();
        for _ in 0..50 {
            let delay = policy.retry_after_delay(Some("5"));
            assert!(delay >= Duration::from_secs(5) && delay <= Duration::from_secs(15));
            let fallback = policy.retry_after_delay(None);
            assert!(fallback >= Duration::from_secs(10) && fallback <= Duration::from_secs(20));
            let garbage = policy.retry_after_delay(Some("Wed, 21 Oct 2015 07:28:00 GMT"));
            assert!(garbage >= Duration::from_secs(10) && garbage <= Duration::from_secs(20));
            assert_eq!(
                policy.retry_after_delay(Some("3600")),
                Duration::from_secs(60)
            );
        }
    }

    #[test]
    fn backoff_grows_and_stays_capped() {
        let policy = RetryPolicy::default();
        for attempt in 0..10 {
            let delay = policy.backoff_delay(attempt);
            assert!(delay >= Duration::from_millis(100));
            assert!(delay <= Duration::from_secs(60));
            let ceiling = Duration::from_millis(100).saturating_mul(1 << attempt);
            assert!(delay <= ceiling.max(Duration::from_millis(100)));
        }
    }

    #[test]
    fn only_429_and_503_are_retried() {
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
    }
}
