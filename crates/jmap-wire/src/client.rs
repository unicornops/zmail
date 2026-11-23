//! The client: a session, and requests sent to its API endpoint.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use url::Url;

use crate::{
    auth::{Credentials, TokenRequest},
    error::{Error, ProblemDetails, RedirectRefusal},
    method::Method,
    request::Request,
    response::Response,
    session::Session,
    transport::{
        HttpMethod, HttpRequest, HttpResponse, HttpTransport, RedirectAttempt, RedirectPolicy,
        without_query,
    },
};

/// How long an exchange may take unless [`ClientBuilder::timeout`] says
/// otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest API response read unless
/// [`ClientBuilder::max_response_bytes`] says otherwise: 64 MiB.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// The largest Session object read: 4 MiB.
pub const DEFAULT_MAX_SESSION_BYTES: usize = 4 * 1024 * 1024;
/// The `User-Agent` sent unless [`ClientBuilder::user_agent`] says
/// otherwise.
pub const DEFAULT_USER_AGENT: &str = concat!("jmap-wire/", env!("CARGO_PKG_VERSION"));

/// Everything a client is set up with but its transport and session.
#[derive(Debug, Clone)]
struct Config {
    credentials: Credentials,
    redirects: RedirectPolicy,
    timeout: Duration,
    max_response_bytes: usize,
    user_agent: Option<String>,
}

/// Sets a [`Client`] up: credentials, redirect policy, timeout and
/// limits. Finish with [`connect`](Self::connect),
/// [`connect_with`](Self::connect_with) or
/// [`with_session`](Self::with_session).
#[derive(Debug, Clone)]
#[must_use]
pub struct ClientBuilder {
    config: Config,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    /// A builder with no credentials and the defaults for everything
    /// else; the same as [`Client::builder`].
    pub fn new() -> Self {
        Self {
            config: Config {
                credentials: Credentials::None,
                redirects: RedirectPolicy::default(),
                timeout: DEFAULT_TIMEOUT,
                max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
                user_agent: Some(DEFAULT_USER_AGENT.to_owned()),
            },
        }
    }

    /// The credentials every request is sent with: to the session URL,
    /// to the endpoints the session names, and to any redirect the
    /// [`RedirectPolicy`] allows.
    pub fn credentials(mut self, credentials: Credentials) -> Self {
        self.config.credentials = credentials;
        self
    }

    /// Which redirects are followed;
    /// [`same_host`](RedirectPolicy::same_host) unless set.
    pub fn redirect_policy(mut self, policy: RedirectPolicy) -> Self {
        self.config.redirects = policy;
        self
    }

    /// How long each HTTP exchange may take, connecting and reading the
    /// body included; [`DEFAULT_TIMEOUT`] unless set.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.config.timeout = timeout;
        self
    }

    /// The largest API response body to read;
    /// [`DEFAULT_MAX_RESPONSE_BYTES`] unless set. A larger one fails
    /// with a [`TooLarge`](crate::TransportErrorKind::TooLarge)
    /// transport error instead of being held in memory.
    pub fn max_response_bytes(mut self, bytes: usize) -> Self {
        self.config.max_response_bytes = bytes;
        self
    }

    /// The `User-Agent` header; [`DEFAULT_USER_AGENT`] unless set, none
    /// when `None`.
    pub fn user_agent(mut self, user_agent: Option<String>) -> Self {
        self.config.user_agent = user_agent;
        self
    }

    /// Fetches the session at `session_url` with the default transport
    /// (see [`connect_with`](Self::connect_with)).
    #[cfg(feature = "reqwest")]
    pub async fn connect(self, session_url: Url) -> Result<Client<crate::ReqwestTransport>, Error> {
        self.connect_with(crate::ReqwestTransport::new()?, session_url)
            .await
    }

    /// Fetches the session at `session_url` over `transport` (RFC 8620
    /// section 2): an authenticated GET, following the redirects the
    /// policy allows. When redirected, the client's session URL is where
    /// the session was found.
    ///
    /// `session_url` is the session resource itself; for a bare domain
    /// or a `/.well-known/jmap` URL that may redirect to another host,
    /// see [`discovery::session_url`](crate::discovery::session_url).
    pub async fn connect_with<T: HttpTransport>(
        self,
        transport: T,
        session_url: Url,
    ) -> Result<Client<T>, Error> {
        let (session, session_url) = fetch_session(&transport, &self.config, session_url).await?;
        self.with_session(transport, session_url, session)
    }

    /// A client for a session that is already at hand (kept from an
    /// earlier run, or fetched by other means), without a request.
    /// `session_url` is where it came from: relative endpoint URLs
    /// resolve against it, and
    /// [`refresh_session`](Client::refresh_session) fetches it.
    pub fn with_session<T: HttpTransport>(
        self,
        transport: T,
        session_url: Url,
        session: Session,
    ) -> Result<Client<T>, Error> {
        let api_url = session.api_endpoint(&session_url)?;
        Ok(Client {
            transport,
            config: self.config,
            session_url,
            api_url,
            session,
            session_changed: AtomicBool::new(false),
        })
    }
}

/// A JMAP client: a [`Session`] and the means to send [`Request`]s to
/// its API endpoint.
///
#[cfg_attr(feature = "reqwest", doc = "```no_run")]
#[cfg_attr(not(feature = "reqwest"), doc = "```ignore")]
/// use jmap_wire::{Client, Credentials, Get, capability, mail::Identity};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::builder()
///     .credentials(Credentials::bearer("an-api-token"))
///     .connect("https://api.example.com/jmap/session".parse()?)
///     .await?;
/// let account = client.primary_account(capability::SUBMISSION)?;
/// let identities = client.call(Get::<Identity>::new(account)).await?;
/// for identity in identities.list {
///     println!("{:?} <{:?}>", identity.name, identity.email);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Client<T> {
    transport: T,
    config: Config,
    session_url: Url,
    api_url: Url,
    session: Session,
    session_changed: AtomicBool,
}

impl Client<()> {
    /// Starts setting a client up.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }
}

impl<T: HttpTransport> Client<T> {
    /// The session the client holds.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Where the session was fetched from.
    pub fn session_url(&self) -> &Url {
        &self.session_url
    }

    /// The API endpoint requests are sent to.
    pub fn api_url(&self) -> &Url {
        &self.api_url
    }

    /// The transport, for exchanges of your own over the same HTTP
    /// client.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The id of the user's primary account for a capability, or
    /// [`Error::Session`] when the session names none.
    pub fn primary_account(&self, capability: &str) -> Result<&str, Error> {
        self.session
            .primary_account(capability)
            .ok_or_else(|| Error::Session(format!("primary account for {capability}")))
    }

    /// Whether a response has reported a session state other than the
    /// session's own (RFC 8620 section 3.4): the session has changed on
    /// the server, and [`refresh_session`](Self::refresh_session) gets
    /// the new one.
    pub fn session_changed(&self) -> bool {
        self.session_changed.load(Ordering::Relaxed)
    }

    /// Fetches the session again.
    pub async fn refresh_session(&mut self) -> Result<(), Error> {
        let (session, session_url) =
            fetch_session(&self.transport, &self.config, self.session_url.clone()).await?;
        self.api_url = session.api_endpoint(&session_url)?;
        self.session = session;
        self.session_url = session_url;
        self.session_changed.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Sends a request and returns the response (RFC 8620 section 3.1).
    ///
    /// Method-level errors are in the response, one per failed call
    /// ([`Response::get`] turns one into [`Error::Method`]); the request
    /// is never sent a second time, whatever the failure, since the
    /// client cannot know which calls are safe to repeat.
    ///
    /// # Errors
    ///
    /// [`Error::Transport`] when no response arrives, [`Error::Problem`]
    /// or [`Error::Http`] for an HTTP error status, [`Error::Redirect`]
    /// for a redirect that is not followed, and [`Error::Json`] when the
    /// body is not a Response object.
    pub async fn send(&self, request: &Request) -> Result<Response, Error> {
        let body = serde_json::to_vec(request).map_err(Error::Encode)?;
        let response = exchange(
            &self.transport,
            &self.config,
            HttpMethod::Post,
            self.api_url.clone(),
            body,
            self.config.max_response_bytes,
        )
        .await?
        .0;
        let response: Response = serde_json::from_slice(&success(response)?.body)
            .map_err(|source| Error::json("response", source))?;
        if !response.session_state.is_empty() && response.session_state != self.session.state {
            self.session_changed.store(true, Ordering::Relaxed);
        }
        Ok(response)
    }

    /// Sends one typed method call in a request of its own and returns
    /// its answer.
    pub async fn call<M: Method>(&self, method: M) -> Result<M::Response, Error> {
        let mut request = Request::new();
        let call = request.call(method)?;
        self.send(&request).await?.get(&call)
    }
}

/// GETs the session, and says where it was found.
async fn fetch_session<T: HttpTransport>(
    transport: &T,
    config: &Config,
    session_url: Url,
) -> Result<(Session, Url), Error> {
    if !matches!(session_url.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl {
            url: without_query(&session_url),
            reason: "it is neither HTTP nor HTTPS".to_owned(),
        });
    }
    let (response, found_at) = exchange(
        transport,
        config,
        HttpMethod::Get,
        session_url,
        Vec::new(),
        DEFAULT_MAX_SESSION_BYTES,
    )
    .await?;
    let session = serde_json::from_slice(&success(response)?.body)
        .map_err(|source| Error::json("session", source))?;
    Ok((session, found_at))
}

/// One authenticated exchange with its redirects: the response that is
/// not a redirect, and the URL that gave it.
async fn exchange<T: HttpTransport>(
    transport: &T,
    config: &Config,
    method: HttpMethod,
    origin: Url,
    body: Vec<u8>,
    max_response_bytes: usize,
) -> Result<(HttpResponse, Url), Error> {
    let mut url = origin.clone();
    let mut token = TokenRequest::Current;
    let mut redirects = 0;
    loop {
        let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
        if method == HttpMethod::Post {
            headers.push(("content-type".to_owned(), "application/json".to_owned()));
        }
        if let Some(user_agent) = &config.user_agent {
            headers.push(("user-agent".to_owned(), user_agent.clone()));
        }
        if let Some(authorization) = config.credentials.authorization(token).await? {
            headers.push(("authorization".to_owned(), authorization));
        }
        let response = transport
            .execute(HttpRequest {
                method,
                url: url.clone(),
                headers,
                body: body.clone(),
                timeout: config.timeout,
                max_response_bytes,
            })
            .await?;

        // A token the server no longer takes: one fresh one, one more
        // attempt.
        if response.status == 401
            && token == TokenRequest::Current
            && config.credentials.can_refresh()
        {
            token = TokenRequest::Refresh;
            continue;
        }

        if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            return Ok((response, url));
        }
        let refuse = |to: &Url, reason| Error::Redirect {
            to: without_query(to),
            reason,
        };
        let location = response
            .header("location")
            .ok_or_else(|| refuse(&url, RedirectRefusal::BadLocation))?;
        let next = crate::discovery::next_hop(&url, location)?;
        if method == HttpMethod::Post && !matches!(response.status, 307 | 308) {
            return Err(refuse(&next, RedirectRefusal::MethodChange));
        }
        redirects += 1;
        if redirects > config.redirects.limit() {
            return Err(refuse(&next, RedirectRefusal::TooMany));
        }
        let attempt = RedirectAttempt {
            origin: &origin,
            from: &url,
            to: &next,
        };
        if !config.redirects.allows(&attempt) {
            return Err(refuse(&next, RedirectRefusal::Policy));
        }
        url = next;
    }
}

/// The response when its status is a success; otherwise the error it
/// stands for: problem details when the body is one (RFC 8620 section
/// 3.6.1), else the status alone.
fn success(response: HttpResponse) -> Result<HttpResponse, Error> {
    if (200..300).contains(&response.status) {
        return Ok(response);
    }
    if response.is("application/problem+json")
        && let Ok(problem) = serde_json::from_slice::<ProblemDetails>(&response.body)
    {
        return Err(Error::Problem(Box::new(problem)));
    }
    Err(Error::Http {
        status: response.status,
    })
}
