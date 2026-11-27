//! JMAP over HTTPS (the session, the API and the event source share one
//! client) and JMAP push over WebSocket trust what the shared TLS
//! configuration trusts (`zmail_security::tls`, ADR 0009).

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::{self, handshake::server};
use zmail_core::{AuthKind, BlobStore, BlobWriter, CachePolicySummary, ProtocolKind, SyncTarget};
use zmail_security::tls::test_ca;

use crate::{
    PushPreference, check_sign_in,
    push::{PushTimings, watch_with},
};

/// A session whose API and event source are on `http_base` and whose
/// WebSocket is `websocket_url`.
fn session(http_base: &str, websocket_url: &str) -> String {
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {
                "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                "maxCallsInRequest": 16, "maxObjectsInGet": 250,
                "maxObjectsInSet": 500, "collationAlgorithms": []
            },
            "urn:ietf:params:jmap:mail": {},
            "urn:ietf:params:jmap:websocket": {"url": websocket_url, "supportsPush": true}
        },
        "accounts": {
            "a1": {"name": "rob@example.com", "isPersonal": true, "isReadOnly": false,
                   "accountCapabilities": {"urn:ietf:params:jmap:mail": {}}}
        },
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a1"},
        "username": "rob@example.com",
        "apiUrl": format!("{http_base}/api"),
        "downloadUrl": format!("{http_base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{http_base}/upload/{{accountId}}/"),
        "eventSourceUrl": format!("{http_base}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": "s1"
    })
    .to_string()
}

/// Reads a request head and answers every request with the session.
async fn answer_with_session<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    session: &str,
) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&buffer[..read]);
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{session}",
        session.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// A free loopback port's listener.
async fn listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    (listener, port)
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

fn target(endpoint: String) -> SyncTarget {
    SyncTarget {
        id: 1,
        display_name: "Pushed".to_owned(),
        protocol: ProtocolKind::Jmap,
        auth: AuthKind::Password,
        endpoint,
        username: "rob@example.com".to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        identities: Vec::new(),
        blobs: BlobStore::new(Arc::new(NoBlobs)),
        checkpoint: None,
        oauth2: None,
    }
}

#[tokio::test]
async fn shared_tls_jmap_over_https_trusts_what_the_shared_configuration_trusts() {
    test_ca::trust();
    let (listener, port) = listener().await;
    let base = format!("https://localhost:{port}");
    let session = session(&base, &format!("wss://localhost:{port}/ws"));
    let acceptor = TlsAcceptor::from(test_ca::server_config());
    let server = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            if let Ok(tls) = acceptor.accept(tcp).await {
                let _ = answer_with_session(tls, &session).await;
            }
        }
    });
    let signed_in = check_sign_in(&base, "rob@example.com", "pw", AuthKind::Password).await;
    server.abort();
    signed_in.expect("JMAP signs in over HTTPS with a certificate the configuration trusts");
}

/// The session comes over plain HTTP here, so that only the WebSocket
/// handshake is over TLS.
#[tokio::test]
async fn shared_tls_jmap_websocket_push_trusts_what_the_shared_configuration_trusts() {
    test_ca::trust();
    let (http, http_port) = listener().await;
    let (websocket, websocket_port) = listener().await;
    let base = format!("http://127.0.0.1:{http_port}");
    let session = session(&base, &format!("wss://localhost:{websocket_port}/ws"));
    let http_server = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = http.accept().await else {
                break;
            };
            let _ = answer_with_session(tcp, &session).await;
        }
    });
    let upgraded = Arc::new(AtomicBool::new(false));
    let seen = upgraded.clone();
    let acceptor = TlsAcceptor::from(test_ca::server_config());
    let websocket_server = tokio::spawn(async move {
        let (tcp, _) = websocket.accept().await?;
        let tls = acceptor.accept(tcp).await?;
        // The callback's error type is the library's.
        #[allow(clippy::result_large_err)]
        let socket = tokio_tungstenite::accept_hdr_async(
            tls,
            |_: &server::Request, mut response: server::Response| {
                response.headers_mut().insert(
                    "sec-websocket-protocol",
                    tungstenite::http::HeaderValue::from_static("jmap"),
                );
                Ok(response)
            },
        )
        .await
        .map_err(std::io::Error::other)?;
        seen.store(true, Ordering::SeqCst);
        drop(socket);
        Ok::<(), std::io::Error>(())
    });

    let timings = PushTimings {
        ping_interval: Duration::from_secs(1),
        stall_grace: Duration::from_millis(500),
        ws_ready_attempts: 1,
        ws_ready_wait: Duration::from_millis(500),
        connect_timeout: Duration::from_secs(10),
    };
    let watched = tokio::time::timeout(
        Duration::from_secs(30),
        watch_with(
            &target(base),
            "pw",
            PushPreference::WebSocket,
            timings,
            |_| {},
        ),
    )
    .await;
    http_server.abort();
    // A client that never connects leaves the server waiting.
    let served = tokio::time::timeout(Duration::from_secs(5), websocket_server).await;
    assert!(
        upgraded.load(Ordering::SeqCst),
        "the WebSocket handshake over TLS did not complete: {served:?}; the watch: {watched:?}"
    );
}
