//! A local mock HTTPS server for tests of this crate and of `zmail-proton`
//! (built for those only: `cfg(test)` or the `test-harness` feature).
//!
//! The server speaks just enough HTTP/1.1 over rustls for the tests: one
//! request per connection, `Content-Length` bodies, `Connection: close`
//! replies. Its certificate comes from `testdata/` (a throwaway CA and a
//! leaf for `127.0.0.1`), so the client runs its real TLS path.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

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

use crate::{AppVersion, ClientConfig, PinSet, response::RetryPolicy};

/// The mock server's CA.
pub const CA_PEM: &[u8] = include_bytes!("testdata/ca.pem");
const LEAF_PEM: &[u8] = include_bytes!("testdata/leaf.pem");
const LEAF_KEY_PEM: &[u8] = include_bytes!("testdata/leaf-key.pem");
/// SPKI pin of the mock server's leaf certificate.
pub const LEAF_PIN: &str = "w8Iej+c8CZbEzpfZigKNhMQF1/ZM9EOLj7sR10YYCMM=";

/// A request as the mock server saw it.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The method.
    pub method: String,
    /// The path with its query.
    pub path: String,
    /// The headers, in order.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Recorded {
    /// A header's value.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The JSON body.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("JSON request body")
    }
}

/// What a reply's body is.
#[derive(Debug, Clone)]
pub enum Body {
    /// These bytes, with their `Content-Length`.
    Bytes(Vec<u8>),
    /// `length` bytes of spaces (JSON whitespace), written in chunks. With
    /// `declared`, that is the `Content-Length` sent, and when it is more
    /// than `length` the connection is held open after the bytes, as by a
    /// server that never finishes; without it, the body is chunked.
    Streamed {
        /// The `Content-Length` header, if any.
        declared: Option<u64>,
        /// The bytes actually written.
        length: usize,
    },
}

/// A reply from the mock server.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The status code; 0 drops the connection without answering.
    pub status: u16,
    /// Extra headers.
    pub headers: Vec<(&'static str, String)>,
    /// The body.
    pub body: Body,
}

impl Reply {
    /// A JSON reply.
    pub fn json(status: u16, body: serde_json::Value) -> Self {
        Self::bytes(status, body.to_string().into_bytes())
    }

    /// A reply with this body as it is.
    pub fn bytes(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Body::Bytes(body.into()),
        }
    }

    /// A streamed reply (see [`Body::Streamed`]).
    pub fn streamed(status: u16, declared: Option<u64>, length: usize) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Body::Streamed { declared, length },
        }
    }

    /// Closes the connection after reading the request, without
    /// answering (a connection dropped mid-request).
    pub fn drop_connection() -> Self {
        Self::bytes(0, Vec::new())
    }

    /// Adds a header.
    pub fn with_header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }
}

type Handler = dyn Fn(&Recorded, usize) -> Reply + Send + Sync;

/// The mock server; it stops accepting connections when dropped.
pub struct MockServer {
    /// Its base URL, `https://127.0.0.1:<port>`.
    pub url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task: JoinHandle<()>,
}

impl MockServer {
    /// Starts a server on a free port that answers each request with
    /// `handler(request, index of the request)`.
    pub async fn start(
        handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        Self::start_on(listener, Arc::new(handler))
    }

    /// Starts a server on `listener`.
    pub fn start_on(listener: TcpListener, handler: Arc<Handler>) -> Self {
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

    /// The requests received so far.
    pub fn requests(&self) -> Vec<Recorded> {
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
    if reply.status == 0 {
        return;
    }
    let mut head = format!(
        "HTTP/1.1 {} Mock\r\nContent-Type: application/json\r\nConnection: close\r\nDate: Tue, 20 Oct 2026 10:00:00 GMT\r\n",
        reply.status,
    );
    match &reply.body {
        Body::Bytes(bytes) => head.push_str(&format!("Content-Length: {}\r\n", bytes.len())),
        Body::Streamed {
            declared: Some(declared),
            ..
        } => head.push_str(&format!("Content-Length: {declared}\r\n")),
        Body::Streamed { declared: None, .. } => head.push_str("Transfer-Encoding: chunked\r\n"),
    }
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    if reader.get_mut().write_all(head.as_bytes()).await.is_err() {
        return;
    }
    match reply.body {
        Body::Bytes(bytes) => {
            let _ = reader.get_mut().write_all(&bytes).await;
        }
        Body::Streamed { declared, length } => {
            if stream_spaces(reader.get_mut(), length, declared.is_none())
                .await
                .is_err()
            {
                return;
            }
            if declared.is_some_and(|declared| declared > length as u64) {
                // Never finish: wait until the client gives up.
                let mut buffer = [0u8; 64];
                let _ = tokio::time::timeout(
                    Duration::from_secs(300),
                    reader.get_mut().read(&mut buffer),
                )
                .await;
                return;
            }
        }
    }
    let _ = reader.get_mut().shutdown().await;
}

/// Writes `length` spaces, as chunks of a chunked body when `chunked`.
async fn stream_spaces<S>(stream: &mut S, length: usize, chunked: bool) -> std::io::Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    const CHUNK: usize = 256 * 1024;
    let spaces = vec![b' '; CHUNK];
    let mut left = length;
    while left > 0 {
        let size = left.min(CHUNK);
        if chunked {
            stream.write_all(format!("{size:x}\r\n").as_bytes()).await?;
        }
        stream.write_all(&spaces[..size]).await?;
        if chunked {
            stream.write_all(b"\r\n").await?;
        }
        left -= size;
    }
    if chunked {
        stream.write_all(b"0\r\n\r\n").await?;
    }
    stream.flush().await
}

/// A config for the mock server with fast, jitter-free retries.
pub fn config(url: &str, pins: Option<PinSet>) -> ClientConfig {
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
