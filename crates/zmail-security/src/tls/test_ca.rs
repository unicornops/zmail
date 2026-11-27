//! A throwaway certificate authority for tests of the shared TLS
//! configuration, behind the `test-ca` feature (dev-dependencies only).
//!
//! A test starts a loopback server with [`server_config`] (or [`accept`],
//! or [`serve_https_once`]), whose certificate names `localhost`,
//! `127.0.0.1` and `::1` and is signed by this CA, and calls [`trust`]
//! before it connects. From then on the shared configuration
//! ([`client_config`](super::client_config)) trusts this
//! CA on top of the system's, in that process. Nothing else trusts it: a
//! client with TLS settings of its own (bundled roots, or its own load of
//! the system's) fails the handshake, which is what the tests look for.
//!
//! The certificates in `testdata/` were made with `openssl ca` (P-256,
//! SHA-256). The CA's private key was deleted once it had signed the one
//! server certificate, so the CA vouches for loopback names and nothing
//! else, whoever reads the server key checked in beside it. Both
//! certificates start in 2019 and run to 2125: Apple's verifier refuses a
//! server certificate issued after 1 July 2019 that is valid for more
//! than 825 days, and one that expires would break the tests on a date
//! nobody chose.

use std::{
    io::{self, Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use tokio_rustls::rustls::{
    ServerConfig, ServerConnection, StreamOwned,
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _},
};

/// The CA certificate, PEM.
pub const CA_PEM: &[u8] = include_bytes!("testdata/ca.pem");
/// The server certificate for `localhost`, `127.0.0.1` and `::1`, PEM.
const SERVER_PEM: &[u8] = include_bytes!("testdata/leaf.pem");
/// Its private key, PKCS #8 PEM.
const SERVER_KEY_PEM: &[u8] = include_bytes!("testdata/leaf-key.pem");

/// How long the test servers wait for a client that says nothing.
const IDLE: Duration = Duration::from_secs(10);

static TRUSTED: AtomicBool = AtomicBool::new(false);

/// Makes the shared TLS configuration trust this CA, in this process,
/// from the next time a client asks for it.
pub fn trust() {
    TRUSTED.store(true, Ordering::SeqCst);
}

/// Whether a test has called [`trust`] in this process.
pub(super) fn trusted() -> bool {
    TRUSTED.load(Ordering::SeqCst)
}

/// The CA certificate, DER.
pub(super) fn ca() -> CertificateDer<'static> {
    CertificateDer::from_pem_slice(CA_PEM).expect("the test CA parses")
}

/// The TLS settings of a test server: the certificate for the loopback
/// names, signed by this CA.
pub fn server_config() -> Arc<ServerConfig> {
    static CONFIG: OnceLock<Arc<ServerConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let chain = CertificateDer::pem_slice_iter(SERVER_PEM)
                .collect::<Result<Vec<_>, _>>()
                .expect("the test server certificate parses");
            let key =
                PrivateKeyDer::from_pem_slice(SERVER_KEY_PEM).expect("the test server key parses");
            let config =
                ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                    .with_safe_default_protocol_versions()
                    .expect("TLS versions")
                    .with_no_client_auth()
                    .with_single_cert(chain, key)
                    .expect("the test server certificate and key match");
            Arc::new(config)
        })
        .clone()
}

/// A blocking TLS connection a test server accepted.
pub type TlsStream = StreamOwned<ServerConnection, TcpStream>;

/// Runs the server side of a TLS handshake on `tcp` (fresh, or after a
/// plain-text `STARTTLS`) to its end. A client that does not trust this
/// CA makes it an error.
pub fn handshake(tcp: TcpStream) -> io::Result<TlsStream> {
    tcp.set_read_timeout(Some(IDLE))?;
    let connection = ServerConnection::new(server_config()).map_err(io::Error::other)?;
    let mut stream = StreamOwned::new(connection, tcp);
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    Ok(stream)
}

/// Accepts one connection on `listener` and runs [`handshake`] on it.
pub fn accept(listener: &TcpListener) -> io::Result<TlsStream> {
    let (tcp, _) = listener.accept()?;
    handshake(tcp)
}

/// An HTTPS server on a free loopback port that answers one request with
/// `response` (status line, headers and body as they go on the wire) and
/// then closes. The thread returns the request it read, head and body,
/// or the error that stopped it (the client refusing the certificate,
/// say).
pub fn serve_https_once(
    response: impl Into<Vec<u8>>,
) -> (u16, thread::JoinHandle<io::Result<String>>) {
    let response = response.into();
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("an address").port();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener)?;
        let request = read_request(&mut stream)?;
        stream.write_all(&response)?;
        stream.conn.send_close_notify();
        stream.flush()?;
        Ok(request)
    });
    (port, server)
}

/// Reads one HTTP/1.1 request: the head, then as many bytes of body as
/// its `Content-Length` says.
fn read_request(stream: &mut TlsStream) -> io::Result<String> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let body_start = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&request[..body_start]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while request.len() < body_start + length {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        request.extend_from_slice(&buffer[..read]);
    }
    Ok(String::from_utf8_lossy(&request).into_owned())
}
