//! The client: a session, and requests sent to its API endpoint.

use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use url::Url;

use crate::{
    auth::{Credentials, TokenRequest},
    batch::IdBatches,
    error::{Error, ProblemDetails, RedirectRefusal, TransportError},
    method::{Get, GetObject, GetResponse, Method},
    request::Request,
    response::Response,
    session::Session,
    transport::{
        BodyStream, HttpMethod, HttpRequest, HttpResponse, HttpTransport, RedirectAttempt,
        RedirectPolicy, StreamRequest, StreamResponse, StreamingTransport, without_query,
    },
};

/// How long an exchange may take unless [`ClientBuilder::timeout`] says
/// otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest API response read unless
/// [`ClientBuilder::max_response_bytes`] says otherwise: 64 MiB, counted
/// after any decompression.
///
/// It is there so that a server, or whoever answers in its place, cannot
/// make a client hold an answer without bound; it is not a limit a
/// well-behaved exchange should meet. The largest answers are those of
/// an `Email/get` with body values, which grow with the number of ids
/// asked for: [`IdBatches`](crate::IdBatches) asks for fewer when an
/// answer is over the limit, so that only a single record larger than
/// the limit cannot be read. Raise it for accounts with such mail, or
/// bound the answers with `maxBodyValueBytes`.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// How long a blob upload or download may take unless
/// [`ClientBuilder::blob_timeout`] says otherwise: five minutes. Blobs
/// are attachments and whole messages, which run to tens of megabytes;
/// the API timeout would cut them off on a slow line.
pub const DEFAULT_BLOB_TIMEOUT: Duration = Duration::from_secs(300);
/// The largest blob [`Client::download`] reads unless
/// [`ClientBuilder::max_download_bytes`] or the download itself
/// ([`BlobDownload::max_bytes`](crate::BlobDownload::max_bytes)) says
/// otherwise: 128 MiB, counted after any decompression. A download is
/// held in memory whole, so this is also the most memory one download
/// takes.
pub const DEFAULT_MAX_DOWNLOAD_BYTES: usize = 128 * 1024 * 1024;
/// The largest answer to an upload that is read: 1 MiB. The answer is a
/// small JSON object (RFC 8620 section 6.1).
pub(crate) const MAX_UPLOAD_ANSWER_BYTES: usize = 1024 * 1024;
/// The largest Session object read: 4 MiB.
pub const DEFAULT_MAX_SESSION_BYTES: usize = 4 * 1024 * 1024;
/// The `User-Agent` sent unless [`ClientBuilder::user_agent`] says
/// otherwise.
pub const DEFAULT_USER_AGENT: &str = concat!("jmap-wire/", env!("CARGO_PKG_VERSION"));

/// Everything a client is set up with but its transport and session.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) credentials: Credentials,
    redirects: RedirectPolicy,
    pub(crate) timeout: Duration,
    pub(crate) max_response_bytes: usize,
    pub(crate) blob_timeout: Duration,
    pub(crate) max_download_bytes: usize,
    pub(crate) user_agent: Option<String>,
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
                blob_timeout: DEFAULT_BLOB_TIMEOUT,
                max_download_bytes: DEFAULT_MAX_DOWNLOAD_BYTES,
                user_agent: Some(DEFAULT_USER_AGENT.to_owned()),
            },
        }
    }

    /// The credentials every request is sent with: to the session URL,
    /// to the endpoints the session names (the API, upload and download
    /// URLs, which a session may put on other hosts than its own), and
    /// to any redirect the [`RedirectPolicy`] allows.
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
    /// transport error ([`Error::is_response_too_large`]) instead of
    /// being held in memory. The limit is on the body as received and,
    /// when it is compressed, on what it expands to as well, so a small
    /// compressed answer cannot expand without bound.
    pub fn max_response_bytes(mut self, bytes: usize) -> Self {
        self.config.max_response_bytes = bytes;
        self
    }

    /// How long a blob upload or download may take, connecting and
    /// reading the body included; [`DEFAULT_BLOB_TIMEOUT`] unless set.
    /// It is separate from [`timeout`](Self::timeout) because a blob
    /// takes as long as its size and the line allow, and an API call
    /// that takes minutes has gone wrong.
    pub fn blob_timeout(mut self, timeout: Duration) -> Self {
        self.config.blob_timeout = timeout;
        self
    }

    /// The largest blob [`Client::download`] reads;
    /// [`DEFAULT_MAX_DOWNLOAD_BYTES`] unless set. A larger one fails
    /// with a [`TooLarge`](crate::TransportErrorKind::TooLarge)
    /// transport error instead of being held in memory. It is separate
    /// from [`max_response_bytes`](Self::max_response_bytes), which
    /// bounds API answers, and one download can set its own
    /// ([`BlobDownload::max_bytes`](crate::BlobDownload::max_bytes)).
    pub fn max_download_bytes(mut self, bytes: usize) -> Self {
        self.config.max_download_bytes = bytes;
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
    pub(crate) transport: T,
    pub(crate) config: Config,
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
    /// [`Error::Session`] when the session names none. This is how an
    /// account is chosen (see [`Session::primary_account`]): ask for
    /// the capability whose methods you are about to call, and pass the
    /// id to them. Any other account of the session can be passed
    /// instead.
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
    /// for a redirect that is not followed, [`Error::Json`] when the
    /// body is not a Response object, and
    /// [`Error::InvalidResultReference`], before anything is sent, for
    /// a result reference that cannot resolve
    /// ([`Request::check_references`]).
    pub async fn send(&self, request: &Request) -> Result<Response, Error> {
        request.check_references()?;
        let body = serde_json::to_vec(request).map_err(Error::Encode)?;
        let response = exchange(
            &self.transport,
            &self.config,
            Exchange::json(
                HttpMethod::Post,
                self.api_url.clone(),
                body,
                self.config.timeout,
                self.config.max_response_bytes,
            ),
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
        self.send(&request).await?.take(&call)
    }

    /// Fetches the records `ids` with as many `/get` calls as it takes,
    /// one request each: `get` makes the call for a batch of ids, and
    /// `batches` says how many ids a call may have, halving when the
    /// server refuses a call for its size or the answer is over the
    /// response size limit ([`IdBatches`]). The answer is the calls'
    /// answers put together, with the state of the last.
    ///
    /// ```no_run
    /// use jmap_wire::{Get, IdBatches, mail::{Email, EmailProperty}};
    ///
    /// # async fn example(
    /// #     client: &jmap_wire::Client<impl jmap_wire::HttpTransport>,
    /// #     ids: Vec<String>,
    /// # ) -> Result<(), jmap_wire::Error> {
    /// let account = client.primary_account(jmap_wire::capability::MAIL)?;
    /// // Emails with their bodies are large: no more than 50 per call.
    /// let mut batches = IdBatches::for_session(client.session()).at_most(50);
    /// let emails = client
    ///     .get_in_batches(&ids, &mut batches, |batch| {
    ///         Get::<Email>::new(account)
    ///             .ids(batch)
    ///             .properties([EmailProperty::Subject, EmailProperty::TextBody,
    ///                          EmailProperty::BodyValues])
    ///             .fetch_text_body_values(true)
    ///     })
    ///     .await?;
    /// println!("{} of {} emails", emails.list.len(), ids.len());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// The first error that is not answered by asking for fewer; a
    /// batch that fails for its size with a single id fails with that
    /// error.
    pub async fn get_in_batches<O: GetObject>(
        &self,
        ids: &[String],
        batches: &mut IdBatches,
        mut get: impl FnMut(&[String]) -> Get<O>,
    ) -> Result<GetResponse<O::Record>, Error> {
        let mut all = GetResponse::default();
        let mut pending = ids;
        while !pending.is_empty() {
            let batch = batches.next(pending);
            match self.call(get(batch)).await {
                Ok(answer) => {
                    pending = &pending[batch.len()..];
                    all.account_id = answer.account_id.or(all.account_id);
                    all.state = answer.state.or(all.state);
                    all.list.extend(answer.list);
                    all.not_found.extend(answer.not_found);
                }
                Err(error) if batches.shrink(batch.len(), &error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(all)
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
        Exchange::json(
            HttpMethod::Get,
            session_url,
            Vec::new(),
            config.timeout,
            DEFAULT_MAX_SESSION_BYTES,
        ),
    )
    .await?;
    let session = serde_json::from_slice(&success(response)?.body)
        .map_err(|source| Error::json("session", source))?;
    Ok((session, found_at))
}

/// One HTTP exchange for [`exchange`] to make: an API call, the session,
/// or a blob.
pub(crate) struct Exchange<'a> {
    pub(crate) method: HttpMethod,
    /// Where it begins; redirects are judged against this.
    pub(crate) url: Url,
    pub(crate) body: Vec<u8>,
    /// The `Content-Type` of the body, for a POST.
    pub(crate) content_type: Option<&'a str>,
    pub(crate) accept: &'a str,
    /// Whether to ask for a gzip-compressed answer. A compressed answer
    /// is decompressed either way.
    #[cfg_attr(not(feature = "gzip"), allow(dead_code))]
    pub(crate) accept_gzip: bool,
    pub(crate) timeout: Duration,
    /// The largest body to read, counted after decompression.
    pub(crate) max_response_bytes: usize,
    /// Follow a redirect the policy does not allow all the same, from
    /// there on without credentials.
    pub(crate) redirects_without_credentials: bool,
    /// Refuse an answer in a content encoding the client cannot undo,
    /// instead of handing its bytes on as if they were the content.
    pub(crate) exact_content: bool,
    /// Headers beside the ones every exchange has, names in lower case.
    pub(crate) extra_headers: Vec<(String, String)>,
}

impl Exchange<'static> {
    /// An exchange of JSON: the session, or an API request.
    pub(crate) fn json(
        method: HttpMethod,
        url: Url,
        body: Vec<u8>,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Self {
        Self {
            method,
            url,
            content_type: (method == HttpMethod::Post).then_some("application/json"),
            body,
            accept: "application/json",
            accept_gzip: true,
            timeout,
            max_response_bytes,
            redirects_without_credentials: false,
            exact_content: false,
            extra_headers: Vec::new(),
        }
    }
}

/// What the redirect, 401 and credential rules are decided on: the
/// status and the headers of a response, whether its body is at hand
/// or still to read.
trait Head {
    fn status(&self) -> u16;
    fn header(&self, name: &str) -> Option<&str>;
}

impl Head for HttpResponse {
    fn status(&self) -> u16 {
        self.status
    }

    fn header(&self, name: &str) -> Option<&str> {
        HttpResponse::header(self, name)
    }
}

impl<B> Head for StreamResponse<B> {
    fn status(&self) -> u16 {
        self.status
    }

    fn header(&self, name: &str) -> Option<&str> {
        StreamResponse::header(self, name)
    }
}

/// One hop of an exchange: where it goes and the headers it carries.
struct Hop {
    url: Url,
    headers: Vec<(String, String)>,
}

/// One authenticated exchange with its redirects: the response that is
/// not a redirect, and the URL that gave it.
pub(crate) async fn exchange<T: HttpTransport>(
    transport: &T,
    config: &Config,
    exchange: Exchange<'_>,
) -> Result<(HttpResponse, Url), Error> {
    let (response, url) = negotiate(config, &exchange, |hop| {
        transport.execute(HttpRequest {
            method: exchange.method,
            url: hop.url,
            headers: hop.headers,
            body: exchange.body.clone(),
            timeout: exchange.timeout,
            max_response_bytes: exchange.max_response_bytes,
        })
    })
    .await?;
    #[cfg(feature = "gzip")]
    let response = gzip::decoded(response, exchange.max_response_bytes)?;
    if exchange.exact_content {
        refuse_encoded(response.header("content-encoding"))?;
    }
    Ok((response, url))
}

/// As [`exchange`], for a response that is read as it arrives: the
/// same redirects, the same 401 retry and the same rule for who sees
/// the credentials decide which response it is, on its status and
/// headers alone, and its body is handed over unread. The exchange's
/// `timeout` is for the response to begin; its size limit and gzip do
/// not apply, and a body in a content encoding is refused.
pub(crate) async fn open<T: StreamingTransport>(
    transport: &T,
    config: &Config,
    exchange: Exchange<'_>,
) -> Result<(StreamResponse<T::Body>, Url), Error> {
    let (response, url) = negotiate(config, &exchange, |hop| {
        transport.open(StreamRequest {
            method: exchange.method,
            url: hop.url,
            headers: hop.headers,
            body: exchange.body.clone(),
            response_timeout: exchange.timeout,
        })
    })
    .await?;
    let encoded = response
        .header("content-encoding")
        .map(str::trim)
        .is_some_and(|encoding| !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity"));
    if encoded && (200..300).contains(&response.status) {
        return Err(TransportError::new(
            crate::error::TransportErrorKind::Other,
            "the response is in a content encoding, which a streamed response cannot be read in",
        )
        .into());
    }
    Ok((response, url))
}

/// The most of an error response's body that is read to find problem
/// details in it.
const MAX_REFUSAL_BYTES: usize = 64 * 1024;

/// The streamed response when its status is a success; otherwise the
/// error it stands for, as [`success`] gives it, read from the start of
/// its body.
pub(crate) async fn stream_success<B: BodyStream>(
    mut response: StreamResponse<B>,
    idle: Duration,
) -> Result<StreamResponse<B>, Error> {
    if (200..300).contains(&response.status) {
        return Ok(response);
    }
    let mut body = Vec::new();
    // What cannot be read of the body costs the detail, not the status.
    while body.len() < MAX_REFUSAL_BYTES
        && let Ok(Some(chunk)) = response.body.chunk(Some(idle)).await
    {
        body.extend_from_slice(&chunk);
    }
    body.truncate(MAX_REFUSAL_BYTES);
    match success(HttpResponse::new(response.status, response.headers, body)) {
        Err(error) => Err(error),
        Ok(response) => Err(Error::Http {
            status: response.status,
        }),
    }
}

/// The hops of one exchange: sends it with `send`, again with a fresh
/// token after a 401 when the credentials can refresh, and again to
/// wherever an allowed redirect leads, and returns the first response
/// that is neither with the URL that gave it.
async fn negotiate<R, F, Fut>(
    config: &Config,
    exchange: &Exchange<'_>,
    send: F,
) -> Result<(R, Url), Error>
where
    R: Head,
    F: Fn(Hop) -> Fut,
    Fut: Future<Output = Result<R, TransportError>>,
{
    let origin = exchange.url.clone();
    let method = exchange.method;
    let mut url = origin.clone();
    let mut token = TokenRequest::Current;
    let mut redirects = 0;
    // Whether this hop is one the credentials may go to.
    let mut credentialed = true;
    loop {
        let mut headers = vec![("accept".to_owned(), exchange.accept.to_owned())];
        if let Some(content_type) = exchange.content_type {
            headers.push(("content-type".to_owned(), content_type.to_owned()));
        }
        #[cfg(feature = "gzip")]
        if exchange.accept_gzip {
            headers.push(("accept-encoding".to_owned(), "gzip".to_owned()));
        }
        headers.extend(exchange.extra_headers.iter().cloned());
        if let Some(user_agent) = &config.user_agent {
            headers.push(("user-agent".to_owned(), user_agent.clone()));
        }
        if credentialed && let Some(authorization) = config.credentials.authorization(token).await?
        {
            headers.push(("authorization".to_owned(), authorization));
        }
        let response = send(Hop {
            url: url.clone(),
            headers,
        })
        .await?;

        // A token the server no longer takes: one fresh one, one more
        // attempt.
        if response.status() == 401
            && credentialed
            && token == TokenRequest::Current
            && config.credentials.can_refresh()
        {
            token = TokenRequest::Refresh;
            continue;
        }

        if !matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
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
        if method == HttpMethod::Post && !matches!(response.status(), 307 | 308) {
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
        if !credentialed || !config.redirects.allows(&attempt) {
            if !exchange.redirects_without_credentials || method != HttpMethod::Get {
                return Err(refuse(&next, RedirectRefusal::Policy));
            }
            // Followed, but from here on as anyone would fetch it: the
            // credentials stay with the hosts the policy allows, even
            // if a later hop leads back to one.
            credentialed = false;
        }
        url = next;
    }
}

/// An error for a response whose body is still in a content encoding:
/// one that is not gzip, or gzip without the `gzip` feature. Its bytes
/// are not the content.
fn refuse_encoded(content_encoding: Option<&str>) -> Result<(), TransportError> {
    let Some(encoding) = content_encoding
        .map(str::trim)
        .filter(|encoding| !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity"))
    else {
        return Ok(());
    };
    // A transport whose HTTP library undid gzip already may leave the
    // header in place (see `gzip::decoded`).
    let gzip = encoding.eq_ignore_ascii_case("gzip") || encoding.eq_ignore_ascii_case("x-gzip");
    if gzip && cfg!(feature = "gzip") {
        return Ok(());
    }
    Err(TransportError::new(
        crate::error::TransportErrorKind::Other,
        "the response is in a content encoding this client does not read",
    ))
}

/// The response when its status is a success; otherwise the error it
/// stands for: problem details when the body is one (RFC 8620 section
/// 3.6.1), else the status alone.
pub(crate) fn success(response: HttpResponse) -> Result<HttpResponse, Error> {
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

/// Response bodies sent with `Content-Encoding: gzip` (RFC 9110 section
/// 8.4.1.3), which the client asks for with `Accept-Encoding: gzip`.
#[cfg(feature = "gzip")]
mod gzip {
    use std::io::Read;

    use crate::{
        error::{TransportError, TransportErrorKind},
        transport::HttpResponse,
    };

    /// `response` with its body decompressed, when its headers say it is
    /// gzip and it is (a transport whose HTTP library decompressed it
    /// already may leave the header in place). At most `limit` bytes
    /// are produced: a body that expands to more is
    /// [`TransportError::too_large`], however small it was.
    pub(super) fn decoded(
        mut response: HttpResponse,
        limit: usize,
    ) -> Result<HttpResponse, TransportError> {
        let gzipped = response.header("content-encoding").is_some_and(|encoding| {
            let encoding = encoding.trim();
            encoding.eq_ignore_ascii_case("gzip") || encoding.eq_ignore_ascii_case("x-gzip")
        });
        if !gzipped || !response.body.starts_with(&[0x1f, 0x8b]) {
            return Ok(response);
        }
        let mut body = Vec::new();
        // One byte over the limit shows there is more than the limit.
        let read = flate2::read::MultiGzDecoder::new(response.body.as_slice())
            .take(limit as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| {
                TransportError::new(
                    TransportErrorKind::Other,
                    "the response's gzip encoding could not be read",
                )
                .with_source(error)
            })?;
        if read > limit {
            return Err(TransportError::too_large(limit));
        }
        response.body = body;
        response.headers.retain(|(name, _)| {
            !name.eq_ignore_ascii_case("content-encoding")
                && !name.eq_ignore_ascii_case("content-length")
        });
        Ok(response)
    }
}
