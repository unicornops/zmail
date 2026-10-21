// Ported from go-proton-api `manager.go`, `manager_builder.go`,
// `manager_status.go` and `manager_ping.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust on reqwest. The
// resty middleware (app version header, error parsing, `updateTime`,
// connection status, error handlers, retries) became one request loop.
// The cookie jar is Zmail's own. Connection errors other than a failed
// connect are only retried for GET requests. TLS pin mismatches are never
// retried. Pre/post request hooks, the logger and the panic handler are
// not ported.

//! The API manager: one HTTP client, cookie jar and status per API host.

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicI64, Ordering},
    },
};

use chrono::{DateTime, Utc};
use reqwest::{
    Method, StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, DATE, HeaderMap, RETRY_AFTER, SET_COOKIE},
    redirect,
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    config::{CONNECT_TIMEOUT, ClientConfig, POOL_IDLE_TIMEOUT, READ_TIMEOUT},
    cookies::CookieJar,
    response::{ApiError, Code, Error, is_retryable_status},
};

/// Header carrying Zmail's identity (`zmail@<version>`).
pub(crate) const APP_VERSION_HEADER: &str = "x-pm-appversion";
/// Header carrying the session UID.
pub(crate) const UID_HEADER: &str = "x-pm-uid";

/// Whether the API is reachable (go-proton-api's `Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The last request got a response.
    Up,
    /// The last request could not reach the API.
    Down,
}

type StatusObserver = Arc<dyn Fn(Status) + Send + Sync>;
type CodeHandler = Arc<dyn Fn() + Send + Sync>;

/// Shared state for every [`Client`](crate::Client) of one API host.
///
/// Cloning is cheap; clones share the HTTP connection pool, cookie jar and
/// observers.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    config: ClientConfig,
    cookies: CookieJar,
    status: Mutex<Status>,
    observers: Mutex<Vec<StatusObserver>>,
    error_handlers: Mutex<HashMap<Code, Vec<CodeHandler>>>,
    /// Unix time from the last response's `Date` header (0 before any).
    server_time: AtomicI64,
}

impl fmt::Debug for Manager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Manager")
            .field("base_url", &self.inner.config.base_url.as_str())
            .field("app_version", &self.inner.config.app_version.as_str())
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Manager {
    /// Creates a manager for the configured API host.
    pub fn new(config: ClientConfig) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(config.tls_config()?)
            .https_only(true)
            // The API never redirects; following one could only send the
            // session somewhere else.
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .build()
            .map_err(|error| Error::Config(format!("HTTP client: {error}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                config,
                cookies: CookieJar::new(),
                // go-proton-api starts at its zero value, StatusUp.
                status: Mutex::new(Status::Up),
                observers: Mutex::new(Vec::new()),
                error_handlers: Mutex::new(HashMap::new()),
                server_time: AtomicI64::new(0),
            }),
        })
    }

    /// The configuration this manager was built with.
    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }

    /// The cookie jar shared by all requests.
    pub fn cookies(&self) -> &CookieJar {
        &self.inner.cookies
    }

    /// Calls `observer` whenever the connection status changes.
    pub fn add_status_observer(&self, observer: impl Fn(Status) + Send + Sync + 'static) {
        lock(&self.inner.observers).push(Arc::new(observer));
    }

    /// Calls `handler` whenever a response carries the API `code`, for
    /// example [`Code::APP_VERSION_BAD`].
    pub fn add_error_handler(&self, code: Code, handler: impl Fn() + Send + Sync + 'static) {
        lock(&self.inner.error_handlers)
            .entry(code)
            .or_default()
            .push(Arc::new(handler));
    }

    /// The current connection status.
    pub fn status(&self) -> Status {
        *lock(&self.inner.status)
    }

    /// The server's clock as of the last response, from its `Date` header
    /// (go-proton-api feeds this to gopenpgp's `UpdateTime`).
    pub fn server_time(&self) -> Option<DateTime<Utc>> {
        match self.inner.server_time.load(Ordering::Relaxed) {
            0 => None,
            seconds => DateTime::from_timestamp(seconds, 0),
        }
    }

    /// Checks that the API answers at all (`GET /tests/ping`); any HTTP
    /// response counts.
    pub async fn ping(&self) -> Result<(), Error> {
        match self.send(&Request::get("/tests/ping"), None).await {
            Ok(_) => Ok(()),
            Err(error) if error.api_error().is_some() => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Sends a request, retrying 429/503 responses and connection errors,
    /// and turns error responses into [`Error`]s.
    pub(crate) async fn send(
        &self,
        request: &Request,
        auth: Option<&AuthHeaders<'_>>,
    ) -> Result<Response, Error> {
        let policy = self.inner.config.retry;
        let mut attempt = 0;
        loop {
            let outcome = self.send_once(request, auth).await;
            let delay = match &outcome {
                Ok(response)
                    if is_retryable_status(response.status) && attempt < policy.max_retries =>
                {
                    Some(
                        policy.retry_after_delay(
                            response
                                .headers
                                .get(RETRY_AFTER)
                                .and_then(|value| value.to_str().ok()),
                        ),
                    )
                }
                Err(Error::Network(error))
                    if attempt < policy.max_retries
                        && (error.is_connect() || request.method == Method::GET) =>
                {
                    Some(policy.backoff_delay(attempt))
                }
                _ => None,
            };
            match delay {
                Some(delay) => {
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                None => return outcome.and_then(|response| self.check(response)),
            }
        }
    }

    /// One attempt: builds the request, records cookies, time and status.
    async fn send_once(
        &self,
        request: &Request,
        auth: Option<&AuthHeaders<'_>>,
    ) -> Result<Response, Error> {
        let base = self.inner.config.base_url.as_str().trim_end_matches('/');
        let url = format!("{base}{}", request.path);
        let mut builder = self
            .inner
            .http
            .request(request.method.clone(), &url)
            .header(APP_VERSION_HEADER, self.inner.config.app_version.as_str());
        if !request.query.is_empty() {
            builder = builder.query(&request.query);
        }
        if let Some(auth) = auth {
            if !auth.uid.is_empty() {
                builder = builder.header(UID_HEADER, auth.uid);
            }
            if !auth.access_token.is_empty() {
                builder = builder.header(AUTHORIZATION, format!("Bearer {}", auth.access_token));
            }
        }
        for (name, value) in &request.headers {
            builder = builder.header(*name, value);
        }
        let cookie_path = reqwest::Url::parse(&url)
            .map(|url| url.path().to_owned())
            .unwrap_or_else(|_| request.path.clone());
        if let Some(cookies) = self.inner.cookies.header_for(&cookie_path, true) {
            builder = builder.header(COOKIE, cookies);
        }
        if let Some(body) = &request.body {
            builder = builder
                .header(CONTENT_TYPE, request.content_type.as_str())
                .body(body.clone());
        }

        let response = match builder.send().await {
            Ok(response) => response,
            Err(error) => {
                self.set_status(Status::Down);
                return Err(Error::from_transport(error));
            }
        };
        let status = response.status();
        let headers = response.headers().clone();
        let host = self.inner.config.base_url.host_str().unwrap_or_default();
        self.inner.cookies.store(
            host,
            &cookie_path,
            headers
                .get_all(SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok()),
        );
        if let Some(date) = headers
            .get(DATE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
        {
            self.inner
                .server_time
                .store(date.timestamp(), Ordering::Relaxed);
        }
        let body = match response.bytes().await {
            Ok(body) => body.to_vec(),
            Err(error) => {
                self.set_status(Status::Down);
                return Err(Error::from_transport(error));
            }
        };
        self.set_status(Status::Up);
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    /// Turns an error status into an [`Error`] and runs the code handlers
    /// (`catchAPIError` and `handleError`).
    fn check(&self, response: Response) -> Result<Response, Error> {
        if !response.status.is_client_error() && !response.status.is_server_error() {
            return Ok(response);
        }
        let error = ApiError::from_response(response.status, &response.body);
        let handlers = lock(&self.inner.error_handlers)
            .get(&error.code)
            .cloned()
            .unwrap_or_default();
        for handler in handlers {
            handler();
        }
        Err(Error::from_api(error))
    }

    /// Records the connection status and notifies observers on a change
    /// (`onConnUp` and `onConnDown`).
    fn set_status(&self, status: Status) {
        {
            let mut current = lock(&self.inner.status);
            if *current == status {
                return;
            }
            *current = status;
        }
        let observers = lock(&self.inner.observers).clone();
        for observer in observers {
            observer(status);
        }
    }
}

/// The session headers sent with an authenticated request.
pub(crate) struct AuthHeaders<'a> {
    pub(crate) uid: &'a str,
    pub(crate) access_token: &'a str,
}

/// A request to the API, relative to the API base URL.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) method: Method,
    pub(crate) path: String,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) headers: Vec<(&'static str, String)>,
    pub(crate) body: Option<Vec<u8>>,
    /// The body's media type.
    pub(crate) content_type: String,
}

impl Request {
    pub(crate) fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
            content_type: String::new(),
        }
    }

    pub(crate) fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    pub(crate) fn post(path: impl Into<String>) -> Self {
        Self::new(Method::POST, path)
    }

    pub(crate) fn put(path: impl Into<String>) -> Self {
        Self::new(Method::PUT, path)
    }

    pub(crate) fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::DELETE, path)
    }

    /// Adds a header.
    pub(crate) fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    /// Adds a query parameter.
    pub(crate) fn query(mut self, name: impl Into<String>, value: impl ToString) -> Self {
        self.query.push((name.into(), value.to_string()));
        self
    }

    /// Sets a JSON body.
    pub(crate) fn json(mut self, body: &impl Serialize) -> Result<Self, Error> {
        self.body = Some(serde_json::to_vec(body)?);
        self.content_type = "application/json".to_owned();
        Ok(self)
    }

    /// Sets a body of another media type (a multipart form).
    pub(crate) fn body(mut self, content_type: impl Into<String>, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self.content_type = content_type.into();
        self
    }
}

/// A successful (non-error) response.
#[derive(Debug)]
pub(crate) struct Response {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
}

impl Response {
    /// Decodes the JSON body. Success bodies are not required to carry
    /// `Code` (the fake server omits it on some routes).
    pub(crate) fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        Ok(serde_json::from_slice(&self.body)?)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
