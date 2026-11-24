//! What a failed sign-in tells the user, and which account a sign-in
//! works in, pinned while sign-in moved from `jmap-client` to
//! `jmap-wire` (ADR 0008, step 4, #132): written and passing against
//! `jmap-client` first, then kept across the move, with the pinned text
//! changed only where the pull request says so.
//!
//! Each case signs in to a server on loopback that answers the session
//! request in one way, and the error's whole text (what the add-account
//! connection test and an account's "last error" show) is compared with
//! the text files in `sign_in_characterisation`.
//! `ZMAIL_UPDATE_SNAPSHOTS=1` rewrites them; review the diff.

use std::{
    fmt::Write as _,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use zmail_core::AuthKind;

use crate::{check_sign_in, connect_client};

/// What the server does with a request to the session's path.
#[derive(Clone)]
enum Answer {
    /// This status, content type and body.
    Http(u16, &'static str, String),
    /// A redirect (307) to this URL.
    Redirect(String),
    /// Nothing, for longer than the client waits.
    Hang,
}

struct Server {
    origin: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// A server that answers `answer` at `/jmap/session` and at
    /// `/.well-known/jmap`, and 404 anywhere else.
    fn start(answer: Answer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let origin = format!("http://{}", listener.local_addr().expect("an address"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (answer, log) = (answer.clone(), log.clone());
                std::thread::spawn(move || serve(stream, &answer, &log));
            }
        });
        Self { origin, seen }
    }

    fn shown(&self, text: &str) -> String {
        text.replace(self.origin.trim_start_matches("http://"), "server")
    }
}

fn serve(mut stream: TcpStream, answer: &Answer, seen: &Mutex<Vec<String>>) {
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
        if header.to_ascii_lowercase().starts_with("authorization:") {
            authorized = true;
        }
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    if !request_line.starts_with("GET ") {
        // A TLS handshake, say: not a request this server reads.
        return;
    }
    seen.lock().expect("the log").push(format!(
        "{target}{}",
        if authorized { " with credentials" } else { "" }
    ));
    let plain = |status: u16, headers: &str, body: &str| {
        format!(
            "HTTP/1.1 {status} Status\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let response = if !matches!(target.as_str(), "/jmap/session" | "/.well-known/jmap") {
        plain(404, "", "")
    } else {
        match answer {
            Answer::Http(status, content_type, body) => {
                plain(*status, &format!("Content-Type: {content_type}\r\n"), body)
            }
            Answer::Redirect(to) => plain(307, &format!("Location: {to}\r\n"), ""),
            Answer::Hang => {
                std::thread::sleep(Duration::from_secs(30));
                return;
            }
        }
    };
    let _ = stream.write_all(response.as_bytes());
}

/// A Session object with these accounts' ids and primary accounts.
fn session(origin: &str, accounts: &[&str], primary: Value) -> Value {
    let accounts = accounts
        .iter()
        .map(|id| {
            (
                (*id).to_owned(),
                json!({"name": format!("{id}@example.com"), "isPersonal": true,
                       "isReadOnly": false,
                       "accountCapabilities": {"urn:ietf:params:jmap:mail": {},
                                               "urn:ietf:params:jmap:submission": {}}}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {
                "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                "maxCallsInRequest": 16, "maxObjectsInGet": 250,
                "maxObjectsInSet": 500, "collationAlgorithms": []
            },
            "urn:ietf:params:jmap:mail": {},
            "urn:ietf:params:jmap:submission": {}
        },
        "accounts": accounts,
        "primaryAccounts": primary,
        "username": "rob@example.com",
        "apiUrl": format!("{origin}/api"),
        "downloadUrl": format!("{origin}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{origin}/upload/{{accountId}}/"),
        "eventSourceUrl": format!("{origin}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": "s1"
    })
}

/// Compares `actual` with the pinned text, or writes it when
/// `ZMAIL_UPDATE_SNAPSHOTS` is set.
fn assert_pinned(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/sign_in_characterisation")
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

/// Whether the add-account flow takes the text for the server refusing
/// the credentials (`zmail_setup::check::refused_text` looks for
/// "unauthorized", among other words no JMAP error has).
fn says_unauthorized(text: &str) -> bool {
    text.to_ascii_lowercase().contains("unauthorized")
}

/// Signs in to `path` on a server that answers `answer` and writes down
/// what came of it.
async fn case(text: &mut String, title: &str, path: &str, answer: Answer) {
    let server = Server::start(answer);
    let result = check_sign_in(
        &format!("{}{path}", server.origin),
        "rob@example.com",
        "pw",
        AuthKind::Password,
    )
    .await;
    writeln!(text, "# {title} (signing in to http://server{path})").expect("writes");
    match result {
        Ok(()) => writeln!(text, "  signed in").expect("writes"),
        Err(error) => {
            let error = server.shown(&format!("{error:#}"));
            writeln!(text, "  error: {error}").expect("writes");
            writeln!(
                text,
                "  taken for refused credentials: {}",
                says_unauthorized(&error)
            )
            .expect("writes");
        }
    }
    writeln!(
        text,
        "  requests: {:?}",
        server.seen.lock().expect("the log")
    )
    .expect("writes");
}

/// The errors a server's answer to the session request gives: wrong
/// credentials, a wrong URL, something that is not a session.
#[tokio::test]
async fn refused_sign_ins_say_what_they_said_before() {
    let mut text = String::new();
    let html = || Answer::Http(200, "text/html", "<html>Welcome</html>".to_owned());
    for (title, answer) in [
        (
            "wrong credentials (401)",
            Answer::Http(401, "text/plain", String::new()),
        ),
        (
            "a wrong URL (404)",
            Answer::Http(404, "text/plain", "not found".to_owned()),
        ),
        ("a page that is not JSON", html()),
        (
            "JSON that is not a session",
            Answer::Http(200, "application/json", "{\"hello\":true}".to_owned()),
        ),
        (
            "problem details (403)",
            Answer::Http(
                403,
                "application/problem+json",
                json!({"type": "about:blank", "status": 403, "title": "Forbidden",
                       "detail": "This account may not use JMAP"})
                .to_string(),
            ),
        ),
        (
            "a server error (503)",
            Answer::Http(503, "text/plain", String::new()),
        ),
    ] {
        for path in ["/jmap/session", ""] {
            case(&mut text, title, path, answer.clone()).await;
        }
    }
    assert_pinned("refused.txt", &text);
}

/// A session URL that redirects to another host is not followed: the
/// credentials would go with it.
#[tokio::test]
async fn a_session_that_moves_to_another_host_is_refused_as_before() {
    let mut text = String::new();
    let other = Server::start(Answer::Http(200, "text/html", "<html></html>".to_owned()));
    let other_authority = other
        .origin
        .trim_start_matches("http://")
        .replace("127.0.0.1", "localhost");
    case(
        &mut text,
        "a session URL that redirects to another host",
        "/jmap/session",
        Answer::Redirect(format!("http://{other_authority}/jmap/session")),
    )
    .await;
    let text = text.replace(&other_authority, "other");
    assert!(
        other
            .seen
            .lock()
            .expect("the log")
            .iter()
            .all(|seen| !seen.contains("credentials")),
        "the other host saw the credentials"
    );
    assert_pinned("redirected.txt", &text);
}

/// Endpoints that are not URLs a JMAP session can be at, and credentials
/// that cannot be sent: refused before any request.
#[tokio::test]
async fn unusable_endpoints_and_credentials_say_what_they_said_before() {
    let mut text = String::new();
    for endpoint in ["not a url", "ftp://example.com/jmap", "example.com"] {
        let error = check_sign_in(endpoint, "rob@example.com", "pw", AuthKind::Password)
            .await
            .expect_err("no such endpoint");
        writeln!(text, "# the endpoint {endpoint:?}\n  error: {error:#}").expect("writes");
    }
    let server = Server::start(Answer::Http(401, "text/plain", String::new()));
    for (title, secret, auth) in [
        (
            "an access token with a line break",
            "token\r\nX-Evil: 1",
            AuthKind::OAuth2,
        ),
        (
            "an access token with a control character",
            "tok\u{7f}en",
            AuthKind::OAuth2,
        ),
        ("a Proton account", "pw", AuthKind::ProtonSession),
    ] {
        let error = check_sign_in(&server.origin, "rob@example.com", secret, auth)
            .await
            .expect_err("refused");
        writeln!(
            text,
            "# {title}\n  error: {}",
            server.shown(&format!("{error:#}"))
        )
        .expect("writes");
        writeln!(
            text,
            "  requests: {:?}",
            server.seen.lock().expect("the log")
        )
        .expect("writes");
    }
    assert_pinned("unusable.txt", &text);
}

/// Failures of the connection itself. Their texts end in the operating
/// system's or the TLS library's own words, which differ between
/// platforms and versions, so only how they begin is held.
#[tokio::test]
async fn connection_failures_begin_as_they_did_before() {
    // Nothing listens on a port that was just closed.
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        format!("http://{}", listener.local_addr().expect("an address"))
    };
    let error = check_sign_in(
        &format!("{closed}/jmap/session"),
        "rob@example.com",
        "pw",
        AuthKind::Password,
    )
    .await
    .expect_err("nothing listens");
    let refused = format!("{error:#}").replace(closed.trim_start_matches("http://"), "server");
    println!("connection refused: {refused}");
    assert!(refused.starts_with(CONNECTION_REFUSED), "{refused}");

    // A server that does not speak TLS, asked over HTTPS.
    let server = Server::start(Answer::Http(200, "text/plain", String::new()));
    let https = server.origin.replace("http://", "https://");
    let error = check_sign_in(
        &format!("{https}/jmap/session"),
        "rob@example.com",
        "pw",
        AuthKind::Password,
    )
    .await
    .expect_err("no TLS");
    let tls = server.shown(&format!("{error:#}"));
    println!("TLS failure: {tls}");
    assert!(tls.starts_with(TLS_FAILURE), "{tls}");
}

/// How a connection that is refused is worded, up to the operating
/// system's own words.
const CONNECTION_REFUSED: &str =
    "failed to connect to the JMAP endpoint http://server/jmap/session: ";
/// How a failed TLS handshake is worded, up to the TLS library's own
/// words.
const TLS_FAILURE: &str = "failed to connect to the JMAP endpoint https://server/jmap/session: ";

/// A server that accepts the request and never answers: the sign-in
/// gives up after ten seconds.
#[tokio::test]
async fn a_server_that_never_answers_times_out_as_before() {
    let server = Server::start(Answer::Hang);
    let started = std::time::Instant::now();
    let error = check_sign_in(
        &format!("{}/jmap/session", server.origin),
        "rob@example.com",
        "pw",
        AuthKind::Password,
    )
    .await
    .expect_err("times out");
    let waited = started.elapsed();
    assert!(
        (Duration::from_secs(9)..Duration::from_secs(20)).contains(&waited),
        "waited {waited:?}"
    );
    let mut text = String::new();
    writeln!(text, "# a server that never answers").expect("writes");
    writeln!(text, "  error: {}", server.shown(&format!("{error:#}"))).expect("writes");
    assert_pinned("timeout.txt", &text);
}

/// Which account a sign-in works in, for sessions with one account,
/// with several, and with different primary accounts for mail and for
/// submission.
#[tokio::test]
async fn the_account_is_chosen_as_pinned() {
    let mut text = String::new();
    let mail = "urn:ietf:params:jmap:mail";
    let submission = "urn:ietf:params:jmap:submission";
    let contacts = "urn:ietf:params:jmap:contacts";
    for (title, accounts, primary) in [
        (
            "one account, primary for everything",
            vec!["a1"],
            json!({mail: "a1", submission: "a1", contacts: "a1"}),
        ),
        (
            "two accounts, one primary for everything",
            vec!["a1", "a2"],
            json!({mail: "a2", submission: "a2", contacts: "a2"}),
        ),
        (
            "different primary accounts for mail and for submission",
            vec!["a1", "a2"],
            json!({mail: "a1", submission: "a2"}),
        ),
        (
            "mail in one account, contacts and calendars in another",
            vec!["a1", "a2"],
            json!({mail: "a1", submission: "a1", contacts: "a2",
                   "urn:ietf:params:jmap:calendars": "a2",
                   "urn:ietf:params:jmap:principals": "a2",
                   "urn:ietf:params:jmap:quota": "a2"}),
        ),
        (
            "a primary account for contacts only",
            vec!["a1", "a2"],
            json!({contacts: "a2"}),
        ),
        ("no primary accounts", vec!["a1"], json!({})),
        ("no accounts at all", vec![], json!({})),
    ] {
        writeln!(text, "# {title}").expect("writes");
        writeln!(text, "  accounts: {accounts:?}").expect("writes");
        writeln!(
            text,
            "  primaryAccounts: {}",
            crate::write_characterisation_tests::canonical(&primary)
        )
        .expect("writes");
        // `jmap-client` took the first entry of a hash map whose order
        // changed from one sign-in to the next, so every account picked
        // in several sign-ins is listed (it was forty while the choice
        // was `jmap-client`'s; the pinned text then had two accounts
        // where it now has one).
        let mut picked = std::collections::BTreeSet::new();
        let mut failure = None;
        for _ in 0..5 {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let origin = format!("http://{}", listener.local_addr().expect("an address"));
            let body = session(&origin, &accounts, primary.clone()).to_string();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    serve(
                        stream,
                        &Answer::Http(200, "application/json", body.clone()),
                        &Mutex::new(Vec::new()),
                    );
                }
            });
            match connect_client(
                &format!("{origin}/jmap/session"),
                "rob@example.com",
                "pw",
                AuthKind::Password,
            )
            .await
            {
                Ok(client) => {
                    picked.insert(client.account_id().to_owned());
                }
                Err(error) => {
                    failure = Some(
                        format!("{error:#}")
                            .replace(origin.trim_start_matches("http://"), "server"),
                    );
                    break;
                }
            }
        }
        match failure {
            Some(error) => writeln!(text, "  sign-in fails: {error}").expect("writes"),
            None => writeln!(text, "  every call is made in: {picked:?}").expect("writes"),
        }
    }
    assert_pinned("accounts.txt", &text);
}
