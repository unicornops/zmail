// Ported from go-srp `srp.go` (`modulusPubkey` and `readClearSignedMessage`)
// at v0.0.7 (167a266a601fa1875838578be2edfbd4e97e6358,
// https://github.com/ProtonMail/go-srp).
// Copyright (c) 2019 Proton Technologies AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust on rPGP (`pgp`)
// as a `proton_srp::ModulusSignatureVerifier`, so `proton-srp` runs without
// its own rPGP feature. The SRP arithmetic itself is `proton-srp`'s.

//! Checks the signature on the SRP modulus the server sends.
//!
//! Proton's server picks the SRP group modulus and signs it with a
//! hard-coded key. Without this check a malicious or intercepted server
//! could choose a weak group and attack the password; with it, only moduli
//! Proton signed are accepted.

use pgp::composed::{CleartextSignedMessage, Deserializable, SignedPublicKey};
use proton_srp::{ModulusSignatureVerifier, ModulusVerifyError};

/// go-srp's `modulusPubkey`: the key that signs every SRP modulus.
pub(crate) const MODULUS_PUBKEY: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----\r\n\r\nxjMEXAHLgxYJKwYBBAHaRw8BAQdAFurWXXwjTemqjD7CXjXVyKf0of7n9Ctm\r\nL8v9enkzggHNEnByb3RvbkBzcnAubW9kdWx1c8J3BBAWCgApBQJcAcuDBgsJ\r\nBwgDAgkQNQWFxOlRjyYEFQgKAgMWAgECGQECGwMCHgEAAPGRAP9sauJsW12U\r\nMnTQUZpsbJb53d0Wv55mZIIiJL2XulpWPQD/V6NglBd96lZKBmInSXX/kXat\r\nSv+y0io+LR8i2+jV+AbOOARcAcuDEgorBgEEAZdVAQUBAQdAeJHUz1c9+KfE\r\nkSIgcBRE3WuXC4oj5a2/U3oASExGDW4DAQgHwmEEGBYIABMFAlwBy4MJEDUF\r\nhcTpUY8mAhsMAAD/XQD8DxNI6E78meodQI+wLsrKLeHn32iLvUqJbVDhfWSU\r\nWO4BAMcm1u02t4VKw++ttECPt+HUgPUq5pqQWe5Q2cW4TMsE\r\n=Y4Mw\r\n-----END PGP PUBLIC KEY BLOCK-----";

/// Verifies the modulus signature with rPGP (`readClearSignedMessage`).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ModulusVerifier;

impl ModulusSignatureVerifier for ModulusVerifier {
    /// Always checks against [`MODULUS_PUBKEY`] (tracked against go-srp by
    /// the upstream check), whatever key `proton-srp` bundles.
    fn verify_and_extract_modulus(
        &self,
        modulus: &str,
        _server_key: &str,
    ) -> Result<String, ModulusVerifyError> {
        let (key, _) = SignedPublicKey::from_string(MODULUS_PUBKEY)
            .map_err(|error| ModulusVerifyError::KeyImport(error.to_string()))?;
        // rPGP refuses data after the signature, like go-srp's
        // `ErrDataAfterModulus`.
        let (message, _) = CleartextSignedMessage::from_string(modulus)
            .map_err(|error| ModulusVerifyError::CleartextParse(error.to_string()))?;
        message
            .verify(&key)
            .map_err(|error| ModulusVerifyError::SignatureVerification(error.to_string()))?;
        Ok(message.signed_text().trim().to_owned())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// go-srp `srp_test.go`: `testModulus` and `testModulusClearSign`.
    pub(crate) const TEST_MODULUS: &str = "W2z5HBi8RvsfYzZTS7qBaUxxPhsfHJFZpu3Kd6s1JafNrCCH9rfvPLrfuqocxWPgWDH2R8neK7PkNvjxto9TStuY5z7jAzWRvFWN9cQhAKkdWgy0JY6ywVn22+HFpF4cYesHrqFIKUPDMSSIlWjBVmEJZ/MusD44ZT29xcPrOqeZvwtCffKtGAIjLYPZIEbZKnDM1Dm3q2K/xS5h+xdhjnndhsrkwm9U9oyA2wxzSXFL+pdfj2fOdRwuR5nW0J2NFrq3kJjkRmpO/Genq1UW+TEknIWAb6VzJJJA244K/H8cnSx2+nSNZO3bbo6Ys228ruV9A8m6DhxmS+bihN3ttQ==";
    pub(crate) const TEST_MODULUS_CLEAR_SIGN: &str = "-----BEGIN PGP SIGNED MESSAGE-----
Hash: SHA256

W2z5HBi8RvsfYzZTS7qBaUxxPhsfHJFZpu3Kd6s1JafNrCCH9rfvPLrfuqocxWPgWDH2R8neK7PkNvjxto9TStuY5z7jAzWRvFWN9cQhAKkdWgy0JY6ywVn22+HFpF4cYesHrqFIKUPDMSSIlWjBVmEJZ/MusD44ZT29xcPrOqeZvwtCffKtGAIjLYPZIEbZKnDM1Dm3q2K/xS5h+xdhjnndhsrkwm9U9oyA2wxzSXFL+pdfj2fOdRwuR5nW0J2NFrq3kJjkRmpO/Genq1UW+TEknIWAb6VzJJJA244K/H8cnSx2+nSNZO3bbo6Ys228ruV9A8m6DhxmS+bihN3ttQ==
-----BEGIN PGP SIGNATURE-----
Version: ProtonMail
Comment: https://protonmail.com

wl4EARYIABAFAlwB1j0JEDUFhcTpUY8mAAD8CgEAnsFnF4cF0uSHKkXa1GIa
GO86yMV4zDZEZcDSJo0fgr8A/AlupGN9EdHlsrZLmTA1vhIx+rOgxdEff28N
kvNM7qIK
=q6vu
-----END PGP SIGNATURE-----";

    /// go-srp `TestReadClearSigned`.
    #[test]
    fn reads_and_verifies_the_signed_modulus() {
        let modulus = ModulusVerifier
            .verify_and_extract_modulus(TEST_MODULUS_CLEAR_SIGN, "")
            .expect("valid modulus");
        assert_eq!(modulus, TEST_MODULUS);
    }

    #[test]
    fn rejects_a_tampered_signature() {
        // go-srp swaps the character 100 bytes from the end for a "c".
        let cut = TEST_MODULUS_CLEAR_SIGN.len() - 100;
        let tampered = format!(
            "{}c{}",
            &TEST_MODULUS_CLEAR_SIGN[..cut],
            &TEST_MODULUS_CLEAR_SIGN[cut + 1..]
        );
        assert_ne!(tampered, TEST_MODULUS_CLEAR_SIGN);
        assert!(
            ModulusVerifier
                .verify_and_extract_modulus(&tampered, "")
                .is_err()
        );
    }

    #[test]
    fn rejects_a_tampered_modulus() {
        let tampered = TEST_MODULUS_CLEAR_SIGN.replacen("W2z5", "W2z6", 1);
        assert!(matches!(
            ModulusVerifier.verify_and_extract_modulus(&tampered, ""),
            Err(ModulusVerifyError::SignatureVerification(_))
        ));
    }

    #[test]
    fn rejects_data_after_the_signature() {
        let trailing = format!("{TEST_MODULUS_CLEAR_SIGN}data after modulus");
        assert!(matches!(
            ModulusVerifier.verify_and_extract_modulus(&trailing, ""),
            Err(ModulusVerifyError::CleartextParse(_))
        ));
    }

    #[test]
    fn rejects_an_unsigned_modulus() {
        let unsigned = "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nabc\n-----BEGIN PGP SIGNATURE-----\n\n-----END PGP SIGNATURE-----\n";
        assert!(
            ModulusVerifier
                .verify_and_extract_modulus(unsigned, "")
                .is_err()
        );
    }
}
