//! The client over real sockets: the default `reqwest` transport against
//! HTTP servers on loopback. What is checked is what goes on the wire
//! (headers, `using`, redirects followed or refused, credentials and who
//! sees them) and how answers that are not a Response come back
//! (problem details, limits, timeouts).
#![cfg(feature = "reqwest")]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use jmap_wire::{
    BlobDownload, Client, ClientBuilder, Credentials, Error, Get, ProblemKind, RedirectPolicy,
    RedirectRefusal, Request, ReqwestTransport, Set, TokenError, TokenRequest, TokenSource,
    TransportErrorKind, capability, discovery,
    mail::{Email, Identity},
};
use serde_json::{Value, json};
use url::Url;

/// One request a server received.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    /// Header names in lower case.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
}

/// What a server answers with.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Wait this long before answering.
    delay: Duration,
}

impl Answer {
    fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".to_owned(), content_type.to_owned())],
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    fn json(value: Value) -> Self {
        Self::new(200, "application/json", value.to_string())
    }

    fn redirect(status: u16, location: &str) -> Self {
        Self {
            status,
            headers: vec![("Location".to_owned(), location.to_owned())],
            body: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    fn not_found() -> Self {
        Self::new(404, "text/plain", "not found")
    }
}

/// An HTTP server on loopback that answers each request with what
/// `handler` returns and records what it received. Every response says
/// `Connection: close`.
struct Server {
    origin: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    /// Starts a server reached as `http://{host}:{port}`; `host` is the
    /// name the client uses for it (`127.0.0.1` or `localhost`), so two
    /// servers can be two hosts.
    fn start(host: &str, handler: impl Fn(&Seen, &str) -> Answer + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        let origin = format!("http://{host}:{port}");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (log, base) = (seen.clone(), origin.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                serve(stream, &base, &handler, &log);
            }
        });
        Self { origin, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the log").clone()
    }

    fn url(&self, path: &str) -> Url {
        Url::parse(&format!("{}{path}", self.origin)).expect("a URL")
    }
}

fn serve(
    mut stream: TcpStream,
    base: &str,
    handler: &impl Fn(&Seen, &str) -> Answer,
    log: &Mutex<Vec<Seen>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let seen = Seen {
        method,
        target,
        headers,
        body,
    };
    log.lock().expect("the log").push(seen.clone());
    let answer = handler(&seen, base);
    std::thread::sleep(answer.delay);
    let mut head = format!("HTTP/1.1 {} Status\r\n", answer.status);
    for (name, value) in &answer.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        answer.body.len()
    ));
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&answer.body);
}

/// A session whose endpoints are on `base`.
fn session(base: &str) -> Value {
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {
                "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                "maxCallsInRequest": 16, "maxObjectsInGet": 500,
                "maxObjectsInSet": 500, "collationAlgorithms": []
            },
            "urn:ietf:params:jmap:mail": {},
            "urn:ietf:params:jmap:submission": {}
        },
        "accounts": {"a1": {"name": "ada@example.com", "isPersonal": true,
                             "isReadOnly": false, "accountCapabilities": {}}},
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a1",
                             "urn:ietf:params:jmap:submission": "a1"},
        "username": "ada@example.com",
        "apiUrl": format!("{base}/api"),
        "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{base}/upload/{{accountId}}/"),
        "eventSourceUrl": format!("{base}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": "s1"
    })
}

/// A server that is a JMAP server: the session at `/session`, and at
/// `/api` whatever `api` answers.
fn jmap_server(api: impl Fn(&Seen) -> Answer + Send + 'static) -> Server {
    Server::start("127.0.0.1", move |seen, base| {
        match (seen.method.as_str(), seen.target.as_str()) {
            ("GET", "/session") => Answer::json(session(base)),
            ("POST", "/api") => api(seen),
            _ => Answer::not_found(),
        }
    })
}

/// Answers every method call with an empty answer of the same name.
fn echo_names(seen: &Seen) -> Answer {
    let request = seen.json();
    let responses = request["methodCalls"]
        .as_array()
        .expect("method calls")
        .iter()
        .map(|call| json!([call[0], {"accountId": "a1"}, call[2]]))
        .collect::<Vec<_>>();
    Answer::json(json!({"methodResponses": responses, "sessionState": "s1"}))
}

fn builder() -> ClientBuilder {
    Client::builder()
        .credentials(Credentials::basic("ada@example.com", "pw"))
        .timeout(Duration::from_secs(10))
}

async fn connect(server: &Server) -> Client<ReqwestTransport> {
    builder()
        .connect(server.url("/session"))
        .await
        .expect("the session loads")
}

#[tokio::test]
async fn the_session_and_requests_are_sent_with_credentials_and_json_headers() {
    let server = jmap_server(echo_names);
    let client = connect(&server).await;
    assert_eq!(client.session().username, "ada@example.com");
    assert_eq!(client.api_url().path(), "/api");
    assert_eq!(client.primary_account(capability::MAIL).expect("one"), "a1");
    assert!(matches!(
        client.primary_account("urn:ietf:params:jmap:contacts"),
        Err(Error::Session(_))
    ));

    client
        .call(Get::<Identity>::new("a1"))
        .await
        .expect("answered");

    let seen = server.seen();
    let [session, api] = seen.as_slice() else {
        panic!("two requests: {seen:?}");
    };
    // "ada@example.com:pw" in Base64 (RFC 7617).
    let basic = "Basic YWRhQGV4YW1wbGUuY29tOnB3";
    assert_eq!(session.method, "GET");
    assert_eq!(session.header("authorization"), Some(basic));
    assert_eq!(session.header("accept"), Some("application/json"));
    assert!(session.header("content-type").is_none());
    assert_eq!(api.method, "POST");
    assert_eq!(api.header("authorization"), Some(basic));
    assert_eq!(api.header("content-type"), Some("application/json"));
    assert!(
        api.header("user-agent")
            .is_some_and(|agent| agent.starts_with("jmap-wire/")),
        "{api:?}"
    );
    assert!(api.header("cookie").is_none() && api.header("referer").is_none());
    assert_eq!(
        api.json(),
        json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:submission"],
            "methodCalls": [["Identity/get", {"accountId": "a1"}, "c0"]]
        })
    );
}

/// The regression test for unicornops/zmail#132, on the wire: the server
/// rejects any capability but Core and Mail, as RFC 8620 section 3.6.1
/// says a server must for one it does not support, and a request that
/// uses only Core and Mail gets through.
#[tokio::test]
async fn a_core_and_mail_request_passes_a_server_that_knows_only_core_and_mail() {
    let server = jmap_server(|seen| {
        let request = seen.json();
        let unknown = request["using"]
            .as_array()
            .expect("using")
            .iter()
            .filter_map(Value::as_str)
            .find(|uri| ![capability::CORE, capability::MAIL].contains(uri))
            .map(str::to_owned);
        match unknown {
            Some(uri) => Answer::new(
                400,
                "application/problem+json",
                json!({
                    "type": "urn:ietf:params:jmap:error:unknownCapability",
                    "status": 400,
                    "detail": format!("The Request object used capability '{uri}', \
                                       which is not supported by this server.")
                })
                .to_string(),
            ),
            None => echo_names(seen),
        }
    });
    let client = connect(&server).await;

    let mut request = Request::new();
    request.call_raw("Mailbox/get", json!({"accountId": "a1"}));
    request.call_raw("Email/query", json!({"accountId": "a1"}));
    let mut destroy = Set::<Email>::new("a1");
    destroy.destroy(["e1"]);
    request.call(destroy).expect("encodes");
    let response = client.send(&request).await.expect("not rejected");
    assert_eq!(response.method_responses.len(), 3);
    let sent = server.seen().last().expect("a request").json();
    assert_eq!(
        sent["using"],
        json!(["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"])
    );

    // And the server does reject what it does not know, as problem
    // details the client hands back whole.
    let mut request = Request::new();
    request.call_raw("Mailbox/get", json!({"accountId": "a1"}));
    request.add_capability("urn:ietf:params:jmap:contacts");
    match client.send(&request).await {
        Err(Error::Problem(problem)) => {
            assert_eq!(problem.kind(), ProblemKind::UnknownCapability);
            assert_eq!(problem.status, Some(400));
            assert!(
                problem
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("contacts")),
                "{problem:?}"
            );
        }
        other => panic!("expected problem details, got {other:?}"),
    }
}

#[tokio::test]
async fn http_errors_come_back_as_problem_details_or_as_the_status() {
    let server = jmap_server(|seen| {
        let call = seen.json()["methodCalls"][0][0]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        match call.as_str() {
            // RFC 8620 section 3.6.1.1's second example, with a
            // parameter on the media type.
            "Limit/hit" => Answer::new(
                400,
                "application/problem+json; charset=utf-8",
                json!({
                    "type": "urn:ietf:params:jmap:error:limit",
                    "limit": "maxSizeRequest",
                    "status": 400,
                    "detail": "The request is larger than the server is willing to process."
                })
                .to_string(),
            ),
            // RFC 7807 with a type of the server's own, on a 5xx.
            "Server/down" => Answer::new(
                503,
                "application/problem+json",
                json!({"type": "https://example.com/problems/maintenance",
                       "title": "Down for maintenance", "status": 503,
                       "retryAfter": 120})
                .to_string(),
            ),
            "Gateway/bad" => Answer::new(502, "text/html", "<h1>Bad Gateway</h1>"),
            "Problem/garbled" => Answer::new(500, "application/problem+json", "<html>"),
            _ => Answer::new(200, "text/html", "<html>a captive portal</html>"),
        }
    });
    let client = connect(&server).await;
    let send = |name: &'static str| {
        let client = &client;
        async move {
            let mut request = Request::new();
            request.call_raw(name, json!({}));
            client.send(&request).await.expect_err("an error")
        }
    };

    match send("Limit/hit").await {
        Error::Problem(problem) => {
            assert_eq!(problem.kind(), ProblemKind::Limit);
            assert_eq!(problem.limit.as_deref(), Some("maxSizeRequest"));
        }
        other => panic!("{other:?}"),
    }
    match send("Server/down").await {
        Error::Problem(problem) => {
            assert_eq!(problem.kind(), ProblemKind::Other);
            assert_eq!(problem.status, Some(503));
            assert_eq!(problem.title.as_deref(), Some("Down for maintenance"));
            assert_eq!(problem.extra["retryAfter"], 120);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        send("Gateway/bad").await,
        Error::Http { status: 502 }
    ));
    assert!(matches!(
        send("Problem/garbled").await,
        Error::Http { status: 500 }
    ));
    assert!(matches!(send("Not/json").await, Error::Json { .. }));
}

#[tokio::test]
async fn a_response_over_the_size_limit_is_refused() {
    let server = jmap_server(|seen| {
        let mut answer = echo_names(seen);
        if seen.json()["methodCalls"][0][0] == "Big/get" {
            answer.body = json!({
                "methodResponses": [["Big/get", {"padding": "x".repeat(4096)}, "c0"]],
                "sessionState": "s1"
            })
            .to_string()
            .into_bytes();
        }
        answer
    });
    let client = builder()
        .max_response_bytes(1024)
        .connect(server.url("/session"))
        .await
        .expect("the session has its own limit");

    let mut small = Request::new();
    small.call_raw("Small/get", json!({}));
    client.send(&small).await.expect("under the limit");

    let mut big = Request::new();
    big.call_raw("Big/get", json!({}));
    match client.send(&big).await {
        Err(Error::Transport(error)) => assert_eq!(error.kind(), TransportErrorKind::TooLarge),
        other => panic!("expected a size error, got {other:?}"),
    }
}

/// Over real sockets with the default transport: `reqwest` hands the
/// compressed body on as it is, and the client decompresses it within
/// its limit.
#[cfg(feature = "gzip")]
#[tokio::test]
async fn a_gzipped_response_is_read_and_its_expanded_size_is_what_counts() {
    use std::io::Write as _;

    let server = jmap_server(|seen| {
        let padding = if seen.json()["methodCalls"][0][0] == "Big/get" {
            1024 * 1024
        } else {
            2000
        };
        let body = json!({
            "methodResponses": [["Any/get", {"padding": "x".repeat(padding)}, "c0"]],
            "sessionState": "s1"
        })
        .to_string();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(body.as_bytes()).expect("compresses");
        let mut answer = Answer::new(
            200,
            "application/json",
            encoder.finish().expect("compresses"),
        );
        answer
            .headers
            .push(("Content-Encoding".to_owned(), "gzip".to_owned()));
        answer
    });
    let client = builder()
        .max_response_bytes(64 * 1024)
        .connect(server.url("/session"))
        .await
        .expect("the session loads");

    let mut small = Request::new();
    small.call_raw("Small/get", json!({}));
    let response = client.send(&small).await.expect("decompressed");
    assert_eq!(
        response.method_responses[0].arguments["padding"]
            .as_str()
            .map(str::len),
        Some(2000)
    );
    let seen = server.seen();
    assert_eq!(
        seen.last().and_then(|seen| seen.header("accept-encoding")),
        Some("gzip")
    );

    // About a kilobyte on the wire, a megabyte expanded.
    let mut big = Request::new();
    big.call_raw("Big/get", json!({}));
    let error = client.send(&big).await.expect_err("over the limit");
    assert!(error.is_response_too_large(), "{error}");
}

#[tokio::test]
async fn an_exchange_that_takes_too_long_times_out() {
    let server = jmap_server(|seen| {
        let mut answer = echo_names(seen);
        answer.delay = Duration::from_secs(2);
        answer
    });
    let client = builder()
        .timeout(Duration::from_millis(200))
        .connect(server.url("/session"))
        .await
        .expect("the session is quick");
    let mut request = Request::new();
    request.call_raw("Core/echo", json!({}));
    match client.send(&request).await {
        Err(Error::Transport(error)) => {
            assert_eq!(error.kind(), TransportErrorKind::Timeout);
            assert!(!error.to_string().contains("pw"), "{error}");
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn nothing_listening_is_a_connect_error() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        listener.local_addr().expect("an address").port()
    };
    let url = Url::parse(&format!("http://127.0.0.1:{port}/session?token=s3cret")).expect("a URL");
    // A client that ignores any proxy in the environment, which would
    // answer for the port that is not there.
    let direct = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("a client");
    match builder()
        .connect_with(ReqwestTransport::from_client(direct), url)
        .await
    {
        Err(Error::Transport(error)) => {
            assert_eq!(error.kind(), TransportErrorKind::Connect);
            assert!(!error.to_string().contains("s3cret"), "{error}");
        }
        other => panic!("expected a connect error, got {other:?}"),
    }
}

/// A TLS configuration reqwest cannot use fails the transport's set-up:
/// it never falls back to the bundled roots.
#[test]
fn a_tls_configuration_reqwest_cannot_use_is_an_error() {
    let error = ReqwestTransport::with_preconfigured_tls("not a rustls configuration")
        .expect_err("only a rustls ClientConfig is accepted");
    assert_eq!(error.kind(), TransportErrorKind::Other);
}

/// Two hosts: `127.0.0.1` redirects its well-known URL and its API to
/// `localhost`, which is the JMAP server.
fn two_hosts() -> (Server, Server) {
    let api = Server::start("localhost", |seen, base| {
        match (seen.method.as_str(), seen.target.as_str()) {
            ("GET", "/session") if seen.header("authorization").is_none() => {
                Answer::new(401, "text/plain", "")
            }
            ("GET", "/session") => Answer::json(session(base)),
            ("POST", "/api") => echo_names(seen),
            _ => Answer::not_found(),
        }
    });
    let target = api.origin.clone();
    let front = Server::start("127.0.0.1", move |seen, base| {
        match (seen.method.as_str(), seen.target.as_str()) {
            ("GET", "/.well-known/jmap") => Answer::redirect(307, &format!("{target}/session")),
            ("GET", "/local") => Answer::redirect(302, "/session"),
            ("GET", "/session") => {
                // A session whose API endpoint redirects.
                let mut session = session(base);
                session["apiUrl"] = json!(format!("{base}/api"));
                Answer::json(session)
            }
            ("POST", "/api") => Answer::redirect(307, &format!("{target}/api")),
            ("POST", "/api-moved") => Answer::redirect(302, "/api"),
            ("GET", "/loop") => Answer::redirect(302, "/loop"),
            _ => Answer::not_found(),
        }
    });
    (front, api)
}

#[tokio::test]
async fn redirects_are_followed_only_where_the_policy_allows() {
    let (front, api) = two_hosts();

    // The default: the same host only. The other host sees nothing.
    let error = builder()
        .connect(front.url("/.well-known/jmap"))
        .await
        .expect_err("another host");
    assert!(
        matches!(&error, Error::Redirect { to, reason: RedirectRefusal::Policy }
            if to.starts_with(&api.origin)),
        "{error:?}"
    );
    assert!(api.seen().is_empty(), "{:?}", api.seen());

    // A same-host redirect is followed, credentials and all, and the
    // session URL is where the session was.
    let client = builder()
        .connect(front.url("/local"))
        .await
        .expect("the same host");
    assert_eq!(client.session_url().path(), "/session");

    // No redirects at all.
    let error = builder()
        .redirect_policy(RedirectPolicy::none())
        .connect(front.url("/local"))
        .await
        .expect_err("no redirects");
    assert!(matches!(
        error,
        Error::Redirect {
            reason: RedirectRefusal::Policy,
            ..
        }
    ));

    // The caller names the other host: followed, with the credentials.
    let client = builder()
        .redirect_policy(RedirectPolicy::hosts(["localhost"]))
        .connect(front.url("/.well-known/jmap"))
        .await
        .expect("an allowed host");
    assert_eq!(client.session_url().host_str(), Some("localhost"));
    assert!(
        api.seen()
            .iter()
            .any(|seen| seen.target == "/session" && seen.header("authorization").is_some())
    );

    // More redirects than the limit.
    let error = builder()
        .redirect_policy(RedirectPolicy::same_host().max_redirects(3))
        .connect(front.url("/loop"))
        .await
        .expect_err("a loop");
    assert!(matches!(
        error,
        Error::Redirect {
            reason: RedirectRefusal::TooMany,
            ..
        }
    ));
    let loops = front
        .seen()
        .iter()
        .filter(|seen| seen.target == "/loop")
        .count();
    assert_eq!(loops, 4, "the first request and three redirects");
}

#[tokio::test]
async fn an_api_redirect_keeps_the_post_or_is_refused() {
    let (front, api) = two_hosts();
    let mut request = Request::new();
    request.call_raw("Core/echo", json!({"hello": true}));

    // 307 to another host, refused by the default policy: the request
    // and its credentials stay where they were sent.
    let client = builder()
        .connect(front.url("/session"))
        .await
        .expect("the session loads");
    let error = client.send(&request).await.expect_err("another host");
    assert!(matches!(
        error,
        Error::Redirect {
            reason: RedirectRefusal::Policy,
            ..
        }
    ));
    assert!(api.seen().is_empty(), "{:?}", api.seen());

    // The same redirect when the caller allows it: the POST and its
    // body arrive unchanged.
    let client = builder()
        .redirect_policy(RedirectPolicy::custom(|attempt| {
            attempt.from.host_str() == Some("127.0.0.1")
                && attempt.to.host_str() == Some("localhost")
        }))
        .connect(front.url("/session"))
        .await
        .expect("the session loads");
    client.send(&request).await.expect("followed");
    let arrived = api.seen().pop().expect("a request");
    assert_eq!(arrived.method, "POST");
    assert_eq!(arrived.json()["methodCalls"][0][1], json!({"hello": true}));
    assert!(arrived.header("authorization").is_some());

    // A 302 would turn the POST into a GET: never followed.
    let mut session = session(&front.origin);
    session["apiUrl"] = json!(format!("{}/api-moved", front.origin));
    let client = builder()
        .with_session(
            ReqwestTransport::new().expect("a transport"),
            front.url("/session"),
            serde_json::from_value(session).expect("a session"),
        )
        .expect("a client");
    let error = client.send(&request).await.expect_err("a 302 on a POST");
    assert!(matches!(
        error,
        Error::Redirect {
            reason: RedirectRefusal::MethodChange,
            ..
        }
    ));
}

/// Hands out `token-1`, then `token-2` and so on at each refresh.
struct Tokens {
    refreshes: AtomicUsize,
    fail: bool,
}

impl TokenSource for Tokens {
    fn token(
        &self,
        request: TokenRequest,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<String, TokenError>> + Send + '_>> {
        Box::pin(async move {
            if request == TokenRequest::Refresh {
                if self.fail {
                    return Err("the refresh token was revoked".into());
                }
                self.refreshes.fetch_add(1, Ordering::SeqCst);
            }
            Ok(format!(
                "token-{}",
                self.refreshes.load(Ordering::SeqCst) + 1
            ))
        })
    }
}

/// A server that takes `Bearer token-2` only.
fn bearer_server() -> Server {
    Server::start("127.0.0.1", |seen, base| {
        if seen.header("authorization") != Some("Bearer token-2") {
            return Answer::new(401, "text/plain", "");
        }
        match seen.method.as_str() {
            "GET" => Answer::json(session(base)),
            _ => echo_names(seen),
        }
    })
}

#[tokio::test]
async fn a_bearer_token_the_server_refuses_is_refreshed_once() {
    let server = bearer_server();
    let tokens = Arc::new(Tokens {
        refreshes: AtomicUsize::new(0),
        fail: false,
    });
    struct Shared(Arc<Tokens>);
    impl TokenSource for Shared {
        fn token(
            &self,
            request: TokenRequest,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<String, TokenError>> + Send + '_>>
        {
            self.0.token(request)
        }
    }

    let client = Client::builder()
        .credentials(Credentials::bearer_source(Shared(tokens.clone())))
        .connect(server.url("/session"))
        .await
        .expect("signed in after one refresh");
    assert_eq!(tokens.refreshes.load(Ordering::SeqCst), 1);
    let sent = server
        .seen()
        .iter()
        .map(|seen| seen.header("authorization").unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(sent, ["Bearer token-1", "Bearer token-2"]);

    // The refreshed token is the current one from then on.
    let mut request = Request::new();
    request.call_raw("Core/echo", json!({}));
    client.send(&request).await.expect("answered");
    assert_eq!(tokens.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(server.seen().len(), 3);

    // A fixed token is never retried: the 401 is the answer.
    let error = Client::builder()
        .credentials(Credentials::bearer("token-1"))
        .connect(server.url("/session"))
        .await
        .expect_err("refused");
    assert!(matches!(error, Error::Http { status: 401 }), "{error:?}");

    // A source that cannot refresh says why.
    let error = Client::builder()
        .credentials(Credentials::bearer_source(Tokens {
            refreshes: AtomicUsize::new(0),
            fail: true,
        }))
        .connect(server.url("/session"))
        .await
        .expect_err("no token");
    assert!(
        matches!(&error, Error::Token(_)) && error.to_string().contains("no access token"),
        "{error:?}"
    );

    // A token that cannot be a header is refused before anything is
    // sent.
    let before = server.seen().len();
    let error = Client::builder()
        .credentials(Credentials::bearer("token\r\nX-Evil: 1"))
        .connect(server.url("/session"))
        .await
        .expect_err("not a header value");
    assert!(matches!(error, Error::InvalidCredentials));
    assert_eq!(server.seen().len(), before);
}

#[tokio::test]
async fn a_changed_session_state_is_noticed_and_the_session_refreshed() {
    let states = Arc::new(AtomicUsize::new(1));
    let current = states.clone();
    let server = Server::start("127.0.0.1", move |seen, base| {
        let state = format!("s{}", current.load(Ordering::SeqCst));
        match seen.method.as_str() {
            "GET" => {
                let mut session = session(base);
                session["state"] = json!(state);
                Answer::json(session)
            }
            _ => Answer::json(json!({"methodResponses": [], "sessionState": state})),
        }
    });
    let mut client = connect(&server).await;
    let request = Request::new();
    client.send(&request).await.expect("answered");
    assert!(!client.session_changed());

    states.store(2, Ordering::SeqCst);
    client.send(&request).await.expect("answered");
    assert!(client.session_changed());
    client.refresh_session().await.expect("refreshed");
    assert_eq!(client.session().state, "s2");
    assert!(!client.session_changed());
}

#[tokio::test]
async fn discovery_follows_the_well_known_redirect_without_credentials() {
    let (front, api) = two_hosts();
    let transport = ReqwestTransport::new().expect("a transport");
    let options = discovery::Discovery::default();

    // A bare origin means its well-known URL, which redirects to the
    // session on another host; that host answers 401 as a session
    // resource does.
    let found = discovery::session_url(&transport, &front.origin, &options)
        .await
        .expect("found");
    assert_eq!(found.as_str(), format!("{}/session", api.origin));
    assert!(
        front
            .seen()
            .iter()
            .chain(api.seen().iter())
            .all(|seen| seen.header("authorization").is_none()),
        "discovery sends no credentials"
    );

    // Connecting there with the strictest useful policy then works, and
    // the front host never sees the credentials.
    builder()
        .connect(found)
        .await
        .expect("signed in on the API host");
    assert!(
        front
            .seen()
            .iter()
            .all(|seen| seen.header("authorization").is_none())
    );

    // Any other URL is the session itself: no request is made.
    let before = front.seen().len();
    let direct = discovery::session_url(
        &transport,
        &format!("{}/jmap/session#top", front.origin),
        &options,
    )
    .await
    .expect("taken as it is");
    assert_eq!(direct.path(), "/jmap/session");
    assert_eq!(direct.fragment(), None);
    assert_eq!(front.seen().len(), before);

    assert!(matches!(
        discovery::session_url(&transport, "imap://example.com", &options).await,
        Err(Error::InvalidUrl { .. })
    ));
}

#[tokio::test]
async fn discovery_does_not_take_a_sign_in_page_for_the_session() {
    let elsewhere = Server::start("localhost", |_, _| {
        Answer::new(200, "text/html", "<form>Sign in</form>")
    });
    let target = format!("{}/login", elsewhere.origin);
    let front = Server::start("127.0.0.1", move |_, _| Answer::redirect(302, &target));
    let found = discovery::session_url(
        &ReqwestTransport::new().expect("a transport"),
        &format!("{}/.well-known/jmap", front.origin),
        &discovery::Discovery::default(),
    )
    .await
    .expect("falls back");
    assert_eq!(found, front.url("/.well-known/jmap"));
}

/// A JMAP server that keeps what is uploaded and serves it back, with
/// one blob larger than the API's response limit and one that is slow.
fn blob_server() -> Server {
    let blobs: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    Server::start("127.0.0.1", move |seen, base| {
        let mut blobs = blobs.lock().expect("the blobs");
        match (seen.method.as_str(), seen.target.as_str()) {
            ("GET", "/session") => Answer::json(session(base)),
            ("POST", "/upload/a1/") => {
                let content_type = seen.header("content-type").unwrap_or_default().to_owned();
                blobs.push(seen.body.clone());
                let mut answer = Answer::json(json!({
                    "accountId": "a1", "blobId": format!("b{}", blobs.len() - 1),
                    "type": content_type, "size": seen.body.len()
                }));
                answer.status = 201;
                answer
            }
            ("GET", target) if target.starts_with("/download/a1/slow/") => {
                let mut answer = Answer::new(200, "text/plain", "slow");
                answer.delay = Duration::from_millis(600);
                answer
            }
            ("GET", target) if target.starts_with("/download/a1/b") => {
                let index = target["/download/a1/b".len()..]
                    .split('/')
                    .next()
                    .and_then(|index| index.parse::<usize>().ok());
                match index.and_then(|index| blobs.get(index)) {
                    // The type asked for in the URL is what comes back.
                    Some(bytes) => Answer::new(
                        200,
                        if target.ends_with("type=image%2Fpng") {
                            "image/png"
                        } else {
                            "application/octet-stream"
                        },
                        bytes.clone(),
                    ),
                    None => Answer::new(
                        404,
                        "application/problem+json",
                        r#"{"type":"about:blank","status":404,"detail":"No such blob"}"#,
                    ),
                }
            }
            _ => Answer::not_found(),
        }
    })
}

/// RFC 8620 sections 6.1 and 6.2 over real sockets with the default
/// transport: what is uploaded comes back byte for byte, larger than
/// the API's response limit allows and with headers of its own.
#[tokio::test]
async fn a_blob_goes_up_and_comes_back_under_limits_of_its_own() {
    let server = blob_server();
    let client = builder()
        .max_response_bytes(64 * 1024)
        .connect(server.url("/session"))
        .await
        .expect("the session loads");
    let account = client
        .primary_account(capability::MAIL)
        .expect("an account")
        .to_owned();

    // Every byte value, 300 KiB of them: over the API's 64 KiB.
    let bytes = (0..300 * 1024).map(|index| index as u8).collect::<Vec<_>>();
    let blob = client
        .upload(&account, "image/png", bytes.clone())
        .await
        .expect("uploaded");
    assert_eq!(blob.blob_id, "b0");
    assert_eq!(blob.size, Some(bytes.len() as u64));
    assert_eq!(blob.content_type.as_deref(), Some("image/png"));

    let download = BlobDownload::new(&account, &blob.blob_id)
        .name("every byte.png")
        .content_type("image/png");
    let back = client.download(&download).await.expect("downloaded");
    assert_eq!(back.bytes, bytes, "byte for byte");
    assert_eq!(back.content_type.as_deref(), Some("image/png"));

    let seen = server.seen();
    let upload = seen
        .iter()
        .find(|seen| seen.target == "/upload/a1/")
        .expect("the upload");
    assert_eq!(upload.method, "POST");
    assert_eq!(upload.header("content-type"), Some("image/png"));
    assert_eq!(upload.header("accept"), Some("application/json"));
    assert!(upload.header("authorization").is_some());
    let fetched = seen
        .iter()
        .find(|seen| seen.target.starts_with("/download/"))
        .expect("the download");
    assert_eq!(
        fetched.target,
        "/download/a1/b0/every%20byte.png?type=image%2Fpng"
    );
    assert_eq!(fetched.header("accept"), Some("image/png"));
    assert_eq!(fetched.header("accept-encoding"), None);
    assert!(fetched.header("authorization").is_some());
    assert_eq!(fetched.header("cookie"), None);

    // The download's own limit is kept by the transport: the body is
    // not read past it.
    let error = client
        .download(&BlobDownload::new(&account, "b0").max_bytes(1024))
        .await
        .expect_err("over its limit");
    assert!(error.is_response_too_large(), "{error:?}");
    // At the limit exactly is fine.
    client
        .download(&BlobDownload::new(&account, "b0").max_bytes(bytes.len()))
        .await
        .expect("at its limit");

    // A blob the server does not have.
    let error = client
        .download(&BlobDownload::new(&account, "b7"))
        .await
        .expect_err("no such blob");
    assert_eq!(error.http_status(), Some(404));
    assert!(matches!(error, Error::Problem(_)), "{error:?}");
}

/// A blob may take longer than an API call: the two timeouts are
/// separate, and a download can have its own.
#[tokio::test]
async fn a_slow_blob_is_given_the_blob_timeout_not_the_api_timeout() {
    let server = blob_server();
    let client = builder()
        .timeout(Duration::from_millis(200))
        .blob_timeout(Duration::from_secs(10))
        .connect(server.url("/session"))
        .await
        .expect("the session loads");
    let slow = BlobDownload::new("a1", "slow");
    let blob = client
        .download(&slow)
        .await
        .expect("slower than an API call may be");
    assert_eq!(blob.bytes, b"slow");

    match client
        .download(&slow.clone().timeout(Duration::from_millis(100)))
        .await
    {
        Err(Error::Transport(error)) => assert_eq!(error.kind(), TransportErrorKind::Timeout),
        other => panic!("expected a timeout, got {other:?}"),
    }
}
