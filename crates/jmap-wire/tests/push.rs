//! Push over real sockets: the event source and streamed downloads over
//! the default `reqwest` transport, and JMAP over WebSocket over the
//! `tungstenite` feature's, against servers on loopback. What is
//! checked is what goes on the wire (the URL, the headers, the
//! messages), who sees the credentials, and how streams that end, go
//! quiet, break or are too large come back.
#![cfg(feature = "reqwest")]

use std::{
    future::Future,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use jmap_wire::{
    BlobDownload, Client, ClientBuilder, Credentials, Error, ProblemKind, RedirectRefusal,
    ReqwestTransport, TokenError, TokenRequest, TokenSource, TransportErrorKind,
    event_source::{EventSource, EventSourceEvent},
};
use serde_json::{Value, json};
use url::Url;

/// One request a server received.
#[derive(Debug, Clone)]
struct Seen {
    target: String,
    /// Header names in lower case.
    headers: Vec<(String, String)>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One step of a response sent in chunks.
#[derive(Clone)]
enum Step {
    Send(Vec<u8>),
    Wait(Duration),
    /// The last chunk: the response ends as it should.
    End,
    /// The connection is dropped in the middle of the response.
    Abort,
}

fn send(text: &str) -> Step {
    Step::Send(text.as_bytes().to_vec())
}

/// What a server answers with.
enum Reply {
    Plain {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    },
    /// A 200 sent in chunks; when the steps run out without an end, the
    /// connection is held until the client drops it.
    Chunked {
        headers: Vec<(&'static str, String)>,
        steps: Vec<Step>,
    },
    /// No answer at all.
    Hang,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self::Plain {
            status: 200,
            headers: vec![("Content-Type", "application/json".to_owned())],
            body: value.to_string().into_bytes(),
        }
    }

    fn status(status: u16) -> Self {
        Self::Plain {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn redirect(location: &str) -> Self {
        Self::Plain {
            status: 307,
            headers: vec![("Location", location.to_owned())],
            body: Vec::new(),
        }
    }

    fn events(steps: Vec<Step>) -> Self {
        Self::Chunked {
            headers: vec![("Content-Type", "text/event-stream".to_owned())],
            steps,
        }
    }
}

/// An HTTP server on loopback, one thread per connection, that answers
/// each request with what `handler` returns and records what it
/// received.
struct Server {
    origin: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    /// Starts a server reached as `http://{host}:{port}`; `host` is the
    /// name the client uses for it (`127.0.0.1` or `localhost`), so two
    /// servers can be two hosts.
    fn start(host: &str, handler: impl Fn(&Seen, &str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        let origin = format!("http://{host}:{port}");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (log, base, handler) = (seen.clone(), origin.clone(), Arc::new(handler));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (log, base, handler) = (log.clone(), base.clone(), handler.clone());
                std::thread::spawn(move || serve(stream, &base, &*handler, &log));
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
    handler: &dyn Fn(&Seen, &str) -> Reply,
    log: &Mutex<Vec<Seen>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
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
    let seen = Seen { target, headers };
    log.lock().expect("the log").push(seen.clone());
    match handler(&seen, base) {
        Reply::Plain {
            status,
            headers,
            body,
        } => {
            let mut head = format!("HTTP/1.1 {status} Status\r\n");
            for (name, value) in &headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ));
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
        Reply::Chunked { headers, steps } => {
            let mut head = "HTTP/1.1 200 OK\r\n".to_owned();
            for (name, value) in &headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
            if stream.write_all(head.as_bytes()).is_err() {
                return;
            }
            for step in steps {
                match step {
                    Step::Send(bytes) => {
                        let _ = stream.write_all(format!("{:x}\r\n", bytes.len()).as_bytes());
                        let _ = stream.write_all(&bytes);
                        if stream.write_all(b"\r\n").is_err() {
                            return;
                        }
                        let _ = stream.flush();
                    }
                    Step::Wait(time) => std::thread::sleep(time),
                    Step::End => {
                        let _ = stream.write_all(b"0\r\n\r\n");
                        return;
                    }
                    Step::Abort => return,
                }
            }
            until_closed(stream);
        }
        Reply::Hang => until_closed(stream),
    }
}

/// Holds the connection until the client drops it.
fn until_closed(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut buffer = [0_u8; 256];
    while matches!(stream.read(&mut buffer), Ok(read) if read > 0) {}
}

/// A session whose endpoints are on `base`, with a WebSocket at
/// `websocket` when there is one.
fn session(base: &str, websocket: Option<&str>) -> Value {
    let mut capabilities = json!({
        "urn:ietf:params:jmap:core": {
            "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
            "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
            "maxCallsInRequest": 16, "maxObjectsInGet": 500,
            "maxObjectsInSet": 500, "collationAlgorithms": []
        },
        "urn:ietf:params:jmap:mail": {}
    });
    if let Some(url) = websocket {
        capabilities["urn:ietf:params:jmap:websocket"] = json!({"url": url, "supportsPush": true});
    }
    json!({
        "capabilities": capabilities,
        "accounts": {"a1": {"name": "ada@example.com", "isPersonal": true,
                             "isReadOnly": false, "accountCapabilities": {}}},
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a1"},
        "username": "ada@example.com",
        "apiUrl": format!("{base}/api"),
        "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{base}/upload/{{accountId}}/"),
        "eventSourceUrl": format!("{base}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": "s1"
    })
}

/// A JMAP server whose event source answers what `events` returns.
fn event_server(events: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Server {
    Server::start("127.0.0.1", move |seen, base| {
        if seen.target == "/session" {
            Reply::json(session(base, None))
        } else if seen.target.starts_with("/events") {
            events(seen)
        } else {
            Reply::status(404)
        }
    })
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

fn state_change(account: &str, data_type: &str, state: &str) -> String {
    json!({"@type": "StateChange", "changed": {account: {data_type: state}}}).to_string()
}

#[tokio::test]
async fn the_event_source_is_opened_as_rfc_8620_section_7_3_says_and_read_event_by_event() {
    let server = event_server(|_| {
        Reply::events(vec![
            send(": connected\n\n"),
            // Cut in the middle of an event, and of a line.
            send("event: state\nid: 41\ndata: {\"@type\":\"StateChange\","),
            send("\ndata: \"changed\":{\"a1\":{\"Email\":\"s2\"}}}\n\nevent: pi"),
            send("ng\ndata: {\"interval\":300}\n\n"),
            send(&format!(
                "retry: 4000\r\nid: 42\r\ndata: {}\r\n\r\n",
                state_change("a1", "Mailbox", "m3")
            )),
            send("event: vendor\ndata: something else\n\nevent: state\ndata: not JSON\n\n"),
            Step::End,
        ])
    });
    let client = connect(&server).await;
    let source = EventSource::new()
        .types(["Email", "Mailbox"])
        .ping(Duration::from_secs(30))
        .last_event_id("40");
    let mut events = client.event_source(&source).await.expect("opens");
    assert_eq!(events.last_event_id(), Some("40"));
    assert_eq!(events.idle_timeout(), Some(Duration::from_secs(45)));

    let Some(EventSourceEvent::StateChange(change)) = events.next().await.expect("an event") else {
        panic!("a state change");
    };
    assert_eq!(change.state("a1", "Email"), Some("s2"));
    assert_eq!(events.last_event_id(), Some("41"));

    assert_eq!(
        events.next().await.expect("an event"),
        Some(EventSourceEvent::Ping {
            interval: Some(Duration::from_secs(300))
        })
    );
    // The server said how often it pings: silence is measured by that.
    assert_eq!(events.idle_timeout(), Some(Duration::from_secs(315)));

    let Some(EventSourceEvent::StateChange(change)) = events.next().await.expect("an event") else {
        panic!("a state change without an event type");
    };
    assert!(change.touches("a1", ["Mailbox"]));
    assert_eq!(events.last_event_id(), Some("42"));
    assert_eq!(events.retry(), Some(Duration::from_secs(4)));

    for (name, data) in [("vendor", "something else"), ("state", "not JSON")] {
        let Some(EventSourceEvent::Other(event)) = events.next().await.expect("an event") else {
            panic!("an event kept as it came");
        };
        assert_eq!((event.event.as_str(), event.data.as_str()), (name, data));
        assert_eq!(event.id.as_deref(), Some("42"));
    }
    assert_eq!(events.next().await.expect("the end"), None);

    let seen = server.seen();
    let opened = seen.last().expect("the event source request");
    assert_eq!(
        opened.target,
        "/events?types=Email,Mailbox&closeafter=no&ping=30"
    );
    assert_eq!(opened.header("accept"), Some("text/event-stream"));
    assert_eq!(opened.header("cache-control"), Some("no-cache"));
    assert_eq!(opened.header("last-event-id"), Some("40"));
    assert_eq!(opened.header("accept-encoding"), None);
    assert!(opened.header("authorization").is_some());
    assert!(
        opened
            .header("user-agent")
            .is_some_and(|agent| agent.starts_with("jmap-wire/"))
    );
    assert_eq!(opened.header("cookie"), None);

    // Every type, closing after each state, no pings.
    let all = EventSource::new()
        .close_after_state()
        .ping(Duration::ZERO)
        .open_timeout(Duration::from_secs(5));
    assert_eq!(
        client.event_source_url(&all).expect("a URL").as_str(),
        format!("{}/events?types=*&closeafter=state&ping=0", server.origin)
    );
}

/// A stream outlives the client's timeout and its response size limit:
/// both are for exchanges that end.
#[tokio::test]
async fn a_stream_is_under_neither_the_timeout_nor_the_size_limit_of_an_exchange() {
    let line = format!(
        "event: state\ndata: {}\n\n",
        state_change("a1", "Email", "s")
    );
    let steps = (0..30)
        .flat_map(|_| [send(&line), Step::Wait(Duration::from_millis(40))])
        .chain([Step::End])
        .collect::<Vec<_>>();
    let server = event_server(move |_| Reply::events(steps.clone()));
    let signed_in = connect(&server).await;
    let client = builder()
        .timeout(Duration::from_millis(600))
        .max_response_bytes(1024)
        .with_session(
            ReqwestTransport::new().expect("a transport"),
            signed_in.session_url().clone(),
            signed_in.session().clone(),
        )
        .expect("a client");
    let started = Instant::now();
    // The response may take its time to begin on a busy machine; what is
    // shown is that it then outlasts the client's timeout.
    let mut events = client
        .event_source(&EventSource::new().open_timeout(Duration::from_secs(10)))
        .await
        .expect("opens");
    let mut count = 0;
    while let Some(event) = events.next().await.expect("an event") {
        assert!(matches!(event, EventSourceEvent::StateChange(_)));
        count += 1;
    }
    assert_eq!(count, 30);
    assert!(started.elapsed() > Duration::from_millis(900));
    assert!(line.len() * 30 > 1024);
}

#[tokio::test]
async fn a_stream_that_goes_quiet_for_longer_than_its_pings_allow_fails() {
    let server = event_server(|_| {
        Reply::events(vec![send(&format!(
            "event: state\ndata: {}\n\n",
            state_change("a1", "Email", "s2")
        ))])
    });
    let client = connect(&server).await;

    // A ping every second, dead 300 ms after one was due.
    let source = EventSource::new()
        .ping(Duration::from_secs(1))
        .ping_grace(Duration::from_millis(300));
    let mut events = client.event_source(&source).await.expect("opens");
    assert!(events.next().await.expect("an event").is_some());
    let started = Instant::now();
    let error = events.next().await.expect_err("silence");
    assert!(error.is_timeout(), "{error}");
    assert!(
        (Duration::from_millis(1200)..Duration::from_secs(5)).contains(&started.elapsed()),
        "{:?}",
        started.elapsed()
    );

    // Without pings, only an idle time the caller sets ends the wait.
    let source = EventSource::new()
        .ping(Duration::ZERO)
        .idle_timeout(Duration::from_millis(300));
    let mut events = client.event_source(&source).await.expect("opens");
    assert!(events.next().await.expect("an event").is_some());
    assert!(events.next().await.expect_err("silence").is_timeout());
    assert!(
        server
            .seen()
            .last()
            .is_some_and(|seen| seen.target.ends_with("ping=0"))
    );
}

#[tokio::test]
async fn a_stream_that_breaks_is_an_error_and_a_line_without_end_is_refused() {
    let server = event_server(|seen| {
        if seen.target.contains("types=Broken") {
            Reply::events(vec![send("event: state\ndata: {}\n\n"), Step::Abort])
        } else {
            Reply::events(vec![
                send(&"x".repeat(4000)),
                send(&"x".repeat(4000)),
                Step::End,
            ])
        }
    });
    let client = connect(&server).await;
    let mut events = client
        .event_source(&EventSource::new().types(["Broken"]))
        .await
        .expect("opens");
    assert!(events.next().await.expect("an event").is_some());
    assert!(matches!(
        events.next().await,
        Err(Error::Transport(error)) if error.kind() == TransportErrorKind::Other
    ));

    let mut events = client
        .event_source(&EventSource::new().max_event_bytes(5000))
        .await
        .expect("opens");
    assert!(matches!(
        events.next().await,
        Err(Error::Transport(error)) if error.kind() == TransportErrorKind::TooLarge
    ));
}

#[tokio::test]
async fn an_event_source_that_does_not_open_says_why() {
    let server = event_server(|seen| {
        let asked = |name: &str| seen.target.contains(&format!("types={name}&"));
        if asked("Missing") {
            Reply::status(404)
        } else if asked("Forbidden") {
            Reply::Plain {
                status: 403,
                headers: vec![("Content-Type", "application/problem+json".to_owned())],
                body: json!({"type": "urn:ietf:params:jmap:error:limit", "status": 403,
                             "limit": "maxConcurrentRequests"})
                .to_string()
                .into_bytes(),
            }
        } else if asked("Page") {
            Reply::Plain {
                status: 200,
                headers: vec![("Content-Type", "text/html; charset=utf-8".to_owned())],
                body: b"<html>Sign in</html>".to_vec(),
            }
        } else if asked("Zipped") {
            Reply::Chunked {
                headers: vec![
                    ("Content-Type", "text/event-stream".to_owned()),
                    ("Content-Encoding", "gzip".to_owned()),
                ],
                steps: vec![Step::End],
            }
        } else {
            Reply::Hang
        }
    });
    let client = connect(&server).await;
    let open = |name: &'static str| {
        let client = &client;
        async move {
            client
                .event_source(
                    &EventSource::new()
                        .types([name])
                        .open_timeout(Duration::from_millis(400)),
                )
                .await
                .expect_err("does not open")
        }
    };
    assert_eq!(open("Missing").await.http_status(), Some(404));
    assert!(matches!(
        open("Forbidden").await,
        Error::Problem(problem) if problem.kind() == ProblemKind::Limit
    ));
    assert!(matches!(
        open("Page").await,
        Error::NotEventStream { content_type: Some(found) } if found.starts_with("text/html")
    ));
    assert!(matches!(open("Zipped").await, Error::Transport(_)));
    let started = Instant::now();
    assert!(open("Silent").await.is_timeout());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn event_source_redirects_follow_the_policy_and_the_credentials_stay_home() {
    let other = Server::start("localhost", |_, _| Reply::events(vec![Step::End]));
    let elsewhere = format!("{}/events", other.origin);
    let server = event_server(move |seen| {
        if seen.target.contains("types=Away") {
            Reply::redirect(&elsewhere)
        } else if seen.target.starts_with("/events?") {
            Reply::redirect("/events-moved")
        } else {
            Reply::events(vec![send("data: moved\n\n"), Step::End])
        }
    });
    let client = connect(&server).await;

    let mut events = client
        .event_source(&EventSource::new())
        .await
        .expect("a redirect on the same host is followed");
    assert!(matches!(
        events.next().await.expect("an event"),
        Some(EventSourceEvent::Other(event)) if event.data == "moved"
    ));
    let seen = server.seen();
    let moved = seen.last().expect("the redirected request");
    assert_eq!(moved.target, "/events-moved");
    assert!(moved.header("authorization").is_some());
    assert_eq!(moved.header("accept"), Some("text/event-stream"));

    let error = client
        .event_source(&EventSource::new().types(["Away"]))
        .await
        .expect_err("another host");
    assert!(matches!(
        error,
        Error::Redirect {
            reason: RedirectRefusal::Policy,
            ..
        }
    ));
    assert!(other.seen().is_empty(), "{:?}", other.seen());
}

/// Hands out `token-1`, then `token-2` and so on at each refresh.
struct Tokens(Arc<AtomicUsize>);

impl TokenSource for Tokens {
    fn token(
        &self,
        request: TokenRequest,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<String, TokenError>> + Send + '_>> {
        Box::pin(async move {
            if request == TokenRequest::Refresh {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            Ok(format!("token-{}", self.0.load(Ordering::SeqCst) + 1))
        })
    }
}

#[tokio::test]
async fn a_token_the_event_source_refuses_is_refreshed_once() {
    let server = Server::start("127.0.0.1", |seen, base| {
        if seen.target == "/session" {
            Reply::json(session(base, None))
        } else if seen.header("authorization") == Some("Bearer token-2") {
            Reply::events(vec![send("data: in\n\n"), Step::End])
        } else {
            Reply::status(401)
        }
    });
    let refreshes = Arc::new(AtomicUsize::new(0));
    let client = Client::builder()
        .credentials(Credentials::bearer_source(Tokens(refreshes.clone())))
        .connect(server.url("/session"))
        .await
        .expect("the session loads");
    let mut events = client
        .event_source(&EventSource::new())
        .await
        .expect("opens after one refresh");
    assert!(events.next().await.expect("an event").is_some());
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    let sent = server
        .seen()
        .iter()
        .filter(|seen| seen.target.starts_with("/events"))
        .map(|seen| seen.header("authorization").unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(sent, ["Bearer token-1", "Bearer token-2"]);
}

#[tokio::test]
async fn a_blob_is_downloaded_piece_by_piece_within_a_limit_of_its_own() {
    let blob = (0..=255_u8).cycle().take(300_000).collect::<Vec<_>>();
    let body = blob.clone();
    let server = Server::start("127.0.0.1", move |seen, base| {
        if seen.target == "/session" {
            Reply::json(session(base, None))
        } else if seen.target.starts_with("/download/a1/whole/") {
            Reply::Plain {
                status: 200,
                headers: vec![("Content-Type", "image/png".to_owned())],
                body: body.clone(),
            }
        } else if seen.target.starts_with("/download/a1/chunked/") {
            Reply::Chunked {
                headers: vec![("Content-Type", "application/octet-stream".to_owned())],
                steps: body
                    .chunks(50_000)
                    .map(|piece| Step::Send(piece.to_vec()))
                    .chain([Step::End])
                    .collect(),
            }
        } else if seen.target.starts_with("/download/a1/cut/") {
            Reply::Chunked {
                headers: Vec::new(),
                steps: vec![Step::Send(body[..1000].to_vec()), Step::Abort],
            }
        } else {
            Reply::status(404)
        }
    });
    // A response size limit far below the blob: it is not a stream's.
    let client = builder()
        .max_response_bytes(64 * 1024)
        .max_download_bytes(64 * 1024)
        .connect(server.url("/session"))
        .await
        .expect("the session loads");

    for id in ["whole", "chunked"] {
        let download = BlobDownload::new("a1", id)
            .name("a b.png")
            .content_type("image/png");
        let mut stream = client.download_stream(&download).await.expect("opens");
        let (mut read, mut pieces) = (Vec::new(), 0);
        while let Some(piece) = stream.chunk().await.expect("a piece") {
            read.extend_from_slice(&piece);
            pieces += 1;
        }
        assert_eq!(read, blob, "{id}: byte for byte");
        assert!(pieces > 1, "{id}: {pieces} pieces");
        if id == "whole" {
            assert_eq!(stream.content_type(), Some("image/png"));
            assert_eq!(stream.content_length(), Some(300_000));
        }
    }
    let seen = server.seen();
    let fetched = seen.last().expect("the download");
    assert_eq!(
        fetched.target,
        "/download/a1/chunked/a%20b.png?type=image%2Fpng"
    );
    assert_eq!(fetched.header("accept"), Some("image/png"));
    assert_eq!(fetched.header("accept-encoding"), None);
    assert!(fetched.header("authorization").is_some());

    // The download's own limit: known from the length, or met on the way.
    let limited = |id: &str| BlobDownload::new("a1", id).max_bytes(100_000);
    assert!(matches!(
        client.download_stream(&limited("whole")).await,
        Err(error) if error.is_response_too_large()
    ));
    let mut stream = client
        .download_stream(&limited("chunked"))
        .await
        .expect("opens");
    let error = loop {
        match stream.chunk().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the limit was not kept"),
            Err(error) => break error,
        }
    };
    assert!(error.is_response_too_large(), "{error}");

    // A body that stops short is an error, never a short blob.
    let mut stream = client
        .download_stream(&BlobDownload::new("a1", "cut"))
        .await
        .expect("opens");
    let error = loop {
        match stream.chunk().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("a cut download read as whole"),
            Err(error) => break error,
        }
    };
    assert!(matches!(error, Error::Transport(_)), "{error}");

    let missing = client
        .download_stream(&BlobDownload::new("a1", "missing"))
        .await
        .expect_err("no such blob");
    assert_eq!(missing.http_status(), Some(404));
}

#[cfg(feature = "tungstenite")]
mod websocket {
    use jmap_wire::{
        Get, Request, TungsteniteTransport,
        mail::Mailbox,
        websocket::{PushEnable, WebSocketMessage},
    };
    use tokio_tungstenite::tungstenite::{
        self,
        handshake::server::{ErrorResponse, Request as Handshake, Response},
        http::{HeaderValue, StatusCode},
    };

    use super::*;

    type Socket = tungstenite::WebSocket<TcpStream>;

    /// A WebSocket server on loopback. It refuses a handshake with the
    /// status `refuse` returns for its headers, confirms the
    /// subprotocol unless `confirm` is false, and then runs `serve`.
    struct WsServer {
        url: String,
        handshakes: Arc<Mutex<Vec<Seen>>>,
    }

    impl WsServer {
        fn start(
            confirm: bool,
            refuse: impl Fn(&Seen) -> Option<u16> + Send + Sync + 'static,
            serve: impl Fn(Socket) + Send + Sync + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let url = format!("ws://{}/ws", listener.local_addr().expect("an address"));
            let handshakes = Arc::new(Mutex::new(Vec::new()));
            let (log, refuse, serve) = (handshakes.clone(), Arc::new(refuse), Arc::new(serve));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let (log, refuse, serve) = (log.clone(), refuse.clone(), serve.clone());
                    std::thread::spawn(move || {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                        // The callback's error type is the library's.
                        #[allow(clippy::result_large_err)]
                        let accepted = tungstenite::accept_hdr(
                            stream,
                            |request: &Handshake, mut response: Response| {
                                let seen = Seen {
                                    target: request.uri().to_string(),
                                    headers: request
                                        .headers()
                                        .iter()
                                        .map(|(name, value)| {
                                            (
                                                name.as_str().to_owned(),
                                                value.to_str().unwrap_or_default().to_owned(),
                                            )
                                        })
                                        .collect(),
                                };
                                log.lock().expect("the log").push(seen.clone());
                                if let Some(status) = refuse(&seen) {
                                    let mut refusal = ErrorResponse::new(None);
                                    *refusal.status_mut() =
                                        StatusCode::from_u16(status).expect("a status");
                                    return Err(refusal);
                                }
                                if confirm {
                                    response.headers_mut().insert(
                                        "Sec-WebSocket-Protocol",
                                        HeaderValue::from_static("jmap"),
                                    );
                                }
                                Ok(response)
                            },
                        );
                        if let Ok(socket) = accepted {
                            serve(socket);
                        }
                    });
                }
            });
            Self { url, handshakes }
        }

        fn handshakes(&self) -> Vec<Seen> {
            self.handshakes.lock().expect("the log").clone()
        }
    }

    /// Answers each request as a server would, pushes a state change
    /// once push is enabled, and answers a message that is not JSON
    /// with a request-level error.
    fn jmap_socket(mut socket: Socket) {
        while let Ok(message) = socket.read() {
            let tungstenite::Message::Text(text) = message else {
                continue;
            };
            let reply = match serde_json::from_str::<Value>(text.as_str()) {
                Ok(frame) if frame["@type"] == "Request" => json!({
                    "@type": "Response",
                    "requestId": frame["id"],
                    "methodResponses": frame["methodCalls"].as_array().expect("calls").iter()
                        .map(|call| json!([call[0], {"accountId": "a1", "state": "m1",
                                                     "list": [], "notFound": []}, call[2]]))
                        .collect::<Vec<_>>(),
                    "sessionState": "s1"
                }),
                Ok(frame) if frame["@type"] == "WebSocketPushEnable" => json!({
                    "@type": "StateChange",
                    "changed": {"a1": {"Email": "s2"}},
                    "pushState": format!("after {}", frame["dataTypes"])
                }),
                Ok(frame) if frame["@type"] == "WebSocketPushDisable" => {
                    json!({"@type": "Vendor", "disabled": true})
                }
                _ => json!({
                    "@type": "RequestError", "requestId": null,
                    "type": "urn:ietf:params:jmap:error:notJSON", "status": 400,
                    "detail": "The request did not parse as I-JSON."
                }),
            };
            if socket.send(reply.to_string().into()).is_err() {
                return;
            }
        }
    }

    /// A client whose session advertises the WebSocket at `url`.
    async fn client_for(url: &str, builder: ClientBuilder) -> Client<ReqwestTransport> {
        let url = url.to_owned();
        let server = Server::start("127.0.0.1", move |seen, base| {
            if seen.target == "/session" {
                Reply::json(session(base, Some(&url)))
            } else {
                Reply::status(404)
            }
        });
        builder
            .connect(server.url("/session"))
            .await
            .expect("the session loads")
    }

    const WAIT: Option<Duration> = Some(Duration::from_secs(10));

    #[tokio::test]
    async fn requests_pushes_and_errors_travel_as_rfc_8887_has_them() {
        let server = WsServer::start(true, |_| None, jmap_socket);
        let client = client_for(&server.url, builder()).await;
        assert_eq!(client.websocket_url().expect("a URL").as_str(), server.url);
        let mut socket = client
            .connect_websocket(&TungsteniteTransport::new())
            .await
            .expect("connects");

        let handshakes = server.handshakes();
        let handshake = handshakes.first().expect("the handshake");
        assert_eq!(handshake.target, "/ws");
        assert_eq!(handshake.header("sec-websocket-protocol"), Some("jmap"));
        // "ada@example.com:pw" in Base64 (RFC 7617).
        assert_eq!(
            handshake.header("authorization"),
            Some("Basic YWRhQGV4YW1wbGUuY29tOnB3")
        );
        assert!(
            handshake
                .header("user-agent")
                .is_some_and(|agent| agent.starts_with("jmap-wire/"))
        );
        assert_eq!(handshake.header("cookie"), None);
        assert_eq!(handshake.header("origin"), None);

        // Two requests; each answer names the one it answers.
        let mut request = Request::new();
        let mailboxes = request.call(Get::<Mailbox>::new("a1")).expect("encodes");
        let first = socket.send(&request).await.expect("sent");
        let second = socket.send(&request).await.expect("sent");
        assert_eq!((first.as_str(), second.as_str()), ("r0", "r1"));
        for sent in [&first, &second] {
            let Some(WebSocketMessage::Response {
                request_id,
                mut response,
            }) = socket.next(WAIT).await.expect("a message")
            else {
                panic!("a response");
            };
            assert_eq!(request_id.as_ref(), Some(sent));
            assert_eq!(
                response.take(&mailboxes).expect("typed").state.as_deref(),
                Some("m1")
            );
        }

        socket
            .enable_push(&PushEnable::new().data_types(["Email", "Mailbox"]))
            .await
            .expect("sent");
        let Some(WebSocketMessage::StateChange(change)) = socket.next(WAIT).await.expect("a push")
        else {
            panic!("a state change");
        };
        assert!(change.touches("a1", ["Email"]));
        assert_eq!(
            change.push_state.as_deref(),
            Some(r#"after ["Email","Mailbox"]"#)
        );

        // A request-level error is a message, and the connection goes on.
        {
            use jmap_wire::websocket::WebSocket as _;
            socket
                .socket()
                .send("The quick brown fox".to_owned())
                .await
                .expect("sent");
        }
        let Some(WebSocketMessage::RequestError {
            request_id,
            problem,
        }) = socket.next(WAIT).await.expect("a message")
        else {
            panic!("a request error");
        };
        assert_eq!(request_id, None);
        assert_eq!(problem.kind(), ProblemKind::NotJson);

        socket.disable_push().await.expect("sent");
        assert!(matches!(
            socket.next(WAIT).await.expect("a message"),
            Some(WebSocketMessage::Unknown(text)) if text.contains("Vendor")
        ));

        // Nothing more: the wait runs out, and the connection is still
        // good for a request afterwards.
        let quiet = socket
            .next(Some(Duration::from_millis(200)))
            .await
            .expect_err("silence");
        assert!(quiet.is_timeout(), "{quiet}");
        let third = socket.send(&request).await.expect("sent");
        assert!(matches!(
            socket.next(WAIT).await.expect("a message"),
            Some(WebSocketMessage::Response { request_id: Some(id), .. }) if id == third
        ));

        socket.close().await.expect("closes");
        assert_eq!(socket.next(WAIT).await.expect("closed"), None);
    }

    #[tokio::test]
    async fn a_connection_the_server_closes_or_drops_ends_or_fails() {
        let closing = WsServer::start(
            true,
            |_| None,
            |mut socket| {
                let _ = socket.send("{\"@type\":\"StateChange\",\"changed\":{}}".into());
                let _ = socket.close(None);
                while socket.read().is_ok() {}
            },
        );
        let client = client_for(&closing.url, builder()).await;
        let mut socket = client
            .connect_websocket(&TungsteniteTransport::new())
            .await
            .expect("connects");
        assert!(matches!(
            socket.next(WAIT).await.expect("a push"),
            Some(WebSocketMessage::StateChange(_))
        ));
        assert_eq!(socket.next(WAIT).await.expect("a clean end"), None);

        let dropping = WsServer::start(true, |_| None, drop);
        let client = client_for(&dropping.url, builder()).await;
        let mut socket = client
            .connect_websocket(&TungsteniteTransport::new())
            .await
            .expect("connects");
        assert!(matches!(socket.next(WAIT).await, Err(Error::Transport(_))));
    }

    #[tokio::test]
    async fn a_handshake_the_server_refuses_is_its_status_and_a_token_is_refreshed_once() {
        let server = WsServer::start(
            true,
            |seen| match seen.header("authorization") {
                Some("Bearer token-2") => None,
                Some("Bearer gone") => Some(404),
                _ => Some(401),
            },
            jmap_socket,
        );

        let client = client_for(&server.url, builder()).await;
        let refused = client
            .connect_websocket(&TungsteniteTransport::new())
            .await
            .expect_err("wrong credentials");
        assert!(matches!(refused, Error::Http { status: 401 }), "{refused}");

        let gone = client_for(
            &server.url,
            Client::builder().credentials(Credentials::bearer("gone")),
        )
        .await
        .connect_websocket(&TungsteniteTransport::new())
        .await
        .expect_err("no WebSocket there");
        assert_eq!(gone.http_status(), Some(404));

        let refreshes = Arc::new(AtomicUsize::new(0));
        let client = client_for(
            &server.url,
            Client::builder().credentials(Credentials::bearer_source(Tokens(refreshes.clone()))),
        )
        .await;
        client
            .connect_websocket(&TungsteniteTransport::new())
            .await
            .expect("connects after one refresh");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        let sent = server
            .handshakes()
            .iter()
            .map(|seen| seen.header("authorization").unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(sent[sent.len() - 2..], ["Bearer token-1", "Bearer token-2"]);

        // A token that cannot be a header never leaves.
        let before = server.handshakes().len();
        let unsendable = client_for(
            &server.url,
            Client::builder().credentials(Credentials::None),
        )
        .await;
        let unsendable = Client::builder()
            .credentials(Credentials::bearer("token\r\nX-Evil: 1"))
            .with_session(
                ReqwestTransport::new().expect("a transport"),
                unsendable.session_url().clone(),
                unsendable.session().clone(),
            )
            .expect("a client");
        assert!(matches!(
            unsendable
                .connect_websocket(&TungsteniteTransport::new())
                .await,
            Err(Error::InvalidCredentials)
        ));
        assert_eq!(server.handshakes().len(), before);
    }

    #[tokio::test]
    async fn a_server_that_does_not_confirm_the_subprotocol_is_refused() {
        let server = WsServer::start(false, |_| None, jmap_socket);
        let client = client_for(&server.url, builder()).await;
        assert!(matches!(
            client.connect_websocket(&TungsteniteTransport::new()).await,
            Err(Error::Transport(_))
        ));
    }

    #[tokio::test]
    async fn a_message_over_the_limit_is_refused_and_a_dead_port_is_a_connect_error() {
        let server = WsServer::start(
            true,
            |_| None,
            |mut socket| {
                let large = json!({"@type": "StateChange", "changed": {},
                                   "padding": "x".repeat(100_000)});
                let _ = socket.send(large.to_string().into());
                while socket.read().is_ok() {}
            },
        );
        let client = client_for(&server.url, builder().max_response_bytes(16 * 1024)).await;
        let mut socket = client
            .connect_websocket(&TungsteniteTransport::new())
            .await
            .expect("connects");
        let error = socket.next(WAIT).await.expect_err("too large");
        assert!(error.is_response_too_large(), "{error}");

        let dead = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            format!("ws://{}/ws", listener.local_addr().expect("an address"))
        };
        let client = client_for(&dead, builder()).await;
        assert!(matches!(
            client.connect_websocket(&TungsteniteTransport::new()).await,
            Err(Error::Transport(error)) if error.kind() == TransportErrorKind::Connect
        ));

        // A session without the capability has no WebSocket to open.
        let plain = Server::start("127.0.0.1", |_, base| Reply::json(session(base, None)));
        let client = connect(&plain).await;
        assert!(matches!(
            client.connect_websocket(&TungsteniteTransport::new()).await,
            Err(Error::Session(_))
        ));
    }
}
