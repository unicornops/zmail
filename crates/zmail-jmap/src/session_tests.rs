//! Signing in with each shape of JMAP endpoint Zmail stores, against
//! servers on loopback that answer the session at one path only: a bare
//! origin (the live harness), a `/.well-known/jmap` URL (the setup form's
//! placeholder and the DNS SRV step), any other session URL (the
//! well-known step's URL after redirects, Fastmail's preset) and a
//! `/.well-known/jmap` that redirects to another host (RFC 8620 section
//! 2.2).

use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

use serde_json::json;
use zmail_core::AuthKind;

use crate::check_sign_in;

/// One request a server received: its request target and whether it
/// carried an `Authorization` header.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    target: String,
    authorized: bool,
}

/// A JMAP server on loopback that answers the session at `session_path`
/// only, redirects `redirect.0` to `redirect.1`, and answers 404 to
/// anything else.
struct SessionServer {
    origin: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl SessionServer {
    /// Starts a server reached as `http://{host}:{port}`; `host` is the
    /// name the client uses for it (`127.0.0.1` or `localhost`), so two
    /// servers can be two hosts.
    fn start(host: &str, session_path: &str, redirect: Option<(&str, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        let origin = format!("http://{host}:{port}");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (log, base, session_path) = (seen.clone(), origin.clone(), session_path.to_owned());
        let redirect = redirect.map(|(from, to)| (from.to_owned(), to));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                serve(stream, &base, &session_path, redirect.as_ref(), &log);
            }
        });
        Self { origin, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the log").clone()
    }

    fn targets(&self) -> Vec<String> {
        self.seen().into_iter().map(|seen| seen.target).collect()
    }
}

/// Answers one request and closes the connection. Every response says
/// `Connection: close`, so the client never sends its next request (the
/// one a redirect leads to, say) down a connection this server has
/// already dropped.
fn serve(
    mut stream: TcpStream,
    base: &str,
    session_path: &str,
    redirect: Option<&(String, String)>,
    seen: &Mutex<Vec<Seen>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut authorized = false;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, _)) = header.split_once(':')
            && name.eq_ignore_ascii_case("authorization")
        {
            authorized = true;
        }
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    seen.lock().expect("the log").push(Seen {
        target: target.clone(),
        authorized,
    });
    let response = if let Some((_, to)) = redirect.filter(|(from, _)| *from == target) {
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    } else if target == session_path && !authorized {
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"jmap\"\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
            .to_owned()
    } else if target == session_path {
        let body = json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {
                    "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                    "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                    "maxCallsInRequest": 16, "maxObjectsInGet": 250,
                    "maxObjectsInSet": 500, "collationAlgorithms": []
                },
                "urn:ietf:params:jmap:mail": {}
            },
            "accounts": {"a1": {"name": "rob@example.com", "isPersonal": true,
                                 "isReadOnly": false, "accountCapabilities": {}}},
            "primaryAccounts": {"urn:ietf:params:jmap:mail": "a1"},
            "username": "rob@example.com",
            "apiUrl": format!("{base}/api"),
            "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
            "uploadUrl": format!("{base}/upload/{{accountId}}/"),
            "eventSourceUrl": format!("{base}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
            "state": "s1"
        })
        .to_string();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    } else {
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
    };
    let _ = stream.write_all(response.as_bytes());
}

async fn sign_in(endpoint: &str) -> anyhow::Result<()> {
    check_sign_in(endpoint, "rob@example.com", "pw", AuthKind::Password).await
}

#[tokio::test]
async fn a_bare_origin_signs_in_at_its_well_known_url() {
    let server = SessionServer::start("127.0.0.1", "/.well-known/jmap", None);
    sign_in(&server.origin).await.expect("signs in");
    sign_in(&format!("{}/", server.origin))
        .await
        .expect("signs in with a trailing slash");
    assert!(
        server
            .targets()
            .iter()
            .all(|target| target == "/.well-known/jmap"),
        "{:?}",
        server.targets()
    );
}

#[tokio::test]
async fn a_well_known_url_is_not_given_a_second_well_known_path() {
    let server = SessionServer::start("127.0.0.1", "/.well-known/jmap", None);
    sign_in(&format!("{}/.well-known/jmap", server.origin))
        .await
        .expect("signs in");
    assert!(
        server
            .targets()
            .iter()
            .all(|target| target == "/.well-known/jmap"),
        "{:?}",
        server.targets()
    );
}

#[tokio::test]
async fn any_other_session_url_is_fetched_as_it_is() {
    let server = SessionServer::start("127.0.0.1", "/jmap/session", None);
    sign_in(&format!("{}/jmap/session", server.origin))
        .await
        .expect("signs in");
    assert_eq!(server.targets(), ["/jmap/session"]);
}

#[tokio::test]
async fn a_same_host_well_known_redirect_is_followed() {
    let server = SessionServer::start(
        "127.0.0.1",
        "/jmap/session",
        Some(("/.well-known/jmap", "/jmap/session".to_owned())),
    );
    sign_in(&server.origin).await.expect("signs in");
    assert!(
        server.seen().contains(&Seen {
            target: "/jmap/session".to_owned(),
            authorized: true,
        }),
        "{:?}",
        server.seen()
    );
}

#[tokio::test]
async fn a_well_known_redirect_to_another_host_signs_in_there() {
    let api = SessionServer::start("localhost", "/jmap/session", None);
    let domain = SessionServer::start(
        "127.0.0.1",
        "/unused",
        Some(("/.well-known/jmap", format!("{}/jmap/session", api.origin))),
    );
    sign_in(&format!("{}/.well-known/jmap", domain.origin))
        .await
        .expect("signs in");
    // The domain only redirects: it never sees the credentials.
    assert!(
        domain.seen().iter().all(|seen| !seen.authorized),
        "{:?}",
        domain.seen()
    );
    assert!(
        api.seen().contains(&Seen {
            target: "/jmap/session".to_owned(),
            authorized: true,
        }),
        "{:?}",
        api.seen()
    );
}

#[tokio::test]
async fn a_redirect_that_does_not_end_at_jmap_gets_no_credentials() {
    // A sign-in page, say: it answers neither 401 nor a session.
    let elsewhere = SessionServer::start("localhost", "/unused", None);
    let domain = SessionServer::start(
        "127.0.0.1",
        "/unused",
        Some(("/.well-known/jmap", format!("{}/login", elsewhere.origin))),
    );
    sign_in(&domain.origin)
        .await
        .expect_err("there is no session to sign in to");
    assert!(
        elsewhere.seen().iter().all(|seen| !seen.authorized),
        "{:?}",
        elsewhere.seen()
    );
}

#[tokio::test]
async fn an_access_token_that_cannot_be_a_header_is_an_error() {
    let server = SessionServer::start("127.0.0.1", "/.well-known/jmap", None);
    let error = check_sign_in(
        &server.origin,
        "rob",
        "token\r\nX-Evil: 1",
        AuthKind::OAuth2,
    )
    .await
    .expect_err("refused before connecting");
    assert!(error.to_string().contains("access token"), "{error}");
    assert!(server.seen().is_empty(), "{:?}", server.seen());
}

#[test]
fn redirects_never_leave_https() {
    let https = url::Url::parse("https://example.com/.well-known/jmap").expect("a URL");
    let http = url::Url::parse("http://127.0.0.1:4080/.well-known/jmap").expect("a URL");
    assert_eq!(
        crate::next_session_hop(&https, "https://api.example.com/jmap/session#top")
            .expect("followed")
            .as_str(),
        "https://api.example.com/jmap/session"
    );
    assert_eq!(
        crate::next_session_hop(&https, "/jmap/session")
            .expect("followed")
            .as_str(),
        "https://example.com/jmap/session"
    );
    crate::next_session_hop(&https, "http://api.example.com/jmap/session")
        .expect_err("no downgrade");
    crate::next_session_hop(&https, "ftp://api.example.com/").expect_err("HTTP(S) only");
    crate::next_session_hop(&http, "http://127.0.0.1:4080/jmap/session").expect("still HTTP");
}

#[test]
fn jmap_client_is_given_a_url_it_fetches_the_session_from_as_it_is() {
    let given = |session: &str| crate::jmap_client_url(&url::Url::parse(session).expect("a URL"));
    assert_eq!(
        given("http://127.0.0.1:4080/.well-known/jmap"),
        "http://127.0.0.1:4080"
    );
    assert_eq!(
        given("https://api.fastmail.com/jmap/session"),
        "https://api.fastmail.com/jmap/session#"
    );
    // What jmap-client's session URL resolves relative URLs against, as
    // push does for `eventSourceUrl`, is the session URL.
    let appended = format!(
        "{}/.well-known/jmap",
        given("https://api.example.com/jmap/session")
    );
    let base = url::Url::parse(&appended).expect("a URL");
    assert_eq!(base.path(), "/jmap/session");
    assert_eq!(
        base.join("events?ping=30").expect("joins").as_str(),
        "https://api.example.com/jmap/events?ping=30"
    );
}
