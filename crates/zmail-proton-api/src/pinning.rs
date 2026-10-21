// Ported from proton-bridge `internal/dialer/dialer_pinning.go`,
// `internal/dialer/dialer_pinning_checker.go` and
// `internal/dialer/dialer_pinning_checker_default.go` at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust as a rustls
// certificate verifier. The pins moved to `pins.txt`. Bridge's
// skip-chain-verification path for DoH proxies and its TLS issue reporting
// to reports.proton.me are not ported.

//! SPKI pinning for connections to Proton's API.
//!
//! Like Bridge's `PinningTLSDialer`, a connection is only accepted when the
//! server's chain verifies against the trusted roots **and** the leaf
//! certificate's public key is one of the trusted pins. Bridge also accepts
//! DoH-discovered proxies with self-signed certificates on the pin alone;
//! Zmail never routes through those proxies, so chain verification always
//! applies.

use std::{fmt, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};
use tokio_rustls::rustls::{
    self, CertificateError, DigitallySignedStruct, OtherError, SignatureScheme,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
};

/// Bridge's `TrustedAPIPins`, kept as data so the upstream check can compare
/// them with `dialer_pinning.go`.
const TRUSTED_API_PINS: &str = include_str!("pins.txt");

/// A set of trusted `pin-sha256` values (SHA-256 of a DER SubjectPublicKeyInfo).
#[derive(Clone, PartialEq, Eq)]
pub struct PinSet {
    pins: Vec<[u8; 32]>,
}

impl PinSet {
    /// The pins Proton Mail Bridge trusts for Proton's API.
    pub fn trusted_api_pins() -> Self {
        // `pins.txt` is part of the crate and a unit test parses it, so a
        // malformed line is a build-time bug, not a runtime condition.
        Self::parse(TRUSTED_API_PINS).unwrap_or_else(|_| Self { pins: Vec::new() })
    }

    /// Parses `pin-sha256="<base64>"` lines; `#` starts a comment.
    pub fn parse(text: &str) -> Result<Self, PinParseError> {
        let mut pins = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let error = || PinParseError { line: index + 1 };
            let encoded = line
                .strip_prefix("pin-sha256=\"")
                .and_then(|rest| rest.strip_suffix('"'))
                .ok_or_else(error)?;
            let digest: [u8; 32] = BASE64
                .decode(encoded)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(error)?;
            pins.push(digest);
        }
        Ok(Self { pins })
    }

    /// Builds a pin set from raw SPKI digests.
    pub fn from_digests(pins: impl IntoIterator<Item = [u8; 32]>) -> Self {
        Self {
            pins: pins.into_iter().collect(),
        }
    }

    /// Returns the number of pins.
    pub fn len(&self) -> usize {
        self.pins.len()
    }

    /// Returns whether the set has no pins (and so accepts no connection).
    pub fn is_empty(&self) -> bool {
        self.pins.is_empty()
    }

    /// Returns whether the certificate's public key is pinned
    /// (Bridge's `isCertFoundInKnownPins`).
    fn contains_certificate(&self, certificate: &CertificateDer<'_>) -> bool {
        spki_sha256(certificate).is_some_and(|digest| self.pins.contains(&digest))
    }
}

impl fmt::Debug for PinSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.pins.iter().map(|pin| BASE64.encode(pin)))
            .finish()
    }
}

/// A `pins.txt`-style line that is not a valid `pin-sha256` value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid pin-sha256 line {line}")]
pub struct PinParseError {
    /// The 1-based line number.
    pub line: usize,
}

/// The leaf certificate's public key matched no trusted pin
/// (Bridge's `ErrTLSMismatch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no TLS fingerprint match found")]
pub struct PinMismatch;

/// Returns the SHA-256 of the certificate's DER SubjectPublicKeyInfo.
fn spki_sha256(certificate: &CertificateDer<'_>) -> Option<[u8; 32]> {
    let parsed = webpki::EndEntityCert::try_from(certificate).ok()?;
    let spki = parsed.subject_public_key_info();
    Some(Sha256::digest(spki.as_ref()).into())
}

/// A rustls verifier that runs the normal WebPKI checks and then requires
/// the leaf's public key to be pinned (Bridge's `PinningTLSDialer` with the
/// default `TLSPinChecker`).
#[derive(Debug)]
pub(crate) struct PinningVerifier {
    inner: Arc<WebPkiServerVerifier>,
    pins: PinSet,
}

impl PinningVerifier {
    pub(crate) fn new(inner: Arc<WebPkiServerVerifier>, pins: PinSet) -> Self {
        Self { inner, pins }
    }
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // The chain must verify first; the pin is an extra requirement, never
        // a substitute.
        let verified = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        if self.pins.contains_certificate(end_entity) {
            Ok(verified)
        } else {
            Err(rustls::Error::InvalidCertificate(CertificateError::Other(
                OtherError(Arc::new(PinMismatch)),
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_rustls::rustls::pki_types::pem::PemObject;

    #[test]
    fn bundled_pins_match_bridge() {
        let pins = PinSet::parse(TRUSTED_API_PINS).expect("pins.txt parses");
        assert_eq!(pins.len(), 13);
        assert_eq!(PinSet::trusted_api_pins(), pins);
        let first = BASE64
            .decode("drtmcR2kFkM8qJClsuWgUzxgBkePfRCkRpqUesyDmeE=")
            .expect("base64");
        assert_eq!(pins.pins[0].as_slice(), first.as_slice());
    }

    #[test]
    fn parse_rejects_malformed_lines() {
        assert_eq!(
            PinSet::parse("# ok\npin-sha256=\"abc\"\n"),
            Err(PinParseError { line: 2 })
        );
        assert_eq!(
            PinSet::parse("sha256=\"drtmcR2kFkM8qJClsuWgUzxgBkePfRCkRpqUesyDmeE=\""),
            Err(PinParseError { line: 1 })
        );
        assert!(
            PinSet::parse("\n# only comments\n")
                .expect("empty")
                .is_empty()
        );
    }

    #[test]
    fn spki_digest_matches_openssl() {
        // `openssl x509 -pubkey -noout | openssl pkey -pubin -outform der |
        // openssl dgst -sha256 -binary | base64` on the test leaf.
        let leaf =
            CertificateDer::from_pem_slice(include_bytes!("testdata/leaf.pem")).expect("leaf pem");
        let digest = spki_sha256(&leaf).expect("spki");
        assert_eq!(
            BASE64.encode(digest),
            "w8Iej+c8CZbEzpfZigKNhMQF1/ZM9EOLj7sR10YYCMM="
        );
        let pins = PinSet::parse("pin-sha256=\"w8Iej+c8CZbEzpfZigKNhMQF1/ZM9EOLj7sR10YYCMM=\"")
            .expect("pin");
        assert!(pins.contains_certificate(&leaf));
        assert!(!PinSet::trusted_api_pins().contains_certificate(&leaf));
    }
}
