//! HTTPS fetches for the autoconfig, ISPDB and JMAP steps.
//!
//! [`HttpsFetcher`] is the only network client discovery uses for HTTP, and
//! it is deliberately narrow:
//!
//! - HTTPS only, redirects included, and at most [`MAX_REDIRECTS`] of them.
//! - A [`TIMEOUT`] on the whole request and a [`MAX_BODY_BYTES`] cap on the
//!   body, so a slow or huge response can't stall or exhaust the client.
//! - No credentials, no cookies, and no query string with the address: the
//!   files discovery reads are per domain.

use std::{io::Read as _, time::Duration};

use url::Url;

use crate::{EmailAddress, Found, JmapSettings, Source, StepFindings, StepResult};

/// The largest response body discovery reads. Autoconfig files are a few
/// KiB; anything larger is not one.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// The time limit for one request, connecting included.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The most redirects one request follows.
pub const MAX_REDIRECTS: usize = 5;

/// A fetched response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The URL that answered, after redirects.
    pub url: Url,
    /// The body, at most [`MAX_BODY_BYTES`].
    pub body: Vec<u8>,
}

/// Fetches a URL. Implementations must refuse anything but HTTPS.
pub trait Fetch: Send + Sync {
    /// GETs `url`, following HTTPS redirects. An error is a transport
    /// failure (DNS, TLS, timeout, oversized body); HTTP error statuses are
    /// returned as responses.
    fn get(&self, url: &Url) -> Result<FetchResponse, String>;
}

/// The real [`Fetch`]: a blocking reqwest client with the limits above.
pub struct HttpsFetcher {
    client: Result<reqwest::blocking::Client, String>,
}

impl HttpsFetcher {
    /// Builds the client. A build failure (no TLS roots, say) surfaces on
    /// the first fetch rather than here.
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .user_agent(concat!("zmail-autodiscover/", env!("CARGO_PKG_VERSION")))
            .referer(false)
            .build()
            .map_err(|error| format!("couldn't prepare the HTTPS client: {error}"));
        Self { client }
    }
}

impl Default for HttpsFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for HttpsFetcher {
    fn get(&self, url: &Url) -> Result<FetchResponse, String> {
        if url.scheme() != "https" {
            return Err(format!("refusing a non-HTTPS URL: {url}"));
        }
        let client = self.client.as_ref().map_err(Clone::clone)?;
        let response = client
            .get(url.clone())
            .send()
            .map_err(|error| describe_error(&error))?;
        let status = response.status().as_u16();
        let final_url = response.url().clone();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BODY_BYTES as u64)
        {
            return Err(format!("{final_url} sent more than {MAX_BODY_BYTES} bytes"));
        }
        let mut body = Vec::new();
        response
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| format!("reading {final_url} failed: {error}"))?;
        if body.len() > MAX_BODY_BYTES {
            return Err(format!("{final_url} sent more than {MAX_BODY_BYTES} bytes"));
        }
        Ok(FetchResponse {
            status,
            url: final_url,
            body,
        })
    }
}

/// A short description of a reqwest error, with its cause (reqwest's own
/// message is often just "error sending request").
fn describe_error(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// The RFC 8620 well-known JMAP URL for a domain.
pub(crate) fn well_known_jmap_url(domain: &str) -> Option<Url> {
    Url::parse(&format!("https://{domain}/.well-known/jmap")).ok()
}

/// [`crate::Step::WellKnownJmap`]: probes `https://<domain>/.well-known/jmap`
/// without credentials. A JMAP server either asks for them (401) or, rarely,
/// returns the session object; both mean JMAP lives at the URL the
/// redirects ended on.
pub(crate) fn well_known_jmap_step(email: &EmailAddress, fetch: &dyn Fetch) -> StepResult {
    let Some(url) = well_known_jmap_url(email.domain()) else {
        return Ok(StepFindings::default());
    };
    let response = fetch.get(&url)?;
    if !looks_like_jmap(&response) {
        return Ok(StepFindings::default());
    }
    Ok(StepFindings {
        jmap: Some(Found::new(
            JmapSettings {
                session_url: response.url.to_string(),
                username: email.to_string(),
            },
            Source::WellKnownJmap {
                url: url.to_string(),
            },
        )),
        ..StepFindings::default()
    })
}

/// Whether a credential-less response to `/.well-known/jmap` came from a
/// JMAP server.
fn looks_like_jmap(response: &FetchResponse) -> bool {
    match response.status {
        401 => true,
        200 => {
            let body = String::from_utf8_lossy(&response.body);
            body.trim_start().starts_with('{') && body.contains("\"urn:ietf:params:jmap:core\"")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::{FetchResponse, HttpsFetcher, looks_like_jmap};
    use crate::Fetch as _;

    fn response(status: u16, body: &str) -> FetchResponse {
        FetchResponse {
            status,
            url: Url::parse("https://api.example.com/jmap/session").expect("valid URL"),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn jmap_is_recognised_by_challenge_or_session() {
        assert!(looks_like_jmap(&response(401, "")));
        assert!(looks_like_jmap(&response(
            200,
            r#"{"capabilities":{"urn:ietf:params:jmap:core":{}}}"#
        )));
        // A website's catch-all page or a missing path is not JMAP.
        assert!(!looks_like_jmap(&response(200, "<html>Welcome</html>")));
        assert!(!looks_like_jmap(&response(404, "")));
        assert!(!looks_like_jmap(&response(
            200,
            "<html>urn:ietf:params:jmap:core</html>"
        )));
    }

    #[test]
    fn the_fetcher_refuses_cleartext() {
        let url =
            Url::parse("http://autoconfig.example.com/mail/config-v1.1.xml").expect("valid URL");
        let error = HttpsFetcher::new()
            .get(&url)
            .expect_err("http:// must be refused before any request");
        assert!(error.contains("non-HTTPS"), "{error}");
    }
}
