//! Client tests against the local mock HTTPS server ([`crate::mock`]).

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use tokio::{io::AsyncReadExt, net::TcpListener};

use crate::{
    AppVersion, ClientConfig, Code, Error, Manager, PinSet, Status,
    manager::Request,
    mock::{LEAF_PIN, MockServer, Recorded, Reply},
};

/// A config for the mock server with fast, jitter-free retries.
fn test_config(url: &str, pins: Option<PinSet>) -> ClientConfig {
    crate::mock::config(url, pins)
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
    let identity = AppVersion::zmail("0.14.0").expect("version");
    assert_eq!(request.header("x-pm-appversion"), Some(identity.as_str()));
    assert_eq!(request.header("user-agent"), Some(identity.user_agent()));
    assert_eq!(request.header("x-pm-uid"), Some("uid-1"));
    assert_eq!(request.header("authorization"), Some("Bearer access-1"));
    assert_eq!(
        manager.server_time().map(|time| time.seconds()),
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
        _ => Reply::bytes(500, "not json"),
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
        sink.lock()
            .expect("auth")
            .push(auth.refresh_token.as_str().to_owned());
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
    assert_eq!(auth.refresh_token.as_str(), "refresh-2");
    assert_eq!(client.uid(), "uid");
    client.get_user().await.expect("user");
    let requests = server.requests();
    assert!(requests[0].json().get("AccessToken").is_none());
    assert_eq!(requests[1].header("authorization"), Some("Bearer access-2"));
}

/// Compiles only for a type that wipes itself when dropped.
fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>(_: &T) {}

#[tokio::test]
async fn refresh_and_close_wipe_the_replaced_tokens() {
    let server = MockServer::start(|request, _| match request.path.as_str() {
        "/auth/v4/refresh" => Reply::json(
            200,
            serde_json::json!({"Code": 1000, "UID": "uid", "AccessToken": "access-2", "RefreshToken": "refresh-2"}),
        ),
        _ if request.header("authorization") == Some("Bearer access-1") => {
            api_error(401, 401, "Invalid access token")
        }
        _ => user_reply(),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access-1", "refresh-1");
    client.add_auth_handler(|auth| {
        // The new tokens reach the handlers in wiping buffers too.
        wiped_on_drop(&auth.access_token);
        wiped_on_drop(&auth.refresh_token);
    });

    let (access_1, refresh_1) = client.token_handles();
    {
        let access = access_1.upgrade().expect("access token held");
        let refresh = refresh_1.upgrade().expect("refresh token held");
        assert_eq!(access.as_str(), "access-1");
        assert_eq!(refresh.as_str(), "refresh-1");
        wiped_on_drop(&*access);
        wiped_on_drop(&*refresh);
    }

    // A 401 refreshes the session: the replaced tokens are dropped, and
    // so wiped, with no copy of them left anywhere.
    client.get_user().await.expect("user after refresh");
    assert!(access_1.upgrade().is_none(), "the old access token is kept");
    assert!(
        refresh_1.upgrade().is_none(),
        "the old refresh token is kept"
    );
    let (access_2, refresh_2) = client.token_handles();
    assert_eq!(
        access_2.upgrade().expect("new access token").as_str(),
        "access-2"
    );
    assert_eq!(
        refresh_2.upgrade().expect("new refresh token").as_str(),
        "refresh-2"
    );

    // Closing the client wipes the tokens it held.
    client.close();
    assert!(access_2.upgrade().is_none(), "close keeps the access token");
    assert!(
        refresh_2.upgrade().is_none(),
        "close keeps the refresh token"
    );
    let (access, refresh) = client.token_handles();
    assert!(access.upgrade().is_some_and(|token| token.is_empty()));
    assert!(refresh.upgrade().is_some_and(|token| token.is_empty()));
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

/// A fake Proton login endpoint: the SRP server side from `proton-srp`
/// with go-srp's signed test modulus, so the client's whole login path
/// (modulus signature, proofs, server proof) runs for real.
struct FakeSrpServer {
    salt: String,
    verifier: proton_srp::ServerClientVerifier,
    session: Mutex<Option<proton_srp::ServerInteraction>>,
}

const PASSWORD: &str = "correct horse battery staple";

impl FakeSrpServer {
    fn new() -> Arc<Self> {
        let verifier: proton_srp::SRPVerifierB64 = proton_srp::SRPAuth::generate_verifier(
            &crate::srp_modulus::ModulusVerifier,
            PASSWORD,
            None,
            crate::srp_modulus::tests::TEST_MODULUS_CLEAR_SIGN,
        )
        .expect("verifier")
        .into();
        Arc::new(Self {
            salt: verifier.salt.clone(),
            verifier: proton_srp::ServerClientVerifier::try_from(&verifier).expect("verifier"),
            session: Mutex::new(None),
        })
    }

    fn info(&self) -> Reply {
        let mut session = proton_srp::ServerInteraction::new_with_modulus_extractor(
            &crate::srp_modulus::ModulusVerifier,
            crate::srp_modulus::tests::TEST_MODULUS_CLEAR_SIGN,
            &self.verifier,
        )
        .expect("server session");
        let challenge = session.generate_challenge();
        *self.session.lock().expect("session") = Some(session);
        // The fake server omits `Code` here; the client must cope.
        Reply::json(
            200,
            serde_json::json!({
                "Version": 4,
                "Modulus": crate::srp_modulus::tests::TEST_MODULUS_CLEAR_SIGN,
                "ServerEphemeral": challenge.encode_b64(),
                "Salt": self.salt,
                "SRPSession": "srp-session-1",
            }),
        )
    }

    fn auth(&self, request: &Recorded, server_proof_override: Option<&str>) -> Reply {
        let body = request.json();
        let proof = proton_srp::ServerClientProof::new(
            body["ClientEphemeral"].as_str().expect("ephemeral"),
            body["ClientProof"].as_str().expect("proof"),
        )
        .expect("proof");
        let mut session = self.session.lock().expect("session");
        let Some(session) = session.as_mut() else {
            return api_error(422, 2001, "No SRP session");
        };
        match session.verify_proof(&proof) {
            Ok(server_proof) => Reply::json(
                200,
                serde_json::json!({
                    "Code": 1000,
                    "UserID": "user-1",
                    "UID": "uid-1",
                    "AccessToken": "access-1",
                    "RefreshToken": "refresh-1",
                    "ServerProof": server_proof_override
                        .map_or_else(|| server_proof.encode_b64(), str::to_owned),
                    "Scope": "full mail",
                    "2FA": {"Enabled": 1, "FIDO2": {"AuthenticationOptions": null, "RegisteredKeys": []}},
                    "PasswordMode": 1,
                }),
            ),
            Err(_) => api_error(422, 8002, "Incorrect login credentials"),
        }
    }
}

#[tokio::test]
async fn logs_in_with_srp_and_checks_the_server_proof() {
    let srp = FakeSrpServer::new();
    let fake = srp.clone();
    let server = MockServer::start(move |request, _| match request.path.as_str() {
        "/auth/v4/info" => fake.info(),
        "/auth/v4" if request.method == "POST" => fake.auth(request, None),
        "/auth/v4/2fa" => Reply::json(200, serde_json::json!({"Code": 1000, "Scope": "full mail"})),
        "/auth/v4" if request.method == "DELETE" => {
            Reply::json(200, serde_json::json!({"Code": 1000}))
        }
        _ => user_reply(),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");

    let (client, auth) = manager.login("zmail", PASSWORD).await.expect("login");
    assert_eq!(auth.uid, "uid-1");
    assert_eq!(auth.user_id, "user-1");
    assert!(auth.two_fa.enabled.has_totp());
    assert_eq!(client.uid(), "uid-1");
    client.auth_2fa("123456").await.expect("2fa");
    client.get_user().await.expect("user");
    client.auth_delete().await.expect("logout");

    let requests = server.requests();
    let calls: Vec<(&str, &str)> = requests
        .iter()
        .map(|r| (r.method.as_str(), r.path.as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            ("POST", "/auth/v4/info"),
            ("POST", "/auth/v4"),
            ("POST", "/auth/v4/2fa"),
            ("GET", "/core/v4/users"),
            ("DELETE", "/auth/v4"),
        ]
    );
    assert_eq!(requests[0].json(), serde_json::json!({"Username": "zmail"}));
    let login = requests[1].json();
    assert_eq!(login["Username"], "zmail");
    assert_eq!(login["SRPSession"], "srp-session-1");
    assert!(login.get("TwoFactorCode").is_none());
    assert_eq!(login["FIDO2"]["ClientData"], "");
    // The password never leaves the client.
    assert!(!String::from_utf8_lossy(&requests[1].body).contains(PASSWORD));
    for request in &requests[..2] {
        assert_eq!(request.header("x-pm-uid"), None);
        assert_eq!(request.header("authorization"), None);
        let identity = AppVersion::zmail("0.14.0").expect("version");
        assert_eq!(request.header("x-pm-appversion"), Some(identity.as_str()));
        assert_eq!(request.header("user-agent"), Some(identity.user_agent()));
    }
    assert_eq!(requests[2].json()["TwoFactorCode"], "123456");
    for request in &requests[2..] {
        assert_eq!(request.header("x-pm-uid"), Some("uid-1"));
        assert_eq!(request.header("authorization"), Some("Bearer access-1"));
    }
}

#[tokio::test]
async fn wrong_password_is_a_typed_error() {
    let srp = FakeSrpServer::new();
    let fake = srp.clone();
    let server = MockServer::start(move |request, _| match request.path.as_str() {
        "/auth/v4/info" => fake.info(),
        _ => fake.auth(request, None),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let error = manager
        .login("zmail", "not the password")
        .await
        .expect_err("wrong password");
    assert!(matches!(error, Error::PasswordWrong(_)), "{error:?}");
}

#[tokio::test]
async fn refuses_a_server_that_cannot_prove_the_password() {
    let srp = FakeSrpServer::new();
    let fake = srp.clone();
    let server = MockServer::start(move |request, _| match request.path.as_str() {
        "/auth/v4/info" => fake.info(),
        _ => fake.auth(request, Some(&BASE64.encode([7u8; 256]))),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let error = manager
        .login("zmail", PASSWORD)
        .await
        .expect_err("bad proof");
    assert!(matches!(error, Error::InvalidServerProof), "{error:?}");
}

/// Every password hash version go-srp answers (0 to 4) gets a proof, as
/// under Bridge; any other version gets none.
#[tokio::test]
async fn answers_the_srp_versions_bridge_answers() {
    for version in 0u8..=5 {
        let server = MockServer::start(move |request, _| match request.path.as_str() {
            "/auth/v4/info" => Reply::json(
                200,
                serde_json::json!({
                    "Version": version,
                    "Modulus": crate::srp_modulus::tests::TEST_MODULUS_CLEAR_SIGN,
                    "ServerEphemeral": BASE64.encode([1u8; 256]),
                    "Salt": BASE64.encode([2u8; 10]),
                    "SRPSession": "s",
                }),
            ),
            _ => api_error(422, 8002, "Incorrect login credentials"),
        })
        .await;
        let manager = Manager::new(test_config(&server.url, None)).expect("manager");
        let error = manager.login("zmail", PASSWORD).await.expect_err("login");
        let requests = server.requests();
        if version <= 4 {
            assert!(
                matches!(error, Error::PasswordWrong(_)),
                "{version}: {error:?}"
            );
            assert_eq!(requests.len(), 2, "{version}");
            assert_eq!(requests[1].path, "/auth/v4");
        } else {
            assert!(matches!(error, Error::Srp(_)), "{version}: {error:?}");
            // Only the challenge was fetched; no proof was sent.
            assert_eq!(requests.len(), 1);
        }
    }
}

#[tokio::test]
async fn refuses_an_unsigned_modulus() {
    let server = MockServer::start(|_, _| {
        Reply::json(
            200,
            serde_json::json!({
                "Version": 4,
                "Modulus": crate::srp_modulus::tests::TEST_MODULUS_CLEAR_SIGN.replacen("W2z5", "W2z6", 1),
                "ServerEphemeral": BASE64.encode([1u8; 256]),
                "Salt": BASE64.encode([2u8; 10]),
                "SRPSession": "s",
            }),
        )
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let error = manager.login("zmail", PASSWORD).await.expect_err("modulus");
    assert!(matches!(error, Error::Srp(_)), "{error:?}");
    // Only the challenge was fetched; no proof was sent.
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn human_verification_round_trip() {
    let srp = FakeSrpServer::new();
    let fake = srp.clone();
    let server = MockServer::start(move |request, _| match request.path.as_str() {
        "/auth/v4/info" => fake.info(),
        _ if request.header("x-pm-human-verification-token") == Some("hv-token") => {
            fake.auth(request, None)
        }
        _ => Reply::json(
            422,
            serde_json::json!({
                "Code": 9001,
                "Error": "Human verification required",
                "Details": {
                    "HumanVerificationMethods": ["captcha", "email"],
                    "HumanVerificationToken": "hv-token",
                },
            }),
        ),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");

    let error = manager.login("zmail", PASSWORD).await.expect_err("hv");
    let hv = error
        .human_verification()
        .expect("hv error")
        .expect("hv details");
    assert_eq!(
        hv.verify_url(),
        "https://verify.proton.me/?methods=captcha,email&token=hv-token"
    );

    let (_, auth) = manager
        .login_with_hv("zmail", PASSWORD, Some(&hv))
        .await
        .expect("login after hv");
    assert_eq!(auth.uid, "uid-1");
    let requests = server.requests();
    let retried = requests.last().expect("auth request");
    assert_eq!(
        retried.header("x-pm-human-verification-token"),
        Some("hv-token")
    );
    assert_eq!(
        retried.header("x-pm-human-verification-token-type"),
        Some("captcha,email")
    );
}

#[tokio::test]
async fn salts_the_key_password_and_lists_addresses() {
    use crate::keyring::tests::{ADDRESS_KEY, USER_KEY_ID, USER_KEY_SALT, USER_KEY_SALTED};

    let server = MockServer::start(|request, _| match request.path.as_str() {
        "/core/v4/users" => Reply::json(
            200,
            serde_json::json!({"Code": 1000, "User": {"ID": "user-1", "Name": "zmail", "Keys": [
                {"ID": USER_KEY_ID, "PrivateKey": USER_KEY_SALTED, "Primary": 1, "Active": 1, "Flags": 3},
            ]}}),
        ),
        "/core/v4/keys/salts" => Reply::json(
            200,
            serde_json::json!({"Code": 1000, "KeySalts": [
                {"ID": "other", "KeySalt": null},
                {"ID": USER_KEY_ID, "KeySalt": USER_KEY_SALT},
            ]}),
        ),
        _ => Reply::json(
            200,
            serde_json::json!({"Code": 1000, "Addresses": [
                {"ID": "b", "Email": "zmail.alias@proton.local", "Send": 1, "Receive": 1, "Status": 1, "Type": 2, "Order": 2, "Keys": []},
                {"ID": "a", "Email": "zmail@proton.local", "Send": 1, "Receive": 1, "Status": 1, "Type": 1, "Order": 1, "DisplayName": "Zmail", "Keys": [
                    {"ID": "addr-key", "PrivateKey": ADDRESS_KEY, "Token": "t", "Signature": "s", "Primary": 1, "Active": 1, "Flags": 3},
                ]},
            ]}),
        ),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");

    let user = client.get_user().await.expect("user");
    assert_eq!(
        user.keys.primary().map(|key| key.id.as_str()),
        Some(USER_KEY_ID)
    );
    let salted = client
        .salted_key_pass(&user, "password")
        .await
        .expect("salted key pass");
    assert_eq!(salted.as_bytes().len(), 31);
    let error = client
        .salted_key_pass(&user, "not the password")
        .await
        .expect_err("wrong key password");
    assert!(matches!(error, Error::KeyPasswordWrong), "{error:?}");

    let addresses = client.get_addresses().await.expect("addresses");
    let emails: Vec<&str> = addresses.iter().map(|a| a.email.as_str()).collect();
    assert_eq!(emails, ["zmail@proton.local", "zmail.alias@proton.local"]);
    assert!(addresses[0].send && addresses[0].receive);
    assert_eq!(addresses[0].kind, crate::AddressType::ORIGINAL);
    assert_eq!(addresses[0].status, crate::AddressStatus::ENABLED);
    assert_eq!(addresses[0].keys.0.len(), 1);
    assert!(!addresses[0].is_byoe());
}

#[tokio::test]
async fn label_actions_accept_an_empty_body() {
    // go-proton-api's fake server answers label actions with no body;
    // upstream (resty) reads that as an empty result.
    let server = MockServer::start(|_, _| Reply::bytes(200, "")).await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");

    let ids = ["m1".to_owned()];
    client.label_messages(&ids, "10").await.expect("label");
    client.unlabel_messages(&ids, "10").await.expect("unlabel");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/mail/v4/messages/label");
    assert_eq!(requests[1].json()["LabelID"], "10");
}

#[tokio::test]
async fn null_fields_decode_to_their_defaults_as_in_go() {
    // Proton sends `null` where go-proton-api's Go structs simply keep the
    // zero value; 0.16.2's first real sync failed on one ("invalid type:
    // null, expected a string").
    let server = MockServer::start(|_, _| {
        Reply::json(
            200,
            serde_json::json!({
                "Code": 1000,
                "Stale": null,
                "Messages": [{
                    "ID": "m1",
                    "AddressID": null,
                    "LabelIDs": ["0", "5"],
                    "ExternalID": null,
                    "ConversationID": "c1",
                    "Subject": null,
                    "Sender": {"Name": null, "Address": "a@proton.me"},
                    "ToList": [{"Name": null, "Address": "b@proton.me"}],
                    "CCList": null,
                    "BCCList": null,
                    "ReplyTos": null,
                    "Flags": null,
                    "Time": null,
                    "Size": null,
                    "Unread": null,
                    "IsReplied": null,
                    "NumAttachments": null
                }]
            }),
        )
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");

    let page = client
        .get_message_metadata_page(0, 150, &crate::MessageFilter::default())
        .await
        .expect("metadata with nulls");
    assert_eq!(page.len(), 1);
    let message = &page[0];
    assert_eq!(message.id, "m1");
    assert_eq!(message.external_id, "");
    assert_eq!(message.subject, "");
    assert_eq!(message.address_id, "");
    assert_eq!(message.label_ids, ["0", "5"]);
    assert_eq!(
        message.sender.as_ref().map(|s| s.address.as_str()),
        Some("a@proton.me")
    );
    assert_eq!(message.sender.as_ref().map(|s| s.name.as_str()), Some(""));
    assert_eq!(message.to_list.len(), 1);
    assert!(message.cc_list.is_empty());
    assert_eq!(message.time, 0);
}

#[tokio::test]
async fn retries_a_dropped_message_listing_but_not_a_dropped_post() {
    // Message listing is a POST carrying `X-HTTP-Method-Override: GET`;
    // 0.16.3's first sync failed when Proton dropped one mid-request.
    let server = MockServer::start(|request, index| match (request.path.as_str(), index) {
        ("/mail/v4/messages", 0) => Reply::drop_connection(),
        ("/mail/v4/messages", _) => {
            Reply::json(200, serde_json::json!({"Code": 1000, "Messages": []}))
        }
        _ => Reply::drop_connection(),
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");

    let page = client
        .get_message_metadata_page(0, 128, &crate::MessageFilter::default())
        .await
        .expect("listing after a dropped connection");
    assert!(page.is_empty());
    assert_eq!(server.requests().len(), 2);

    // A plain POST is not repeated: it could create or send something twice.
    let request = Request::post("/core/v4/something")
        .json(&serde_json::json!({}))
        .expect("body");
    let error = manager.send(&request, None).await.expect_err("dropped");
    assert!(error.is_network(), "{error:?}");
    assert_eq!(server.requests().len(), 3);
    // The message carries the cause, not only reqwest's summary.
    let message = error.to_string();
    assert!(
        message.starts_with("could not reach Proton: error sending request"),
        "{message}"
    );
    assert!(message.matches(": ").count() >= 2, "{message}");
}

#[tokio::test]
async fn tells_a_send_that_may_have_happened_from_one_refused() {
    // What `SendDraft` gets back, and whether Proton may have sent the
    // message all the same.
    let cases = [
        ("dropped after the request", Reply::drop_connection(), true),
        ("bad gateway", api_error(502, 0, "Bad gateway"), true),
        ("server error", api_error(500, 0, "Internal error"), true),
        (
            "still rate limited",
            api_error(429, 2028, "Too many requests").with_header("Retry-After", "0"),
            true,
        ),
        (
            "success it cannot read",
            Reply::json(200, serde_json::json!({"Code": 1000})),
            true,
        ),
        (
            "validation failed",
            api_error(422, 2001, "Invalid recipients"),
            false,
        ),
        ("bad request", api_error(400, 0, "Bad request"), false),
        ("forbidden", api_error(403, 0, "Forbidden"), false),
    ];
    for (case, reply, unconfirmed) in cases {
        let reply = Arc::new(Mutex::new(Some(reply)));
        let server = MockServer::start(move |_, _| {
            reply
                .lock()
                .expect("reply")
                .take()
                .unwrap_or_else(|| api_error(429, 2028, "Too many requests"))
        })
        .await;
        let manager = Manager::new(test_config(&server.url, None)).expect("manager");
        let error = manager
            .new_client("uid", "access", "refresh")
            .send_draft("draft", &crate::SendDraftReq::default())
            .await
            .expect_err(case);
        assert_eq!(
            error.may_have_been_carried_out(),
            unconfirmed,
            "{case}: {error:?}"
        );
        if case == "dropped after the request" {
            // The send is never repeated by the client itself.
            assert_eq!(server.requests().len(), 1, "{case}");
        }
    }

    // Nothing listening: the request was never sent.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let manager =
        Manager::new(test_config(&format!("https://127.0.0.1:{port}"), None)).expect("manager");
    let error = manager
        .new_client("uid", "access", "refresh")
        .send_draft("draft", &crate::SendDraftReq::default())
        .await
        .expect_err("refused");
    assert!(error.is_network(), "{error:?}");
    assert!(!error.may_have_been_carried_out(), "{error:?}");

    // A TLS failure stops at the handshake.
    let server = MockServer::start(|_, _| user_reply()).await;
    let manager =
        Manager::new(test_config(&server.url, Some(PinSet::trusted_api_pins()))).expect("manager");
    let error = manager
        .new_client("uid", "access", "refresh")
        .send_draft("draft", &crate::SendDraftReq::default())
        .await
        .expect_err("pin mismatch");
    assert!(!error.may_have_been_carried_out(), "{error:?}");

    // No answer in time once the request was written. The client's own
    // timeouts are Bridge's 30 s, so a client with a short one stands in.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!(
        "http://{}/mail/v4/messages/draft",
        listener.local_addr().expect("addr")
    );
    let silent = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = [0; 1024];
        let _ = stream.read(&mut request).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let timeout = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .expect("client")
        .post(&url)
        .body("{}")
        .send()
        .await
        .expect_err("timed out");
    assert!(timeout.is_timeout() && !timeout.is_connect(), "{timeout:?}");
    assert!(Error::from_transport(timeout).may_have_been_carried_out());
    silent.abort();

    // Errors raised before any request say nothing was sent.
    assert!(!Error::Package("no key".to_owned()).may_have_been_carried_out());
    assert!(
        !Error::Deauthenticated(Box::new(Error::from_api(crate::ApiError {
            status: 422,
            code: Code(10013),
            message: "Invalid refresh token".to_owned(),
            details: None,
        })))
        .may_have_been_carried_out()
    );
}

#[test]
fn idempotent_requests_are_the_ones_safe_to_repeat() {
    assert!(Request::get("/x").is_idempotent());
    assert!(Request::put("/x").is_idempotent());
    assert!(Request::delete("/x").is_idempotent());
    assert!(!Request::post("/x").is_idempotent());
    assert!(
        Request::post("/x")
            .header(crate::manager::METHOD_OVERRIDE_HEADER, "GET")
            .is_idempotent()
    );
    assert!(
        !Request::post("/x")
            .header(crate::manager::METHOD_OVERRIDE_HEADER, "DELETE")
            .is_idempotent()
    );
}

// Issue #462: what the server says never keeps a request going, or fills
// memory, without bound. Each test fails rather than hangs on a regression.

/// Runs `future`, failing the test if it takes longer than `seconds`.
async fn within<T>(seconds: u64, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(seconds), future)
        .await
        .expect("bounded: finished in time")
}

const GIB: u64 = 1 << 30;
const MIB: usize = 1 << 20;

#[tokio::test]
async fn refuses_a_json_body_declared_over_the_limit() {
    // A gigabyte declared, nothing sent: the client must not wait for it.
    let server = MockServer::start(|_, _| Reply::streamed(200, Some(GIB), 0)).await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");
    let error = within(10, client.get_user()).await.expect_err("too large");
    assert!(error.to_string().contains("too large"), "{error:?}");
    assert!(
        matches!(error, Error::ResponseTooLarge { limit } if limit == crate::MAX_JSON_RESPONSE_BYTES),
        "{error:?}"
    );
    assert_eq!(server.requests().len(), 1, "not retried");
}

#[tokio::test]
async fn cuts_off_a_chunked_json_body_past_the_limit() {
    // No length given: the client counts as it reads.
    let server = MockServer::start(|_, _| Reply::streamed(200, None, 80 * MIB)).await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");
    let error = within(30, client.get_user()).await.expect_err("too large");
    assert!(error.to_string().contains("too large"), "{error:?}");
}

#[tokio::test]
async fn refuses_an_attachment_declared_over_the_limit() {
    let server = MockServer::start(|_, _| Reply::streamed(200, Some(4 * GIB), 0)).await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");
    let error = within(10, client.get_attachment("att-1"))
        .await
        .expect_err("too large");
    assert!(error.to_string().contains("too large"), "{error:?}");
    assert!(
        matches!(error, Error::ResponseTooLarge { limit } if limit == crate::MAX_DATA_RESPONSE_BYTES),
        "{error:?}"
    );
}

#[tokio::test]
async fn gives_up_on_a_listing_that_stays_stale() {
    let server = MockServer::start(|_, _| {
        Reply::json(
            200,
            serde_json::json!({"Code": 1000, "Messages": [], "Stale": 1}),
        )
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");
    let error = within(
        20,
        client.get_message_metadata_page(0, 128, &crate::MessageFilter::default()),
    )
    .await
    .expect_err("still stale");
    assert!(error.to_string().contains("stale"), "{error:?}");
    let asked = server.requests().len();
    assert!((2..=10).contains(&asked), "asked {asked} times");
    assert_eq!(asked, crate::MAX_STALE_ATTEMPTS as usize);
}

#[tokio::test]
async fn refuses_an_inflated_contact_total() {
    let server = MockServer::start(|_, _| {
        Reply::json(
            200,
            serde_json::json!({"Code": 1000, "ContactEmails": [], "Total": 1_000_000_000_000u64}),
        )
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");
    let error = within(10, client.get_all_contact_emails("bob@example.com"))
        .await
        .expect_err("inflated total");
    assert!(error.to_string().contains("more than"), "{error:?}");
    assert_eq!(server.requests().len(), 1, "no page past the first");
}

#[tokio::test]
async fn refuses_an_inflated_message_count() {
    let server = MockServer::start(|_, _| {
        Reply::json(
            200,
            serde_json::json!({"Code": 1000, "Messages": [], "Total": 1_000_000_000_000u64}),
        )
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");
    let error = within(
        10,
        client.get_message_metadata(&crate::MessageFilter::default()),
    )
    .await
    .expect_err("inflated total");
    assert!(error.to_string().contains("more than"), "{error:?}");
    assert_eq!(server.requests().len(), 1, "only the count");
}

#[tokio::test]
async fn reads_each_body_up_to_its_routes_limit() {
    let server = MockServer::start(|request, _| {
        let (size, chunked) = match request.path.as_str() {
            "/json-at-limit" | "/data-over-json-limit" => (1024, false),
            "/json-over-limit" => (1025, false),
            "/json-chunked-over-limit" => (1025, true),
            "/data-at-limit" => (4096, true),
            _ => (4097, false),
        };
        Reply::streamed(200, (!chunked).then_some(size as u64), size)
    })
    .await;
    let mut config = test_config(&server.url, None);
    config.limits = crate::config::ResponseLimits {
        json: 1024,
        data: 4096,
    };
    let manager = Manager::new(config).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");
    let send = |request: Request| {
        let client = &client;
        async move { within(10, client.send(&request)).await }
    };

    let body = send(Request::get("/json-at-limit"))
        .await
        .expect("at the limit");
    assert_eq!(body.body.len(), 1024);
    for path in ["/json-over-limit", "/json-chunked-over-limit"] {
        let error = send(Request::get(path)).await.expect_err(path);
        assert!(
            matches!(error, Error::ResponseTooLarge { limit: 1024 }),
            "{path}: {error:?}"
        );
        assert!(error.may_have_been_carried_out(), "an answer was not read");
    }
    assert_eq!(manager.status(), Status::Up, "the server did answer");

    let data = send(Request::get("/data-at-limit").data())
        .await
        .expect("data limit");
    assert_eq!(data.body.len(), 4096);
    send(Request::get("/data-over-json-limit").data())
        .await
        .expect("data routes take the data limit");
    let error = send(Request::get("/data-over-limit").data())
        .await
        .expect_err("over the data limit");
    assert!(
        matches!(error, Error::ResponseTooLarge { limit: 4096 }),
        "{error:?}"
    );
}

#[tokio::test]
async fn asks_again_while_a_listing_is_stale() {
    let server = MockServer::start(|_, index| {
        Reply::json(
            200,
            serde_json::json!({"Code": 1000, "Messages": [{"ID": "m1"}], "Stale": i64::from(index < 2)}),
        )
    })
    .await;
    let manager = Manager::new(test_config(&server.url, None)).expect("manager");
    let client = manager.new_client("uid", "access", "refresh");
    let page = within(
        10,
        client.get_message_metadata_page(0, 128, &crate::MessageFilter::default()),
    )
    .await
    .expect("fresh on the third answer");
    assert_eq!(page.len(), 1);
    assert_eq!(server.requests().len(), 3);
}
