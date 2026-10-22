// Partly ported from go-proton-api `manager_builder.go` and `option.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api), and from proton-bridge
// `internal/constants/host_default.go` and `internal/dialer/dialer_basic.go`
// at v3.27.1 (43b9237786204a0215abfd4dcc51ee7bb92f52d4).
// go-proton-api: Copyright (c) 2020 James Houlahan, Copyright (c) 2022
// Proton AG, MIT License (crates/zmail-proton-api/LICENSE-MIT).
// proton-bridge: Copyright (c) 2026 Proton AG, GPL-3.0-or-later, used under
// GPL-3.0-only.
// Modified by UnicornOps on 2026-10-20: the functional options became a
// config struct. The host and the pins are constants with no production
// override. The app version is Bridge's own (`<os>-bridge@<pinned Bridge
// version>`, Bridge's `getAPIOS()` and format), because Proton's sign-in
// refuses every other client (ADR 0004); the User-Agent names Zmail.
// Bridge's transport timeouts and HTTP/1.1-only TLS dialer are rebuilt on
// reqwest and rustls.

//! Client configuration: the API host, the app version header and TLS trust.

use std::{sync::Arc, time::Duration};

use tokio_rustls::rustls::{
    self, ClientConfig as TlsConfig, RootCertStore, client::WebPkiServerVerifier,
};
use url::Url;

use crate::{
    pinning::{PinSet, PinningVerifier},
    response::{Error, RetryPolicy},
};

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!(
    "the `test-harness` feature points the Proton client at a local fake server and is for debug test builds only"
);

/// Proton's API host (Bridge's `constants.APIHost`).
pub const API_HOST_URL: &str = "https://mail-api.proton.me";

/// Timeout for TCP connect plus the TLS handshake (Bridge's dialer and
/// `TLSHandshakeTimeout`, both 30 s).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout between reads while waiting for a response (Bridge's
/// `ResponseHeaderTimeout`, 30 s).
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle pooled connections are closed after this (Bridge: 5 minutes).
pub(crate) const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The Proton Mail Bridge release Zmail ports, and whose app version it
/// sends: the `v3.27.1` pin in `docs/upstream/proton.md`.
pub const BRIDGE_VERSION: &str = "3.27.1";

/// Zmail's identity: the `x-pm-appversion` and `User-Agent` values.
///
/// Proton's sign-in only accepts Proton's own clients: `zmail` is not a
/// product it knows (code 2064) and its generic `other` product is refused
/// at sign-in (code 5002). Zmail ports Bridge's API behaviour, so it sends
/// Bridge's app version, `<os>-bridge@<BRIDGE_VERSION>` with the platform
/// as Bridge's `getAPIOS()` names it. Rob decided this on 2026-10-22
/// while Proton is asked for a registered identity (ADR 0004). The
/// User-Agent is `zmail/<version> (<os>)`, as Bridge itself sends the name
/// of the mail client connected to it, so the requests still say they come
/// from Zmail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppVersion {
    header: String,
    user_agent: String,
}

impl AppVersion {
    /// Builds Zmail's identity for the platform it runs on from Zmail's
    /// release version, such as `env!("CARGO_PKG_VERSION")` in `zmail-app`.
    pub fn zmail(version: &str) -> Result<Self, Error> {
        Self::zmail_on(api_os(), version)
    }

    fn zmail_on(os: &str, version: &str) -> Result<Self, Error> {
        let valid = !version.is_empty()
            && version.starts_with(|ch: char| ch.is_ascii_digit())
            && version
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+'));
        if valid {
            Ok(Self {
                header: format!("{os}-bridge@{BRIDGE_VERSION}"),
                user_agent: format!("zmail/{version} ({os})"),
            })
        } else {
            Err(Error::Config(format!("invalid Zmail version {version:?}")))
        }
    }

    /// The `x-pm-appversion` value.
    pub fn as_str(&self) -> &str {
        &self.header
    }

    /// The `User-Agent` value.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

/// The platform part of the app version, as Bridge's `getAPIOS()` names it
/// (anything other than macOS and Windows is `linux`).
fn api_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// How the client trusts the server's certificate.
#[derive(Debug, Clone)]
enum Trust {
    /// The platform's roots plus Proton's SPKI pins (production).
    PinnedPlatformRoots(PinSet),
    /// Only the given CA, optionally with pins (the fake server harness and
    /// this crate's tests).
    #[cfg(any(test, feature = "test-harness"))]
    CustomCa {
        ca_pem: Vec<u8>,
        pins: Option<PinSet>,
    },
}

/// Configuration for a [`Manager`](crate::Manager).
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub(crate) base_url: Url,
    pub(crate) app_version: AppVersion,
    trust: Trust,
    pub(crate) retry: RetryPolicy,
}

impl ClientConfig {
    /// The production configuration: Proton's API host, Bridge's pins and
    /// the platform's trusted roots.
    pub fn production(app_version: AppVersion) -> Result<Self, Error> {
        Ok(Self {
            base_url: parse_base_url(API_HOST_URL)?,
            app_version,
            trust: Trust::PinnedPlatformRoots(PinSet::trusted_api_pins()),
            retry: RetryPolicy::default(),
        })
    }

    /// Points the client at the local fake Proton server.
    ///
    /// Only `https://` URLs on a loopback host (`127.0.0.1`, `::1` or
    /// `localhost`) are accepted. The client then trusts only the given CA
    /// (no platform roots) and skips SPKI pinning. The app version is
    /// Zmail's identity with this crate's version; use [`Self::with_app_version`]
    /// to change it.
    #[cfg(feature = "test-harness")]
    pub fn for_test_harness(base_url: &str, ca_pem: &[u8]) -> Result<Self, Error> {
        Self::with_custom_ca(base_url, ca_pem, None)
    }

    #[cfg(any(test, feature = "test-harness"))]
    pub(crate) fn with_custom_ca(
        base_url: &str,
        ca_pem: &[u8],
        pins: Option<PinSet>,
    ) -> Result<Self, Error> {
        let url = parse_base_url(base_url)?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if !loopback {
            return Err(Error::Config(format!(
                "the test harness must run on a loopback host, not {base_url}"
            )));
        }
        roots_from_pem(ca_pem)?;
        Ok(Self {
            base_url: url,
            app_version: AppVersion::zmail(env!("CARGO_PKG_VERSION"))?,
            trust: Trust::CustomCa {
                ca_pem: ca_pem.to_vec(),
                pins,
            },
            retry: RetryPolicy::default(),
        })
    }

    /// Replaces the app version.
    pub fn with_app_version(mut self, app_version: AppVersion) -> Self {
        self.app_version = app_version;
        self
    }

    /// The API base URL.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// The `x-pm-appversion` value.
    pub fn app_version(&self) -> &AppVersion {
        &self.app_version
    }

    /// Builds the rustls configuration: aws-lc-rs, HTTP/1.1 only (Bridge's
    /// custom TLS dialer never negotiates HTTP/2), and the trust rules above.
    pub(crate) fn tls_config(&self) -> Result<TlsConfig, Error> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let (roots, pins) = match &self.trust {
            Trust::PinnedPlatformRoots(pins) => (platform_roots()?, Some(pins.clone())),
            #[cfg(any(test, feature = "test-harness"))]
            Trust::CustomCa { ca_pem, pins } => (roots_from_pem(ca_pem)?, pins.clone()),
        };
        let verifier =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()
                .map_err(|error| Error::Config(format!("TLS verifier: {error}")))?;
        let builder = TlsConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| Error::Config(format!("TLS versions: {error}")))?;
        let mut config = match pins {
            Some(pins) => builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(PinningVerifier::new(verifier, pins)))
                .with_no_client_auth(),
            None => builder.with_webpki_verifier(verifier).with_no_client_auth(),
        };
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }
}

fn parse_base_url(base_url: &str) -> Result<Url, Error> {
    let url = Url::parse(base_url)
        .map_err(|error| Error::Config(format!("invalid API URL {base_url:?}: {error}")))?;
    if url.scheme() != "https" {
        return Err(Error::Config(format!(
            "the Proton API must be reached over https://, not {base_url}"
        )));
    }
    Ok(url)
}

fn platform_roots() -> Result<RootCertStore, Error> {
    let mut roots = RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for certificate in loaded.certs {
        // Platform stores contain certificates rustls cannot parse; skip
        // them like zmail-imap does.
        let _ = roots.add(certificate);
    }
    if roots.is_empty() {
        return Err(Error::Config(
            "no trusted root certificates found on this system".to_owned(),
        ));
    }
    Ok(roots)
}

#[cfg(any(test, feature = "test-harness"))]
fn roots_from_pem(ca_pem: &[u8]) -> Result<RootCertStore, Error> {
    use tokio_rustls::rustls::pki_types::{CertificateDer, pem::PemObject};

    let mut roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(ca_pem) {
        let certificate =
            certificate.map_err(|error| Error::Config(format!("test CA PEM: {error}")))?;
        roots
            .add(certificate)
            .map_err(|error| Error::Config(format!("test CA: {error}")))?;
    }
    if roots.is_empty() {
        return Err(Error::Config(
            "the test CA PEM has no certificates".to_owned(),
        ));
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_version_is_bridges_with_a_zmail_user_agent() {
        let macos = AppVersion::zmail_on("macos", "0.14.0").expect("valid");
        assert_eq!(macos.as_str(), "macos-bridge@3.27.1");
        assert_eq!(macos.user_agent(), "zmail/0.14.0 (macos)");
        let linux = AppVersion::zmail_on("linux", "1.2.3-rc.1+build.5").expect("valid");
        assert_eq!(linux.as_str(), "linux-bridge@3.27.1");
        assert_eq!(linux.user_agent(), "zmail/1.2.3-rc.1+build.5 (linux)");
        for invalid in ["", "v1.0.0", "1.0 beta", "1.0@x", "linux-bridge@3.27.1"] {
            assert!(AppVersion::zmail(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn app_version_names_this_platform_like_bridge() {
        let os = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        };
        let version = AppVersion::zmail("0.14.0").expect("valid");
        assert_eq!(version.as_str(), format!("{os}-bridge@{BRIDGE_VERSION}"));
        assert_eq!(version.user_agent(), format!("zmail/0.14.0 ({os})"));
    }

    #[test]
    fn bridge_version_matches_the_upstream_pin() {
        // Bumping the Bridge pin bumps the app version Zmail sends.
        let pins = include_str!("../../../docs/upstream/proton.md");
        let row = pins
            .lines()
            .find(|line| line.contains("[ProtonMail/proton-bridge]"))
            .expect("Bridge pin row");
        assert!(
            row.contains(&format!("`v{BRIDGE_VERSION}`")),
            "BRIDGE_VERSION {BRIDGE_VERSION} does not match the pin: {row}"
        );
    }

    #[test]
    fn production_uses_proton_host_and_pins() {
        let config = ClientConfig::production(AppVersion::zmail("0.14.0").expect("valid"))
            .expect("production config");
        assert_eq!(config.base_url().as_str(), "https://mail-api.proton.me/");
        assert_eq!(
            config.app_version(),
            &AppVersion::zmail("0.14.0").expect("valid")
        );
        assert!(matches!(&config.trust, Trust::PinnedPlatformRoots(pins) if pins.len() == 13));
    }

    #[test]
    fn custom_ca_requires_https_on_loopback() {
        let ca = include_bytes!("testdata/ca.pem");
        for good in [
            "https://127.0.0.1:18443",
            "https://[::1]:18443",
            "https://localhost:18443",
            "https://LOCALHOST",
        ] {
            assert!(
                ClientConfig::with_custom_ca(good, ca, None).is_ok(),
                "{good}"
            );
        }
        for bad in [
            "http://127.0.0.1:18443",
            "https://mail-api.proton.me",
            "https://10.0.0.1",
            "https://localhost.example.com",
            "not a url",
        ] {
            assert!(
                ClientConfig::with_custom_ca(bad, ca, None).is_err(),
                "{bad}"
            );
        }
        assert!(ClientConfig::with_custom_ca("https://127.0.0.1", b"", None).is_err());
    }

    #[test]
    fn tls_configs_build() {
        let config = ClientConfig::with_custom_ca(
            "https://127.0.0.1:1",
            include_bytes!("testdata/ca.pem"),
            Some(PinSet::trusted_api_pins()),
        )
        .expect("config");
        let tls = config.tls_config().expect("tls");
        assert_eq!(tls.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }
}
