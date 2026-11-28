//! The TLS client configuration of every connection to a mail server
//! (ADR 0009): IMAP, SMTP, JMAP over HTTPS and WebSocket, OAuth token
//! requests and account discovery.
//!
//! Certificates are checked by the operating system's verifier, through
//! `rustls-platform-verifier`: the Security framework on macOS, CryptoAPI
//! on Windows, and on Linux and the BSDs WebPKI against the system's root
//! store (read by `rustls-native-certs`, which honours `SSL_CERT_FILE` and
//! `SSL_CERT_DIR`). A company or private CA the user has added to the
//! system is therefore trusted by every protocol alike, and one root in
//! the system's store that rustls cannot parse is skipped, not fatal.
//!
//! The configuration is built once and shared ([`client_config`]). On
//! Linux and the BSDs that is also when the system's roots are read, so a
//! root added later is used after a restart. A failure to build it (on
//! Linux, a system store with no usable root at all) is not kept: the next
//! connection tries again.
//!
//! Proton's API client does not use it: it pins Proton's keys over the
//! system's roots (ADR 0004).

use std::sync::{Arc, Once, OnceLock};

use rustls_platform_verifier::Verifier;
use tokio_rustls::rustls::{self, ClientConfig, crypto::aws_lc_rs, pki_types::CertificateDer};

#[cfg(any(test, feature = "test-ca"))]
pub mod test_ca;

/// The shared configuration could not be built.
#[derive(Debug, thiserror::Error)]
#[error("the system's certificate verifier could not be set up: {0}")]
pub struct TlsSetupError(#[from] rustls::Error);

/// The TLS client configuration every connection to a mail server uses:
/// aws-lc-rs, TLS 1.2 and 1.3, no client certificate, no ALPN, and the
/// operating system's certificate verifier.
///
/// Every caller gets the same configuration. For a `reqwest` client, hand
/// a copy to `use_preconfigured_tls` (`ClientConfig::clone` keeps the
/// same verifier and session cache); a `reqwest` built without one would
/// use its own roots.
pub fn client_config() -> Result<Arc<ClientConfig>, TlsSetupError> {
    install_default_provider();
    #[cfg(any(test, feature = "test-ca"))]
    if test_ca::trusted() {
        static WITH_TEST_CA: OnceLock<Arc<ClientConfig>> = OnceLock::new();
        return cached(&WITH_TEST_CA, || build(vec![test_ca::ca()]));
    }
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    cached(&CONFIG, || build(Vec::new()))
}

fn cached(
    cell: &OnceLock<Arc<ClientConfig>>,
    build: impl FnOnce() -> Result<ClientConfig, rustls::Error>,
) -> Result<Arc<ClientConfig>, TlsSetupError> {
    if let Some(config) = cell.get() {
        return Ok(config.clone());
    }
    let config = Arc::new(build()?);
    Ok(cell.get_or_init(|| config).clone())
}

/// The configuration with the platform's verifier, trusting
/// `extra_roots` too (only ever the test CA).
fn build(extra_roots: Vec<CertificateDer<'static>>) -> Result<ClientConfig, rustls::Error> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let verifier = if extra_roots.is_empty() {
        Verifier::new(provider.clone())?
    } else {
        Verifier::new_with_extra_roots(extra_roots, provider.clone())?
    };
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        // rustls calls every verifier but its own WebPKI one "dangerous".
        // This is the platform's (what `rustls_platform_verifier`'s
        // `with_platform_verifier` sets up): it checks the chain to a
        // trusted root, the name, the validity dates and the key usage,
        // and revocation where the platform does.
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

/// Makes aws-lc-rs rustls's process-wide default, as the IMAP, SMTP and
/// JMAP push clients each did before they shared this configuration, for
/// any library that builds a rustls configuration of its own.
fn install_default_provider() {
    static PROVIDER: Once = Once::new();
    PROVIDER.call_once(|| {
        // Another provider already installed is left in place.
        let _ = aws_lc_rs::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::{TcpListener, TcpStream},
    };
    use tokio_rustls::{
        TlsAcceptor, TlsConnector,
        rustls::{ClientConfig, pki_types::ServerName},
    };

    use super::{build, client_config, test_ca};

    /// Connects to a loopback server with the test CA's certificate,
    /// checks it with `config`, and exchanges a byte each way.
    async fn handshake(config: Arc<ClientConfig>) -> std::io::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let acceptor = TlsAcceptor::from(test_ca::server_config());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await?;
            let mut tls = acceptor.accept(tcp).await?;
            tls.write_all(b"s").await?;
            let mut byte = [0_u8];
            tls.read_exact(&mut byte).await?;
            Ok::<_, std::io::Error>(())
        });
        let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
        let name = ServerName::try_from("localhost").expect("a name");
        let client = TlsConnector::from(config).connect(name, tcp).await;
        let client = match client {
            Ok(mut tls) => {
                let mut byte = [0_u8];
                tls.read_exact(&mut byte).await?;
                tls.write_all(b"c").await?;
                tls.flush().await
            }
            Err(error) => Err(error),
        };
        let served = server.await.expect("the server task");
        client.and(served)
    }

    #[test]
    fn the_configuration_is_built_once() {
        // As the other tests here do, so that none of them can switch
        // the configuration between the two calls.
        test_ca::trust();
        let first = client_config().expect("the platform verifier");
        let second = client_config().expect("the platform verifier");
        assert!(Arc::ptr_eq(&first, &second));
    }

    /// The test CA is trusted only once a test asks for it, and the
    /// system's verifier really checks: without the CA, the test
    /// server's certificate is refused.
    #[tokio::test]
    async fn the_platform_verifier_refuses_a_root_the_system_does_not_trust() {
        let system_only = Arc::new(build(Vec::new()).expect("the platform verifier"));
        let error = handshake(system_only)
            .await
            .expect_err("the test CA is no system root");
        let message = error.to_string();
        assert!(
            message.contains("certificate") || message.contains("Certificate"),
            "{message}"
        );
    }

    /// On every platform's verifier (CI runs this on macOS and Windows
    /// too), an extra root is trusted on top of the system's: what the
    /// other crates' TLS tests rely on.
    #[tokio::test]
    async fn the_shared_configuration_trusts_the_test_ca_once_asked() {
        test_ca::trust();
        let config = client_config().expect("the platform verifier");
        handshake(config)
            .await
            .expect("the shared configuration trusts the test CA");
    }
}
