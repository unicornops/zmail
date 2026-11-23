//! Credentials: HTTP Basic, a bearer token, or a source of bearer tokens
//! that can refresh one the server no longer accepts.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use base64::Engine as _;

use crate::error::Error;

/// What a [`TokenSource`] is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenRequest {
    /// The token to use now; a cached one will do.
    Current,
    /// A new token: the server answered 401 to the last one.
    Refresh,
}

/// The error of a [`TokenSource`].
pub type TokenError = Box<dyn std::error::Error + Send + Sync>;

/// A source of bearer tokens, for credentials that expire (OAuth 2.0
/// access tokens, say).
///
/// The client asks for the [`Current`](TokenRequest::Current) token
/// before each request. When the server answers 401 it asks once for a
/// [`Refresh`](TokenRequest::Refresh) and sends the request again with
/// the new token; a second 401 is the request's answer.
pub trait TokenSource: Send + Sync {
    /// The bearer token, without the `Bearer ` prefix.
    fn token(
        &self,
        request: TokenRequest,
    ) -> Pin<Box<dyn Future<Output = Result<String, TokenError>> + Send + '_>>;
}

/// How requests are authenticated (RFC 8620 section 8.2 leaves the
/// scheme to the server).
///
/// `Debug` never prints a secret.
#[derive(Clone, Default)]
#[non_exhaustive]
pub enum Credentials {
    /// No `Authorization` header.
    #[default]
    None,
    /// HTTP Basic (RFC 7617).
    Basic {
        /// The user name.
        username: String,
        /// The password.
        password: String,
    },
    /// A fixed bearer token (RFC 6750).
    Bearer(String),
    /// Bearer tokens from a source that can refresh them.
    BearerSource(Arc<dyn TokenSource>),
}

impl Credentials {
    /// HTTP Basic credentials.
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::Basic {
            username: username.into(),
            password: password.into(),
        }
    }

    /// A fixed bearer token.
    pub fn bearer(token: impl Into<String>) -> Self {
        Self::Bearer(token.into())
    }

    /// Bearer tokens from `source`.
    pub fn bearer_source(source: impl TokenSource + 'static) -> Self {
        Self::BearerSource(Arc::new(source))
    }

    /// Whether a 401 is worth a second attempt with a refreshed token.
    pub(crate) fn can_refresh(&self) -> bool {
        matches!(self, Self::BearerSource(_))
    }

    /// The `Authorization` header value, if any.
    pub(crate) async fn authorization(
        &self,
        request: TokenRequest,
    ) -> Result<Option<String>, Error> {
        match self {
            Self::None => Ok(None),
            Self::Basic { username, password } => {
                let encoded = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{password}"));
                Ok(Some(format!("Basic {encoded}")))
            }
            Self::Bearer(token) => bearer(token).map(Some),
            Self::BearerSource(source) => {
                let token = source.token(request).await.map_err(Error::Token)?;
                bearer(&token).map(Some)
            }
        }
    }
}

/// `Bearer {token}`, refused when the token could not be a header value
/// (control characters would let it add headers of its own).
fn bearer(token: &str) -> Result<String, Error> {
    if token.is_empty() || !token.bytes().all(|byte| (0x20..0x7f).contains(&byte)) {
        return Err(Error::InvalidCredentials);
    }
    Ok(format!("Bearer {token}"))
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::Bearer(_) => f.write_str("Bearer(<redacted>)"),
            Self::BearerSource(_) => f.write_str("BearerSource(..)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Polls a future that never waits.
    fn now<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("the future waited"),
        }
    }

    /// RFC 7617 section 2's example: `Aladdin` and `open sesame`.
    #[test]
    fn basic_credentials_are_encoded_as_rfc_7617_has_them() {
        let header =
            now(Credentials::basic("Aladdin", "open sesame").authorization(TokenRequest::Current))
                .expect("a header");
        assert_eq!(
            header.as_deref(),
            Some("Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==")
        );
        assert_eq!(
            now(Credentials::None.authorization(TokenRequest::Current)).expect("none"),
            None
        );
    }

    #[test]
    fn a_token_that_cannot_be_a_header_is_refused() {
        let header = now(Credentials::bearer("abc.def-123").authorization(TokenRequest::Current))
            .expect("a header");
        assert_eq!(header.as_deref(), Some("Bearer abc.def-123"));
        for bad in ["token\r\nX-Evil: 1", "", "tok\u{e9}n", "a\tb"] {
            assert!(matches!(
                now(Credentials::bearer(bad).authorization(TokenRequest::Current)),
                Err(Error::InvalidCredentials)
            ));
        }
    }

    #[test]
    fn debug_output_has_no_secrets() {
        let basic = format!("{:?}", Credentials::basic("rob", "hunter2"));
        assert!(
            basic.contains("rob") && !basic.contains("hunter2"),
            "{basic}"
        );
        let bearer = format!("{:?}", Credentials::bearer("s3cret-token"));
        assert!(!bearer.contains("s3cret"), "{bearer}");
    }
}
