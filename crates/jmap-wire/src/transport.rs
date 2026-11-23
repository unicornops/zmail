//! The HTTP layer, as small as the client needs it: one exchange at a
//! time ([`HttpTransport`]), and the rule for following redirects
//! ([`RedirectPolicy`]), which the client applies itself.

use std::{collections::BTreeSet, fmt, future::Future, sync::Arc, time::Duration};

use url::Url;

use crate::error::TransportError;

/// The HTTP methods a JMAP client uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpMethod {
    /// For the session, downloads and the event source.
    Get,
    /// For API requests and uploads.
    Post,
}

/// One HTTP request for an [`HttpTransport`] to make.
///
/// `Debug` prints header names without their values, and the URL
/// without its query string, as either may carry secrets.
#[derive(Clone)]
#[non_exhaustive]
pub struct HttpRequest {
    /// The method.
    pub method: HttpMethod,
    /// The URL: always `http` or `https`.
    pub url: Url,
    /// The headers, names in lower case. `authorization` is among them
    /// when the client has credentials.
    pub headers: Vec<(String, String)>,
    /// The body; empty for a GET.
    pub body: Vec<u8>,
    /// How long the whole exchange may take, connecting and reading the
    /// body included.
    pub timeout: Duration,
    /// The largest response body to accept, in bytes. A larger one is a
    /// [`TransportError::too_large`] and must not be read to its end.
    pub max_response_bytes: usize,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &without_query(&self.url))
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("body", &format_args!("{} bytes", self.body.len()))
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

/// A URL for messages and logs: no credentials, query or fragment.
pub(crate) fn without_query(url: &Url) -> String {
    let mut shown = url.clone();
    shown.set_query(None);
    shown.set_fragment(None);
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    shown.into()
}

/// What the server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// The headers. Names are matched without regard to case.
    pub headers: Vec<(String, String)>,
    /// The whole body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A response.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// The first header called `name`, whatever its case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Whether the content type is `media_type`, parameters aside.
    pub(crate) fn is(&self, media_type: &str) -> bool {
        self.header("content-type").is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case(media_type)
        })
    }
}

/// Makes HTTP exchanges for a [`Client`](crate::Client). Implement it
/// to use another HTTP library, to add a proxy or metrics, or to test
/// without a network.
///
/// An implementation must:
///
/// - **not follow redirects**: it returns a 3xx response as it is, and
///   the client decides ([`RedirectPolicy`]);
/// - keep to the request's `timeout` and `max_response_bytes`;
/// - send no headers of its own that identify the user (cookies, say),
///   and log neither header values nor bodies.
pub trait HttpTransport: Send + Sync {
    /// Sends `request` and returns the response with its whole body. An
    /// HTTP error status is a response, not an error.
    fn execute(
        &self,
        request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, TransportError>> + Send;
}

impl<T: HttpTransport + ?Sized> HttpTransport for &T {
    fn execute(
        &self,
        request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, TransportError>> + Send {
        (**self).execute(request)
    }
}

impl<T: HttpTransport + ?Sized> HttpTransport for Arc<T> {
    fn execute(
        &self,
        request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, TransportError>> + Send {
        (**self).execute(request)
    }
}

type RedirectRule = dyn Fn(&RedirectAttempt<'_>) -> bool + Send + Sync;

/// A redirect the client was asked to follow.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RedirectAttempt<'a> {
    /// The URL the exchange began at, before any redirect.
    pub origin: &'a Url,
    /// The URL that answered with the redirect.
    pub from: &'a Url,
    /// Where it leads.
    pub to: &'a Url,
}

#[derive(Clone)]
enum Rule {
    None,
    SameHost,
    SameOrigin,
    Hosts(BTreeSet<String>),
    Custom(Arc<RedirectRule>),
}

/// Which redirects a [`Client`](crate::Client) follows.
///
/// A redirect that is followed is sent the whole request again,
/// **credentials included**, so the policy is the statement of which
/// hosts may see them. The default is [`same_host`](Self::same_host).
///
/// Whatever the policy, a redirect is refused when it leads from HTTPS
/// to HTTP or away from HTTP altogether, when it would turn a POST into
/// a GET (301, 302 and 303 on an API request; 307 and 308 keep the
/// method and are followed), and after
/// [`max_redirects`](Self::max_redirects).
///
/// ```
/// use jmap_wire::RedirectPolicy;
///
/// // Only ever these two hosts, and at most two hops.
/// let policy = RedirectPolicy::hosts(["example.com", "api.example.com"]).max_redirects(2);
/// // Or a rule of your own.
/// let own = RedirectPolicy::custom(|attempt| {
///     attempt.to.host_str().is_some_and(|host| host.ends_with(".example.com"))
/// });
/// ```
#[derive(Clone)]
pub struct RedirectPolicy {
    rule: Rule,
    max_redirects: usize,
}

impl RedirectPolicy {
    /// How many redirects are followed unless
    /// [`max_redirects`](Self::max_redirects) says otherwise.
    pub const DEFAULT_MAX_REDIRECTS: usize = 5;

    fn new(rule: Rule) -> Self {
        Self {
            rule,
            max_redirects: Self::DEFAULT_MAX_REDIRECTS,
        }
    }

    /// Follow no redirect.
    pub fn none() -> Self {
        Self::new(Rule::None)
    }

    /// Follow redirects that stay on the host the exchange began at,
    /// whatever the port.
    pub fn same_host() -> Self {
        Self::new(Rule::SameHost)
    }

    /// Follow redirects that stay on the origin (scheme, host and port)
    /// the exchange began at.
    pub fn same_origin() -> Self {
        Self::new(Rule::SameOrigin)
    }

    /// Follow redirects to these hosts only, whichever host the exchange
    /// began at.
    pub fn hosts(hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::new(Rule::Hosts(
            hosts
                .into_iter()
                .map(|host| host.into().to_ascii_lowercase())
                .collect(),
        ))
    }

    /// Follow the redirects `rule` returns true for.
    pub fn custom(rule: impl Fn(&RedirectAttempt<'_>) -> bool + Send + Sync + 'static) -> Self {
        Self::new(Rule::Custom(Arc::new(rule)))
    }

    /// Follow at most `max` redirects in one exchange.
    pub fn max_redirects(mut self, max: usize) -> Self {
        self.max_redirects = max;
        self
    }

    pub(crate) fn limit(&self) -> usize {
        self.max_redirects
    }

    /// Whether the policy's own rule allows the redirect. The rules that
    /// always hold are the client's to check.
    pub fn allows(&self, attempt: &RedirectAttempt<'_>) -> bool {
        match &self.rule {
            Rule::None => false,
            Rule::SameHost => {
                attempt.to.host_str().is_some()
                    && attempt.to.host_str() == attempt.origin.host_str()
            }
            Rule::SameOrigin => attempt.to.origin() == attempt.origin.origin(),
            Rule::Hosts(hosts) => attempt
                .to
                .host_str()
                .is_some_and(|host| hosts.contains(&host.to_ascii_lowercase())),
            Rule::Custom(rule) => rule(attempt),
        }
    }
}

impl Default for RedirectPolicy {
    fn default() -> Self {
        Self::same_host()
    }
}

impl fmt::Debug for RedirectPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rule: &dyn fmt::Debug = match &self.rule {
            Rule::None => &"none",
            Rule::SameHost => &"same host",
            Rule::SameOrigin => &"same origin",
            Rule::Hosts(hosts) => hosts,
            Rule::Custom(_) => &"custom",
        };
        f.debug_struct("RedirectPolicy")
            .field("rule", rule)
            .field("max_redirects", &self.max_redirects)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a URL")
    }

    fn allows(policy: &RedirectPolicy, origin: &str, to: &str) -> bool {
        let (origin, to) = (url(origin), url(to));
        policy.allows(&RedirectAttempt {
            origin: &origin,
            from: &origin,
            to: &to,
        })
    }

    #[test]
    fn each_policy_allows_what_it_says() {
        let start = "https://example.com/.well-known/jmap";
        let same_host_other_port = "https://example.com:8443/jmap";
        let other_host = "https://api.example.com/jmap";

        let none = RedirectPolicy::none();
        assert!(!allows(&none, start, "https://example.com/jmap"));

        let same_host = RedirectPolicy::default();
        assert!(allows(&same_host, start, "https://example.com/jmap"));
        assert!(allows(&same_host, start, same_host_other_port));
        assert!(!allows(&same_host, start, other_host));

        let same_origin = RedirectPolicy::same_origin();
        assert!(allows(&same_origin, start, "https://example.com/jmap"));
        assert!(!allows(&same_origin, start, same_host_other_port));

        let hosts = RedirectPolicy::hosts(["API.example.com"]);
        assert!(allows(&hosts, start, other_host));
        assert!(!allows(&hosts, start, "https://example.com/jmap"));

        let custom = RedirectPolicy::custom(|attempt| attempt.to.path().starts_with("/jmap"));
        assert!(allows(&custom, start, other_host));
        assert!(!allows(&custom, start, "https://example.com/login"));
        assert_eq!(custom.max_redirects(2).limit(), 2);
    }

    #[test]
    fn debug_output_of_a_request_has_no_secrets() {
        let request = HttpRequest {
            method: HttpMethod::Post,
            url: url("https://user:pw@example.com/api?access_token=s3cret"),
            headers: vec![("authorization".to_owned(), "Bearer s3cret".to_owned())],
            body: b"{\"password\":\"s3cret\"}".to_vec(),
            timeout: Duration::from_secs(1),
            max_response_bytes: 10,
        };
        let shown = format!("{request:?}");
        assert!(shown.contains("authorization"), "{shown}");
        assert!(
            !shown.contains("s3cret") && !shown.contains("pw@"),
            "{shown}"
        );
    }

    #[test]
    fn response_headers_are_found_whatever_their_case() {
        let response = HttpResponse::new(
            400,
            vec![(
                "Content-Type".to_owned(),
                "Application/Problem+JSON; charset=utf-8".to_owned(),
            )],
            Vec::new(),
        );
        assert!(response.is("application/problem+json"));
        assert!(!response.is("application/json"));
        assert_eq!(response.header("LOCATION"), None);
    }
}
