//! What a push session sends and reports, pinned while push moved from
//! `jmap-client` to `jmap-wire` (ADR 0008, step 4, #132): written and
//! passing against `jmap-client` first, then kept across the move, with
//! the pinned text changed only where the pull request says so. It stays
//! as the regression test of both push transports.
//!
//! A server on loopback serves the session, an event source that follows
//! a script, and a WebSocket that answers requests and follows a script
//! once push is enabled. Zmail watches through [`watch_with`] with short
//! timings, and what the server received, the events reported and how
//! the session ended are compared with the text files in
//! `push_characterisation`. `ZMAIL_UPDATE_SNAPSHOTS=1` rewrites them;
//! review the diff, as any change is a change on the wire or in what the
//! sync is told.

use std::{
    fmt::Write as _,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use tokio_tungstenite::tungstenite;
use zmail_core::{AuthKind, BlobStore, BlobWriter, CachePolicySummary, ProtocolKind, SyncTarget};

use crate::{
    PushPreference,
    push::{PushTimings, watch_with},
    write_characterisation_tests::canonical,
};

/// Short enough for a test, long enough for a loaded CI runner: the
/// event source asks for a ping every second and counts as stalled half
/// a second later.
const TIMINGS: PushTimings = PushTimings {
    ping_interval: Duration::from_secs(1),
    stall_grace: Duration::from_millis(500),
    ws_ready_attempts: 3,
    ws_ready_wait: Duration::from_millis(400),
    connect_timeout: Duration::from_secs(10),
};

/// One step of an event source's or a WebSocket's script.
#[derive(Clone)]
enum Step {
    /// Send this text: bytes of the event stream, or one WebSocket text
    /// message.
    Send(String),
    /// End cleanly: the last chunk of the response, or a Close frame.
    Close,
    /// Drop the connection in the middle of the response.
    Abort,
    /// Stay connected and say nothing more. A WebSocket stops answering
    /// requests as well.
    Silent,
}

fn send(text: &str) -> Step {
    Step::Send(text.to_owned())
}

/// What the event source endpoint does.
#[derive(Clone)]
enum EventSource {
    /// Answers this status with no body.
    Status(u16),
    /// Answers 200 with JSON instead of an event stream.
    NotEventStream,
    /// Redirects (307) to this URL.
    Redirect(String),
    /// Answers an event stream that follows the script, then stays open.
    Stream(Vec<Step>),
}

/// What the WebSocket endpoint does.
#[derive(Clone)]
enum WebSocket {
    /// The session does not advertise the capability.
    None,
    /// The session advertises it without push.
    WithoutPush,
    /// The handshake is refused with this status.
    Refuse(u16),
    /// The connection closes as soon as it is open.
    CloseAtOnce,
    /// Requests are answered, but for the first `ignored` (frames Apache
    /// James loses right after the upgrade); the script runs after the
    /// answer to the first request that follows `WebSocketPushEnable`.
    Serve { ignored: usize, script: Vec<Step> },
}

struct PushServer {
    endpoint: String,
    seen: Arc<Mutex<Vec<String>>>,
}

/// Attachments are not part of push.
#[derive(Debug)]
struct NoBlobs;

impl BlobWriter for NoBlobs {
    fn write_blob(&self, logical_name: &str, _contents: &[u8]) -> std::io::Result<String> {
        Ok(logical_name.to_owned())
    }

    fn location(&self) -> &Path {
        Path::new("blobs")
    }
}

impl PushServer {
    /// Starts a server reached as `http://{host}:{port}`.
    fn start(host: &str, event_source: EventSource, websocket: WebSocket) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        let authority = format!("{host}:{port}");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let base = authority.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (base, log) = (base.clone(), log.clone());
                let (event_source, websocket) = (event_source.clone(), websocket.clone());
                std::thread::spawn(move || serve(stream, &base, &event_source, &websocket, &log));
            }
        });
        Self {
            endpoint: format!("http://{authority}"),
            seen,
        }
    }

    fn target(&self) -> SyncTarget {
        SyncTarget {
            id: 1,
            display_name: "Pushed".to_owned(),
            protocol: ProtocolKind::Jmap,
            auth: AuthKind::Password,
            endpoint: self.endpoint.clone(),
            username: "rob@example.com".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            identities: Vec::new(),
            blobs: BlobStore::new(Arc::new(NoBlobs)),
            checkpoint: None,
            oauth2: None,
        }
    }

    /// `text` with the server's address (its port changes from run to
    /// run) written as `server`.
    fn shown(&self, text: &str) -> String {
        text.replace(self.endpoint.trim_start_matches("http://"), "server")
    }

    /// Watches until the session ends and returns what the server
    /// received, the events reported and how it ended.
    async fn watch(&self, preference: PushPreference) -> String {
        let mut events = Vec::new();
        let target = self.target();
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            watch_with(&target, "pw", preference, TIMINGS, |event| {
                events.push(event);
            }),
        )
        .await;
        let mut text = String::new();
        for line in std::mem::take(&mut *self.seen.lock().expect("the log")) {
            writeln!(text, "  sent: {}", self.shown(&line)).expect("writes");
        }
        for event in events {
            writeln!(text, "  event: {event:?}").expect("writes");
        }
        match result {
            Ok(Ok(end)) => writeln!(text, "  end: {end:?}"),
            Ok(Err(error)) => writeln!(text, "  error: {}", self.shown(&format!("{error:#}"))),
            Err(_) => writeln!(text, "  still watching after a minute"),
        }
        .expect("writes");
        text
    }
}

/// The request line and headers of the request at the front of
/// `stream`, read without taking them off it, so that a WebSocket
/// handshake can still be read by the WebSocket library.
fn peek_head(stream: &TcpStream) -> String {
    let mut buffer = vec![0_u8; 16 * 1024];
    for _ in 0..200 {
        let Ok(read) = stream.peek(&mut buffer) else {
            break;
        };
        let head = String::from_utf8_lossy(&buffer[..read]);
        if read == 0 || head.contains("\r\n\r\n") {
            return head.into_owned();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    String::new()
}

/// The request as it is pinned: its line, and the headers that say
/// what the client asks for and whether it sent credentials.
fn request_line(head: &str) -> String {
    let mut lines = head.lines();
    let mut parts = lines.next().unwrap_or_default().split_whitespace();
    let (method, target) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let mut shown = Vec::new();
    for name in [
        "accept",
        "accept-encoding",
        "authorization",
        "cache-control",
        "last-event-id",
        "sec-websocket-protocol",
        "user-agent",
    ] {
        let value = head.lines().skip(1).find_map(|line| {
            let (header, value) = line.split_once(':')?;
            header
                .trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        });
        match (name, value) {
            ("authorization", Some(_)) => shown.push("authorization: yes".to_owned()),
            (_, Some(value)) => shown.push(format!("{name}: {value}")),
            _ => {}
        }
    }
    format!("{method} {target} [{}]", shown.join("; "))
}

fn serve(
    mut stream: TcpStream,
    authority: &str,
    event_source: &EventSource,
    websocket: &WebSocket,
    seen: &Mutex<Vec<String>>,
) {
    let head = peek_head(&stream);
    let target = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    seen.lock().expect("the log").push(request_line(&head));
    if target == "/ws" {
        serve_websocket(stream, websocket, seen);
        return;
    }
    // Take the request off the stream before answering.
    let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok() && !line.trim().is_empty() {
        line.clear();
    }
    let authorized = head.to_ascii_lowercase().contains("\nauthorization:");
    let plain = |status: &str, headers: &str, body: &str| {
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let response = if target == "/.well-known/jmap" && !authorized {
        plain(
            "401 Unauthorized",
            "WWW-Authenticate: Basic realm=\"jmap\"\r\n",
            "",
        )
    } else if target == "/.well-known/jmap" {
        plain(
            "200 OK",
            "Content-Type: application/json\r\n",
            &session(authority, websocket).to_string(),
        )
    } else if target.starts_with("/events-moved") {
        // Where a same-host redirect leads: a stream that ends at once.
        stream_events(stream, &[Step::Close]);
        return;
    } else if target.starts_with("/events") {
        match event_source {
            EventSource::Status(status) => plain(&format!("{status} Refused"), "", ""),
            EventSource::NotEventStream => {
                plain("200 OK", "Content-Type: application/json\r\n", "{}")
            }
            EventSource::Redirect(to) => {
                plain("307 Temporary Redirect", &format!("Location: {to}\r\n"), "")
            }
            EventSource::Stream(script) => {
                stream_events(stream, script);
                return;
            }
        }
    } else {
        plain("404 Not Found", "", "")
    };
    let _ = stream.write_all(response.as_bytes());
}

fn session(authority: &str, websocket: &WebSocket) -> Value {
    let mut capabilities = json!({
        "urn:ietf:params:jmap:core": {
            "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
            "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
            "maxCallsInRequest": 16, "maxObjectsInGet": 250,
            "maxObjectsInSet": 500, "collationAlgorithms": []
        },
        "urn:ietf:params:jmap:mail": {}
    });
    if !matches!(websocket, WebSocket::None) {
        capabilities["urn:ietf:params:jmap:websocket"] = json!({
            "url": format!("ws://{authority}/ws"),
            "supportsPush": !matches!(websocket, WebSocket::WithoutPush)
        });
    }
    json!({
        "capabilities": capabilities,
        "accounts": {
            "a1": {"name": "rob@example.com", "isPersonal": true, "isReadOnly": false,
                   "accountCapabilities": {"urn:ietf:params:jmap:mail": {}}},
            "a2": {"name": "shared@example.com", "isPersonal": false, "isReadOnly": true,
                   "accountCapabilities": {"urn:ietf:params:jmap:mail": {}}}
        },
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a1"},
        "username": "rob@example.com",
        "apiUrl": format!("http://{authority}/api"),
        "downloadUrl": format!("http://{authority}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("http://{authority}/upload/{{accountId}}/"),
        "eventSourceUrl": format!("http://{authority}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": "s1"
    })
}

/// Answers an event stream in chunks, one per scripted send.
fn stream_events(mut stream: TcpStream, script: &[Step]) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    for step in script {
        match step {
            Step::Send(text) => {
                let chunk = format!("{:x}\r\n{text}\r\n", text.len());
                if stream.write_all(chunk.as_bytes()).is_err() {
                    return;
                }
                let _ = stream.flush();
            }
            Step::Close => {
                let _ = stream.write_all(b"0\r\n\r\n");
                return;
            }
            Step::Abort => return,
            Step::Silent => break,
        }
    }
    until_closed(stream);
}

/// Holds the connection until the client drops it.
fn until_closed(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let mut buffer = [0_u8; 256];
    while matches!(stream.read(&mut buffer), Ok(read) if read > 0) {}
}

fn serve_websocket(stream: TcpStream, websocket: &WebSocket, seen: &Mutex<Vec<String>>) {
    use tungstenite::handshake::server::{ErrorResponse, Request, Response};

    let refuse = match websocket {
        WebSocket::Refuse(status) => Some(*status),
        _ => None,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    // The callback's error type is the library's.
    #[allow(clippy::result_large_err)]
    let accepted = tungstenite::accept_hdr(stream, |_: &Request, mut response: Response| {
        if let Some(status) = refuse {
            let mut refusal = ErrorResponse::new(None);
            *refusal.status_mut() =
                tungstenite::http::StatusCode::from_u16(status).expect("a status");
            return Err(refusal);
        }
        response.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            tungstenite::http::HeaderValue::from_static("jmap"),
        );
        Ok(response)
    });
    let Ok(mut socket) = accepted else { return };
    let (ignored, script) = match websocket {
        WebSocket::Serve { ignored, script } => (*ignored, script.as_slice()),
        _ => {
            let _ = socket.close(None);
            let _ = socket.flush();
            return;
        }
    };
    let (mut requests, mut enabled, mut scripted, mut silent) = (0, false, false, false);
    while let Ok(message) = socket.read() {
        let tungstenite::Message::Text(text) = message else {
            continue;
        };
        if silent {
            continue;
        }
        let frame: Value = serde_json::from_str(text.as_str()).unwrap_or(Value::Null);
        seen.lock()
            .expect("the log")
            .push(format!("ws: {}", canonical(&frame)));
        match frame["@type"].as_str() {
            Some("WebSocketPushEnable") => enabled = true,
            Some("Request") => {
                requests += 1;
                if requests <= ignored {
                    continue;
                }
                let call = &frame["methodCalls"][0];
                let answer = json!({
                    "@type": "Response",
                    "requestId": frame["id"],
                    "methodResponses": [[call[0], {"accountId": call[1]["accountId"],
                        "state": "m1", "list": [], "notFound": []}, call[2]]],
                    "sessionState": "s1"
                });
                if socket.send(answer.to_string().into()).is_err() {
                    return;
                }
                if enabled && !scripted {
                    scripted = true;
                    for step in script {
                        match step {
                            Step::Send(text) => {
                                if socket.send(text.clone().into()).is_err() {
                                    return;
                                }
                            }
                            Step::Close => {
                                let _ = socket.close(None);
                                let _ = socket.flush();
                            }
                            Step::Abort => return,
                            Step::Silent => silent = true,
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Compares `actual` with the pinned text, or writes it when
/// `ZMAIL_UPDATE_SNAPSHOTS` is set.
fn assert_pinned(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/push_characterisation")
        .join(name);
    if std::env::var_os("ZMAIL_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        std::fs::write(&path, actual).expect("the pinned text is written");
        return;
    }
    // A checkout that turned the line ends into CRLF pins the same text.
    let pinned = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| {
            panic!("{path:?}: {error}; run with ZMAIL_UPDATE_SNAPSHOTS=1 to write it")
        })
        .replace("\r\n", "\n");
    assert!(
        pinned == actual,
        "{name} differs from what is pinned.\n--- pinned\n{pinned}\n--- now\n{actual}"
    );
}

fn state_change(changed: Value) -> String {
    json!({"@type": "StateChange", "changed": changed}).to_string()
}

/// One event of an event stream.
fn event(name: &str, data: &str) -> Step {
    send(&format!("event: {name}\ndata: {data}\n\n"))
}

async fn scenario(
    text: &mut String,
    title: &str,
    event_source: EventSource,
    websocket: WebSocket,
    preference: PushPreference,
) {
    let server = PushServer::start("127.0.0.1", event_source, websocket);
    writeln!(text, "# {title} ({preference:?})").expect("writes");
    text.push_str(&server.watch(preference).await);
}

/// The event source: which events mean mail changed, and how a stream
/// that ends, breaks or goes quiet is reported.
#[tokio::test]
async fn the_event_source_reports_what_it_reported_before() {
    let mut text = String::new();
    let stream = EventSource::Stream(vec![
        send(": a comment the server sends to keep the connection open\n\n"),
        event("state", &state_change(json!({"a1": {"Email": "e2"}}))),
        event("state", &state_change(json!({"a2": {"Email": "e9"}}))),
        event("state", &state_change(json!({"a1": {"Identity": "i2"}}))),
        event("ping", "{\"interval\":300}"),
        event(
            "state",
            &state_change(json!({"a1": {"Mailbox": "m2", "Thread": "t2"}})),
        ),
        // No event field: the default event type.
        send(&format!(
            "data: {}\n\n",
            state_change(json!({"a1": {"EmailDelivery": "d2"}}))
        )),
        // With an id and a retry time, CRLF line ends, and data on two
        // lines.
        send(
            "id: 42\r\nretry: 5000\r\nevent: state\r\ndata: {\"@type\":\"StateChange\",\r\ndata: \"changed\":{\"a1\":{\"Email\":\"e3\"}}}\r\n\r\n",
        ),
        event("state", "not JSON"),
        event("state", "{\"@type\":\"PushVerification\"}"),
        event(
            "state",
            &state_change(json!({"a1": {"CalendarEvent": "c2"}})),
        ),
        event("other", &state_change(json!({"a1": {"Email": "e4"}}))),
        event("ping", "{}"),
        Step::Close,
    ]);
    scenario(
        &mut text,
        "a stream of events that ends",
        stream,
        WebSocket::None,
        PushPreference::Auto,
    )
    .await;
    scenario(
        &mut text,
        "a stream that goes quiet",
        EventSource::Stream(vec![
            event("state", &state_change(json!({"a1": {"Email": "e2"}}))),
            Step::Silent,
        ]),
        WebSocket::None,
        PushPreference::EventSource,
    )
    .await;
    scenario(
        &mut text,
        "a stream that closes at once",
        EventSource::Stream(vec![Step::Close]),
        WebSocket::None,
        PushPreference::Auto,
    )
    .await;
    assert_pinned("event_source.txt", &text);
}

/// A server whose event source cannot be opened: polled when it has no
/// WebSocket push either, an error when only the event source was asked
/// for.
#[tokio::test]
async fn an_event_source_that_does_not_open_is_treated_as_before() {
    let mut text = String::new();
    for (title, event_source) in [
        ("no event source (404)", EventSource::Status(404)),
        ("credentials refused (401)", EventSource::Status(401)),
        ("a server error (503)", EventSource::Status(503)),
        (
            "an answer that is no event stream",
            EventSource::NotEventStream,
        ),
    ] {
        for preference in [PushPreference::Auto, PushPreference::EventSource] {
            scenario(
                &mut text,
                title,
                event_source.clone(),
                WebSocket::None,
                preference,
            )
            .await;
        }
    }
    scenario(
        &mut text,
        "no event source, and a WebSocket without push",
        EventSource::Status(404),
        WebSocket::WithoutPush,
        PushPreference::Auto,
    )
    .await;
    scenario(
        &mut text,
        "no event source, and WebSocket push",
        EventSource::Status(404),
        WebSocket::Serve {
            ignored: 0,
            script: vec![
                send(&state_change(json!({"a1": {"Email": "e2"}}))),
                Step::Close,
            ],
        },
        PushPreference::Auto,
    )
    .await;
    assert_pinned("event_source_unavailable.txt", &text);
}

/// Redirects of the event source: followed on the session's host,
/// refused to another, which never sees the credentials.
#[tokio::test]
async fn event_source_redirects_stay_on_the_host() {
    let mut text = String::new();
    scenario(
        &mut text,
        "a redirect on the same host",
        EventSource::Redirect("/events-moved?types=Email".to_owned()),
        WebSocket::None,
        PushPreference::EventSource,
    )
    .await;

    let other = PushServer::start(
        "localhost",
        EventSource::Stream(vec![Step::Close]),
        WebSocket::None,
    );
    let server = PushServer::start(
        "127.0.0.1",
        EventSource::Redirect(format!("{}/events?types=Email", other.endpoint)),
        WebSocket::None,
    );
    for preference in [PushPreference::Auto, PushPreference::EventSource] {
        writeln!(text, "# a redirect to another host ({preference:?})").expect("writes");
        text.push_str(
            &server
                .watch(preference)
                .await
                .replace(other.endpoint.trim_start_matches("http://"), "other"),
        );
        let elsewhere = std::mem::take(&mut *other.seen.lock().expect("the log"));
        writeln!(text, "  the other host received: {elsewhere:?}").expect("writes");
    }
    assert_pinned("event_source_redirects.txt", &text);
}

/// WebSocket push: the handshake, the requests that make sure the
/// server is reading, enabling push, and which messages mean mail
/// changed.
#[tokio::test]
async fn the_websocket_reports_what_it_reported_before() {
    let mut text = String::new();
    let pushes = vec![
        send(&state_change(json!({"a1": {"Email": "e2"}}))),
        send(&state_change(json!({"a2": {"Email": "e9"}}))),
        send(&state_change(json!({"a1": {"Identity": "i2"}}))),
        send(
            &json!({"@type": "StateChange", "changed": {"a1": {"Mailbox": "m2"}},
                    "pushState": "p2"})
            .to_string(),
        ),
        // A data type from a later specification.
        send(&state_change(json!({"a1": {"FutureType": "f2"}}))),
        send(&state_change(json!({"a2": {"FutureType": "f3"}}))),
        send("not JSON"),
        send(&json!({"@type": "Unknown", "changed": {}}).to_string()),
        Step::Close,
    ];
    scenario(
        &mut text,
        "pushes, then the server closes",
        EventSource::Status(404),
        WebSocket::Serve {
            ignored: 0,
            script: pushes.clone(),
        },
        PushPreference::WebSocket,
    )
    .await;
    scenario(
        &mut text,
        "a server that loses the first two frames",
        EventSource::Status(404),
        WebSocket::Serve {
            ignored: 2,
            script: vec![
                send(&state_change(json!({"a1": {"Email": "e2"}}))),
                Step::Close,
            ],
        },
        PushPreference::WebSocket,
    )
    .await;
    scenario(
        &mut text,
        "a request-level error after push is enabled",
        EventSource::Status(404),
        WebSocket::Serve {
            ignored: 0,
            script: vec![send(
                &json!({"@type": "RequestError", "requestId": null,
                        "type": "urn:ietf:params:jmap:error:limit", "status": 400,
                        "limit": "maxConcurrentRequests", "detail": "Too many requests"})
                .to_string(),
            )],
        },
        PushPreference::WebSocket,
    )
    .await;
    assert_pinned("websocket.txt", &text);
}

/// A WebSocket that cannot be used: not offered, refused, never
/// answering, or going quiet.
#[tokio::test]
async fn a_websocket_that_does_not_work_is_treated_as_before() {
    let mut text = String::new();
    for (title, websocket) in [
        ("no WebSocket capability", WebSocket::None),
        ("a WebSocket without push", WebSocket::WithoutPush),
        ("a handshake refused (401)", WebSocket::Refuse(401)),
        ("a handshake refused (404)", WebSocket::Refuse(404)),
        (
            "a server that never answers",
            WebSocket::Serve {
                ignored: usize::MAX,
                script: Vec::new(),
            },
        ),
    ] {
        scenario(
            &mut text,
            title,
            EventSource::Status(404),
            websocket,
            PushPreference::WebSocket,
        )
        .await;
    }
    assert_pinned("websocket_unavailable.txt", &text);

    // A connection that closes or breaks is an error to retry. Its text
    // ends in the operating system's or the library's own words, so only
    // how it begins is held.
    for (websocket, connected) in [
        (WebSocket::CloseAtOnce, false),
        (
            WebSocket::Serve {
                ignored: 0,
                script: vec![
                    send(&state_change(json!({"a1": {"Email": "e2"}}))),
                    Step::Abort,
                ],
            },
            true,
        ),
    ] {
        let server = PushServer::start("127.0.0.1", EventSource::Status(404), websocket);
        let watched = server.watch(PushPreference::WebSocket).await;
        let end = watched.lines().last().unwrap_or_default();
        assert!(end.starts_with("  error: "), "{watched}");
        assert_eq!(
            watched.contains("  event: Connected(WebSocket)\n  event: MailChanged\n"),
            connected,
            "{watched}"
        );
    }
    let server = PushServer::start(
        "127.0.0.1",
        EventSource::Stream(vec![
            event("state", &state_change(json!({"a1": {"Email": "e2"}}))),
            Step::Abort,
        ]),
        WebSocket::None,
    );
    let watched = server.watch(PushPreference::Auto).await;
    assert!(
        watched.contains("  event: Connected(EventSource)\n  event: MailChanged\n")
            && watched
                .lines()
                .last()
                .is_some_and(|end| end.starts_with("  error: the JMAP event source failed: ")),
        "{watched}"
    );

    // How many heartbeats go out before the silence is noticed depends
    // on timing, so only the end is pinned.
    let server = PushServer::start(
        "127.0.0.1",
        EventSource::Status(404),
        WebSocket::Serve {
            ignored: 0,
            script: vec![Step::Silent],
        },
    );
    let watched = server.watch(PushPreference::WebSocket).await;
    assert!(
        watched.contains("  event: Connected(WebSocket)\n")
            && watched.ends_with("  error: the JMAP WebSocket was silent for 1s\n"),
        "{watched}"
    );
}
