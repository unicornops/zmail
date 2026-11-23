//! Finding the session resource (RFC 8620 section 2.2): from what a
//! user typed or a DNS SRV record gave, to the URL the session is
//! fetched from.

use std::time::Duration;

use url::Url;

use crate::{
    error::{Error, RedirectRefusal},
    transport::{HttpMethod, HttpRequest, HttpTransport, without_query},
};

/// The path of the session resource on a host (RFC 8620 section 2.2).
pub const WELL_KNOWN_PATH: &str = "/.well-known/jmap";

/// How the well-known URL's redirects are followed.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Discovery {
    /// How long each request may take. 10 seconds unless changed.
    pub timeout: Duration,
    /// How many redirects are followed. 5 unless changed.
    pub max_redirects: usize,
    /// The `User-Agent` to send, if any.
    pub user_agent: Option<String>,
}

impl Default for Discovery {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_redirects: 5,
            user_agent: None,
        }
    }
}

/// The session URL for an endpoint as a user or a discovery step gives
/// it, in one of three shapes:
///
/// - a bare origin (`https://example.com`), which means its
///   `/.well-known/jmap`;
/// - a `/.well-known/jmap` URL;
/// - any other URL, which is taken to be the session resource itself
///   and returned as it is, without a request.
///
/// A well-known URL commonly redirects to the session on another host
/// (`example.com` to `api.example.com`). Its redirects are followed
/// here **without credentials**, never from HTTPS to HTTP, so that the
/// caller can then connect to where they end with a redirect policy as
/// strict as [`same_host`](crate::RedirectPolicy::same_host) and the
/// credentials go to that host only. Where the redirects end is
/// returned only when it answers as a session resource does to a
/// request without credentials (401, or JSON), so a redirect to a
/// sign-in page is not taken for the session; otherwise the well-known
/// URL itself is returned.
///
/// # Errors
///
/// [`Error::InvalidUrl`] for an endpoint that is not an HTTP or HTTPS
/// URL, [`Error::Transport`] when a request fails, and
/// [`Error::Redirect`] for a redirect that is insecure, has no usable
/// `Location`, or is one too many.
pub async fn session_url<T: HttpTransport>(
    transport: &T,
    endpoint: &str,
    discovery: &Discovery,
) -> Result<Url, Error> {
    let mut url = Url::parse(endpoint).map_err(|error| Error::InvalidUrl {
        url: endpoint.to_owned(),
        reason: error.to_string(),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl {
            url: endpoint.to_owned(),
            reason: "it is neither HTTP nor HTTPS".to_owned(),
        });
    }
    url.set_fragment(None);
    if url.query().is_none() && matches!(url.path().trim_end_matches('/'), "" | WELL_KNOWN_PATH) {
        url.set_path(WELL_KNOWN_PATH);
    }
    if url.path() != WELL_KNOWN_PATH {
        return Ok(url);
    }

    let well_known = url.clone();
    for _ in 0..=discovery.max_redirects {
        let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
        if let Some(user_agent) = &discovery.user_agent {
            headers.push(("user-agent".to_owned(), user_agent.clone()));
        }
        let response = transport
            .execute(HttpRequest {
                method: HttpMethod::Get,
                url: url.clone(),
                headers,
                body: Vec::new(),
                timeout: discovery.timeout,
                max_response_bytes: crate::client::DEFAULT_MAX_SESSION_BYTES,
            })
            .await?;
        let location = response
            .header("location")
            .filter(|_| (300..400).contains(&response.status));
        let Some(location) = location else {
            let answers_as_jmap = response.status == 401
                || ((200..300).contains(&response.status) && response.is("application/json"));
            return Ok(if url == well_known || answers_as_jmap {
                url
            } else {
                well_known
            });
        };
        url = next_hop(&url, location)?;
    }
    Err(Error::Redirect {
        to: without_query(&url),
        reason: RedirectRefusal::TooMany,
    })
}

/// Where a redirect from `current` to `location` leads, refused when it
/// leaves HTTP(S) or goes from HTTPS to HTTP.
pub(crate) fn next_hop(current: &Url, location: &str) -> Result<Url, Error> {
    let mut next = current.join(location).map_err(|_| Error::Redirect {
        to: location
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .to_owned(),
        reason: RedirectRefusal::BadLocation,
    })?;
    next.set_fragment(None);
    let allowed = match next.scheme() {
        "https" => true,
        "http" => current.scheme() == "http",
        _ => false,
    };
    if !allowed {
        return Err(Error::Redirect {
            to: without_query(&next),
            reason: RedirectRefusal::Insecure,
        });
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_never_leave_https() {
        let https = Url::parse("https://example.com/.well-known/jmap").expect("a URL");
        let http = Url::parse("http://127.0.0.1:4080/.well-known/jmap").expect("a URL");
        assert_eq!(
            next_hop(&https, "https://api.example.com/jmap/session#top")
                .expect("followed")
                .as_str(),
            "https://api.example.com/jmap/session"
        );
        assert_eq!(
            next_hop(&https, "/jmap/session")
                .expect("followed")
                .as_str(),
            "https://example.com/jmap/session"
        );
        assert!(matches!(
            next_hop(&https, "http://api.example.com/jmap/session?token=s3cret"),
            Err(Error::Redirect { to, reason: RedirectRefusal::Insecure })
                if to == "http://api.example.com/jmap/session"
        ));
        assert!(next_hop(&https, "ftp://api.example.com/").is_err());
        assert!(next_hop(&http, "http://127.0.0.1:4080/jmap/session").is_ok());
        assert!(matches!(
            next_hop(&https, "https://exa mple.com/"),
            Err(Error::Redirect {
                reason: RedirectRefusal::BadLocation,
                ..
            })
        ));
    }
}
