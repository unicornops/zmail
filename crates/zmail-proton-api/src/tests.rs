//! Client tests against a local mock HTTPS server.
//!
//! The server speaks just enough HTTP/1.1 over rustls for these tests: one
//! request per connection, `Content-Length` bodies, `Connection: close`
//! replies. Its certificate comes from `testdata/` (a throwaway CA and a
//! leaf for `127.0.0.1`), so the client runs its real TLS path.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
};

use crate::{
    AppVersion, ClientConfig, Code, Error, Manager, PinSet, Status, manager::Request,
    response::RetryPolicy,
};

const CA_PEM: &[u8] = include_bytes!("testdata/ca.pem");
const LEAF_PEM: &[u8] = include_bytes!("testdata/leaf.pem");
const LEAF_KEY_PEM: &[u8] = include_bytes!("testdata/leaf-key.pem");
/// SPKI pin of `testdata/leaf.pem`.
const LEAF_PIN: &str = "w8Iej+c8CZbEzpfZigKNhMQF1/ZM9EOLj7sR10YYCMM=";

/// A request as the mock server saw it.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("JSON request body")
    }
}

/// A reply from the mock server.
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl Reply {
    fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    fn with_header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }
}

type Handler = dyn Fn(&Recorded, usize) -> Reply + Send + Sync;

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task: JoinHandle<()>,
}

impl MockServer {
    async fn start(handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        Self::start_on(listener, Arc::new(handler))
    }

    fn start_on(listener: TcpListener, handler: Arc<Handler>) -> Self {
        let port = listener.local_addr().expect("addr").port();
        let acceptor = TlsAcceptor::from(Arc::new(server_tls()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let handler = handler.clone();
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    serve_one(tls, handler, recorded).await;
                });
            }
        });
        Self {
            url: format!("https://127.0.0.1:{port}"),
            requests,
            task,
        }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("requests").clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn server_tls() -> ServerConfig {
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let chain = vec![CertificateDer::from_pem_slice(LEAF_PEM).expect("leaf")];
    let key = PrivateKeyDer::from_pem_slice(LEAF_KEY_PEM).expect("key");
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("server cert");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

async fn serve_one<S>(stream: S, handler: Arc<Handler>, recorded: Arc<Mutex<Vec<Recorded>>>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let request = Recorded {
        method,
        path,
        headers,
        body,
    };
    let index = {
        let mut all = recorded.lock().expect("requests");
        all.push(request.clone());
        all.len() - 1
    };
    let reply = handler(&request, index);
    let mut response = format!(
        "HTTP/1.1 {} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nDate: Tue, 20 Oct 2026 10:00:00 GMT\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(&reply.body);
    let stream = reader.get_mut();
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// A config for the mock server with fast, jitter-free retries.
fn test_config(url: &str, pins: Option<PinSet>) -> ClientConfig {
    let mut config = ClientConfig::with_custom_ca(url, CA_PEM, pins)
        .expect("config")
        .with_app_version(AppVersion::zmail("0.14.0").expect("version"));
    config.retry = RetryPolicy {
        max_retries: 3,
        max_wait: Duration::from_secs(2),
        fallback_after: Duration::from_millis(20),
        jitter: Duration::ZERO,
        backoff_base: Duration::from_millis(100),
    };
    config
}

fn user_reply() -> Reply {
    Reply::json(
        200,
        serde_json::json!({"Code": 1000, "User": {"ID": "user-1", "Name": "zmail", "Email": "zmail@proton.local", "MaxSpace": 1000}}),
    )
}

fn api_error(status: u16, code: i64, message: &str) -> Reply {
    Reply::json(status, serde_json::json!({"Code": code, "Error": message}))
}

#[tokio::test]
async fn sends_identity_session_and_auth_headers() {
    let server = MockServer::start(|_, _| user_reply()).await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid-1", "access-1", "refresh-1");

    let user = client.get_user().await.expect("user");
    assert_eq!(user.id, "user-1");
    assert_eq!(user.email, "zmail@proton.local");
    assert_eq!(user.max_space, 1000);

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/core/v4/users");
    assert_eq!(request.header("x-pm-appversion"), Some("zmail@0.14.0"));
    assert_eq!(request.header("x-pm-uid"), Some("uid-1"));
    assert_eq!(request.header("authorization"), Some("Bearer access-1"));
    assert_eq!(
        manager.server_time().map(|time| time.timestamp()),
        Some(1_792_490_400)
    );
}

#[tokio::test]
async fn error_codes_become_typed_errors_and_run_handlers() {
    let server = MockServer::start(|request, _| match request.path.as_str() {
        "/core/v4/users" => api_error(400, 5003, "Unsupported client"),
        "/paid" => api_error(422, 10004, "Paid plan required"),
        "/wrong" => api_error(422, 8002, "Incorrect login credentials"),
        "/hv" => Reply::json(
            422,
            serde_json::json!({"Code": 9001, "Error": "Human verification required", "Details": {"HumanVerificationToken": "token", "HumanVerificationMethods": ["captcha"]}}),
        ),
        _ => Reply {
            status: 500,
            headers: Vec::new(),
            body: "not json".to_owned(),
        },
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let app_version_errors = Arc::new(AtomicUsize::new(0));
    let seen = app_version_errors.clone();
    manager.add_error_handler(Code::APP_VERSION_BAD, move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    let client = manager.new_client("uid", "access", "refresh");

    let error = client.get_user().await.expect_err("5003");
    assert!(matches!(error, Error::AppVersion(_)), "{error:?}");
    assert_eq!(error.status(), Some(400));
    assert_eq!(app_version_errors.load(Ordering::SeqCst), 1);

    let send = |path: &str| {
        let request = Request::get(path);
        let client = &client;
        async move { client.send(&request).await.expect_err("error") }
    };
    assert!(matches!(send("/paid").await, Error::PaidPlanRequired(_)));
    assert!(matches!(send("/wrong").await, Error::PasswordWrong(_)));
    let hv = send("/hv").await;
    assert!(matches!(hv, Error::HumanVerificationRequired(_)));
    assert!(
        hv.api_error()
            .expect("api error")
            .details_to_string()
            .contains("\"HumanVerificationToken\":\"token\"")
    );
    let other = send("/other").await;
    assert!(matches!(other, Error::Api(_)));
    assert_eq!(other.code(), Some(Code(0)));
    assert_eq!(other.status(), Some(500));
}

#[tokio::test]
async fn refreshes_once_on_401_and_retries() {
    let server = MockServer::start(|request, _| match request.path.as_str() {
        "/auth/v4/refresh" => Reply::json(
            200,
            serde_json::json!({"Code": 1000, "UID": "uid", "AccessToken": "access-2", "RefreshToken": "refresh-2", "Scope": "full"}),
        ),
        _ if request.header("authorization") == Some("Bearer access-1") => {
            api_error(401, 401, "Invalid access token")
        }
        _ => user_reply(),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access-1", "refresh-1");
    let refreshed = Arc::new(Mutex::new(Vec::new()));
    let sink = refreshed.clone();
    client.add_auth_handler(move |auth| {
        sink.lock().expect("auth").push(auth.refresh_token.clone());
    });

    client.get_user().await.expect("user after refresh");
    client.get_user().await.expect("user with new token");

    let requests = server.requests();
    let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/core/v4/users",
            "/auth/v4/refresh",
            "/core/v4/users",
            "/core/v4/users"
        ]
    );
    let refresh = requests[1].json();
    assert_eq!(refresh["UID"], "uid");
    assert_eq!(refresh["RefreshToken"], "refresh-1");
    assert_eq!(refresh["AccessToken"], "access-1");
    assert_eq!(refresh["GrantType"], "refresh_token");
    assert_eq!(refresh["ResponseType"], "token");
    assert_eq!(refresh["RedirectURI"], "https://protonmail.ch");
    assert!(
        refresh["State"]
            .as_str()
            .is_some_and(|state| !state.is_empty())
    );
    assert_eq!(requests[1].header("authorization"), None);
    assert_eq!(requests[2].header("authorization"), Some("Bearer access-2"));
    assert_eq!(requests[3].header("authorization"), Some("Bearer access-2"));
    assert_eq!(*refreshed.lock().expect("auth"), ["refresh-2"]);
}

#[tokio::test]
async fn second_401_is_returned_without_another_refresh() {
    let server = MockServer::start(|request, _| match request.path.as_str() {
        "/auth/v4/refresh" => Reply::json(
            200,
            serde_json::json!({"UID": "uid", "AccessToken": "access-2", "RefreshToken": "refresh-2"}),
        ),
        _ => api_error(401, 401, "Invalid access token"),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access-1", "refresh-1");

    let error = client.get_user().await.expect_err("401");
    assert_eq!(error.status(), Some(401));
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn rejected_refresh_deauthenticates_once() {
    let server = MockServer::start(|request, _| match request.path.as_str() {
        "/auth/v4/refresh" => api_error(422, 10013, "Invalid refresh token"),
        _ => api_error(401, 401, "Invalid access token"),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access-1", "refresh-1");
    let deauths = Arc::new(AtomicUsize::new(0));
    let seen = deauths.clone();
    client.add_deauth_handler(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });

    for _ in 0..2 {
        let error = client.get_user().await.expect_err("deauth");
        let Error::Deauthenticated(inner) = &error else {
            panic!("expected Deauthenticated, got {error:?}");
        };
        assert!(matches!(**inner, Error::RefreshTokenInvalid(_)));
        assert_eq!(error.code(), Some(Code::AUTH_REFRESH_TOKEN_INVALID));
    }
    assert_eq!(deauths.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn server_errors_during_refresh_do_not_deauthenticate() {
    let server = MockServer::start(|request, _| match request.path.as_str() {
        "/auth/v4/refresh" => api_error(500, 0, "Internal error"),
        _ => api_error(401, 401, "Invalid access token"),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access-1", "refresh-1");
    let deauths = Arc::new(AtomicUsize::new(0));
    let seen = deauths.clone();
    client.add_deauth_handler(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });

    let error = client.get_user().await.expect_err("500");
    assert!(matches!(error, Error::Api(_)), "{error:?}");
    assert_eq!(deauths.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn new_client_with_refresh_resumes_a_session() {
    let server = MockServer::start(|request, _| match request.path.as_str() {
        "/auth/v4/refresh" => Reply::json(
            200,
            serde_json::json!({"UID": "uid", "AccessToken": "access-2", "RefreshToken": "refresh-2"}),
        ),
        _ => user_reply(),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let (client, auth) = manager
        .new_client_with_refresh("uid", "refresh-1")
        .await
        .expect("resume");
    assert_eq!(auth.refresh_token, "refresh-2");
    assert_eq!(client.uid(), "uid");
    client.get_user().await.expect("user");
    let requests = server.requests();
    assert!(requests[0].json().get("AccessToken").is_none());
    assert_eq!(requests[1].header("authorization"), Some("Bearer access-2"));
}

#[tokio::test]
async fn retries_429_and_503_honouring_retry_after() {
    let server = MockServer::start(|_, index| match index {
        0 => api_error(429, 2028, "Too many requests").with_header("Retry-After", "0"),
        1 => api_error(503, 0, "Unavailable"),
        _ => user_reply(),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");

    client.get_user().await.expect("user after retries");
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn gives_up_after_the_retry_budget() {
    let server =
        MockServer::start(|_, _| api_error(503, 0, "Unavailable").with_header("Retry-After", "0"))
            .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");

    let error = client.get_user().await.expect_err("503");
    assert_eq!(error.status(), Some(503));
    // The first attempt plus three retries, as resty's SetRetryCount(3).
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn retries_failed_connects_and_reports_status() {
    // Reserve a port, close it, and only start the server after the first
    // connection attempt has been refused.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let url = format!("https://127.0.0.1:{port}");
    let manager = Manager::new(test_config(&url, None)).expect("manager");
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let sink = statuses.clone();
    manager.add_status_observer(move |status| sink.lock().expect("status").push(status));
    let client = manager.new_client("uid", "access", "refresh");

    let request = tokio::spawn(async move { client.get_user().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("rebind");
    let server = MockServer::start_on(listener, Arc::new(|_: &Recorded, _| user_reply()));

    request.await.expect("task").expect("user after reconnect");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        *statuses.lock().expect("status"),
        [Status::Down, Status::Up]
    );
    assert_eq!(manager.status(), Status::Up);
}

#[tokio::test]
async fn connection_errors_surface_as_network_errors() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let manager =
        Manager::new(test_config(&format!("https://127.0.0.1:{port}"), None)).expect("manager");
    let error = manager.ping().await.expect_err("nothing listening");
    assert!(error.is_network(), "{error:?}");
    assert_eq!(manager.status(), Status::Down);
}

#[tokio::test]
async fn keeps_and_sends_cookies() {
    let server = MockServer::start(|_, index| match index {
        0 => user_reply().with_header("Set-Cookie", "Session-Id=abc; Path=/; Secure; HttpOnly"),
        _ => user_reply(),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");

    client.get_user().await.expect("first");
    client.get_user().await.expect("second");
    let requests = server.requests();
    assert_eq!(requests[0].header("cookie"), None);
    assert_eq!(requests[1].header("cookie"), Some("Session-Id=abc"));
}

#[tokio::test]
async fn accepts_a_pinned_leaf() {
    let server = MockServer::start(|_, _| user_reply()).await;
    let pins = PinSet::parse(&format!("pin-sha256=\"{LEAF_PIN}\"")).expect("pin");
    let manager = Manager::new(test_config(&server.url, Some(pins))).expect("manager");
    manager
        .new_client("uid", "access", "refresh")
        .get_user()
        .await
        .expect("pinned connection");
}

#[tokio::test]
async fn refuses_an_unpinned_leaf_without_retrying() {
    let server = MockServer::start(|_, _| user_reply()).await;
    let manager =
        Manager::new(test_config(&server.url, Some(PinSet::trusted_api_pins()))).expect("manager");
    let error = manager
        .new_client("uid", "access", "refresh")
        .get_user()
        .await
        .expect_err("pin mismatch");
    assert!(matches!(error, Error::PinMismatch(_)), "{error:?}");
    // The handshake failed, so the server never saw a request.
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn refuses_a_certificate_from_another_ca_even_when_pinned() {
    let server = MockServer::start(|_, _| user_reply()).await;
    // The leaf's own pin, but a different trusted CA: the chain must verify
    // before the pin counts.
    let pin: [u8; 32] = BASE64
        .decode(LEAF_PIN)
        .expect("pin")
        .try_into()
        .expect("32 bytes");
    let config = ClientConfig::with_custom_ca(
        &server.url,
        include_bytes!("testdata/other-ca.pem"),
        Some(PinSet::from_digests([pin])),
    )
    .expect("config");
    let manager = Manager::new(config).expect("manager");
    let error = manager.ping().await.expect_err("untrusted chain");
    assert!(matches!(error, Error::Certificate(_)), "{error:?}");
    assert!(server.requests().is_empty());
}
