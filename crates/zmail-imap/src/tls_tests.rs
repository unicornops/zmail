//! IMAP over TLS trusts what the shared TLS configuration trusts
//! (`zmail_security::tls`, ADR 0009), and a root in the system's store
//! that does not parse does not fail the connection.

use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;
use zmail_core::AuthKind;
use zmail_security::tls::test_ca;

use crate::{ImapEndpoint, ImapSecurity, connect_session};

/// An IMAP server over implicit TLS on a free loopback port, with the
/// test CA's certificate, that greets and accepts one LOGIN. The task
/// ends with an error if the client refuses the certificate.
async fn serve_tls() -> (ImapEndpoint, JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    let acceptor = TlsAcceptor::from(test_ca::server_config());
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await?;
        let tls = acceptor.accept(tcp).await?;
        greet_and_login(BufReader::new(tls)).await
    });
    let endpoint = ImapEndpoint {
        host: "localhost".to_owned(),
        port,
        security: ImapSecurity::ImplicitTls,
    };
    (endpoint, task)
}

async fn greet_and_login<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: BufReader<S>,
) -> std::io::Result<()> {
    socket.get_mut().write_all(b"* OK ready\r\n").await?;
    let mut line = String::new();
    socket.read_line(&mut line).await?;
    let tag = line.split_whitespace().next().unwrap_or("a").to_owned();
    socket
        .get_mut()
        .write_all(format!("{tag} OK signed in\r\n").as_bytes())
        .await?;
    socket.get_mut().flush().await
}

async fn sign_in_over_tls() {
    let (endpoint, server) = serve_tls().await;
    let signed_in = connect_session(&endpoint, "user", "secret", AuthKind::Password).await;
    let served = server.await.expect("the server task");
    signed_in.expect("IMAP signs in over TLS with a certificate the configuration trusts");
    served.expect("the server's side of the handshake and LOGIN");
}

#[tokio::test]
async fn shared_tls_imap_trusts_what_the_shared_configuration_trusts() {
    test_ca::trust();
    sign_in_over_tls().await;
}

/// Run by [`shared_tls_an_unparsable_system_root_does_not_fail_imap`] in
/// a child process whose system store is a file of its own.
const CHILD: &str = "ZMAIL_IMAP_SYSTEM_ROOTS_CHILD";

/// On Linux and the BSDs the system's roots are read by
/// rustls-native-certs, which reads `SSL_CERT_FILE` instead of the
/// distribution's bundle when it is set. A child process gets one with a
/// certificate that does not parse, then the test CA: the connection
/// must work with the CA as a system root. (macOS and Windows parse
/// their own stores.)
#[cfg(all(unix, not(target_vendor = "apple")))]
#[test]
fn shared_tls_an_unparsable_system_root_does_not_fail_imap() {
    // A certificate in PEM whose DER is no certificate at all.
    const GARBAGE: &str = "-----BEGIN CERTIFICATE-----\nMAMCAQA=\n-----END CERTIFICATE-----\n";
    let directory = std::env::temp_dir().join(format!("zmail-imap-roots-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a folder for the roots");
    let roots = directory.join("roots.pem");
    let mut contents = GARBAGE.as_bytes().to_vec();
    contents.extend_from_slice(test_ca::CA_PEM);
    std::fs::write(&roots, contents).expect("the roots file");

    let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            "tls_tests::system_roots_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("SSL_CERT_FILE", &roots)
        .env_remove("SSL_CERT_DIR")
        .output()
        .expect("the child runs");
    let _ = std::fs::remove_dir_all(&directory);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "the child failed:\n{stdout}\n{stderr}"
    );
}

/// The child's half: the test CA is trusted only as a system root here,
/// never through `test_ca::trust`.
#[tokio::test]
#[ignore = "run in a child process by shared_tls_an_unparsable_system_root_does_not_fail_imap"]
async fn system_roots_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    sign_in_over_tls().await;
}
