//! The default [`HttpTransport`], on `reqwest`.

use std::error::Error as _;

use crate::{
    error::{TransportError, TransportErrorKind},
    transport::{HttpMethod, HttpRequest, HttpResponse, HttpTransport, without_query},
};

/// An [`HttpTransport`] on a `reqwest` client: TLS by rustls, no
/// redirects (the [`Client`](crate::Client) follows them by its policy),
/// no cookies and no `Referer`.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// A transport with a `reqwest` client of its own.
    pub fn new() -> Result<Self, TransportError> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .build()
            .map(|client| Self { client })
            .map_err(|error| {
                TransportError::new(
                    TransportErrorKind::Other,
                    "the HTTP client could not be set up",
                )
                .with_source(error)
            })
    }

    /// A transport on a `reqwest` client you built, with your own
    /// proxy, root certificates or connection pool settings.
    ///
    /// Build it with `redirect(reqwest::redirect::Policy::none())`: a
    /// client that follows redirects itself goes around the
    /// [`RedirectPolicy`](crate::RedirectPolicy).
    pub fn from_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl HttpTransport for ReqwestTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let shown = without_query(&request.url);
        let limit = request.max_response_bytes;
        let mut builder = match request.method {
            HttpMethod::Get => self.client.get(request.url),
            HttpMethod::Post => self.client.post(request.url).body(request.body),
        }
        .timeout(request.timeout);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let mut response = builder
            .send()
            .await
            .map_err(|error| describe(error, &shown))?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
            })
            .collect();
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(TransportError::too_large(limit));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| describe(error, &shown))?
        {
            if body.len() + chunk.len() > limit {
                return Err(TransportError::too_large(limit));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpResponse::new(status, headers, body))
    }
}

/// A `reqwest` error as a transport error: its kind, and a message with
/// its causes (reqwest's own is often just "error sending request") and
/// the URL without its query string.
fn describe(error: reqwest::Error, url: &str) -> TransportError {
    let kind = if error.is_timeout() {
        TransportErrorKind::Timeout
    } else if error.is_connect() {
        TransportErrorKind::Connect
    } else {
        TransportErrorKind::Other
    };
    // reqwest's Display includes the full URL; the query may hold
    // secrets, so the message is built from the causes instead.
    let error = error.without_url();
    let mut message = match kind {
        TransportErrorKind::Timeout => format!("the request to {url} timed out"),
        TransportErrorKind::Connect => format!("could not connect to {url}"),
        _ => format!("the request to {url} failed"),
    };
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    TransportError::new(kind, message).with_source(error)
}
