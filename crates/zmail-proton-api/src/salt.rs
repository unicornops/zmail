// Ported from go-proton-api `salt.go` and `salt_types.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust on `proton-srp`'s
// bcrypt mailbox hash (go-srp's `MailboxPassword`). A hashing failure is an
// error instead of an empty key password, and the result is zeroized.

//! Key salts and the salted key password (`SaltForKey`).
//!
//! A Proton user key is locked with `bcrypt(password, salt)`: the last 31
//! characters of the `$2y$10$...` hash, where the salt is the key's 16-byte
//! `KeySalt`. That salted key password is what Zmail keeps in the keychain
//! (`ProtonKeyPass`); the password itself is never stored.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{client::Client, manager::Request, response::Error};

/// One key's salt (`GET /core/v4/keys/salts`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Salt {
    /// The key ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// The base64 salt; missing on keys that have none.
    #[serde(rename = "KeySalt", default)]
    pub key_salt: Option<String>,
}

/// The salts for all of the user's keys.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Salts(pub Vec<Salt>);

/// The password that unlocks the user keys: the bcrypt hash of the key
/// password with the primary key's salt, without the `$2y$10$` prefix and
/// salt. Zeroized on drop and redacted from `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct SaltedKeyPass(Zeroizing<Vec<u8>>);

impl SaltedKeyPass {
    /// Wraps a salted key password read back from the keychain.
    pub fn from_secret(secret: &str) -> Self {
        Self(Zeroizing::new(secret.as_bytes().to_vec()))
    }

    /// The password bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The password as text for the keychain (bcrypt output is ASCII).
    pub fn to_secret_string(&self) -> Zeroizing<String> {
        Zeroizing::new(String::from_utf8_lossy(&self.0).into_owned())
    }
}

impl fmt::Debug for SaltedKeyPass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SaltedKeyPass([redacted])")
    }
}

impl Salts {
    /// Salts `key_pass` for the key `key_id` (`SaltForKey`).
    ///
    /// `key_pass` is the login password in one-password mode and the
    /// mailbox password in two-password mode (see
    /// [`key_password`](crate::key_password)).
    pub fn salt_for_key(&self, key_pass: &str, key_id: &str) -> Result<SaltedKeyPass, Error> {
        let salt = self
            .0
            .iter()
            .find(|salt| salt.id == key_id)
            .ok_or_else(|| Error::NoSaltForKey(key_id.to_owned()))?;
        let encoded = salt
            .key_salt
            .as_deref()
            .ok_or_else(|| Error::NoSaltForKey(key_id.to_owned()))?;
        let salt = BASE64
            .decode(encoded)
            .map_err(|error| Error::Key(format!("key salt for {key_id}: {error}")))?;
        let hashed = proton_srp::mailbox_password_hash(key_pass, &salt)
            .map_err(|error| Error::Key(format!("salting the key password: {error}")))?;
        // `hashed_password` is the 31 characters after `$2y$10$` and the
        // 22-character salt, the same as upstream's `[len-31:]`.
        Ok(SaltedKeyPass(Zeroizing::new(
            hashed.hashed_password().to_vec(),
        )))
    }
}

impl Client {
    /// Returns the key salts (`GetSalts`).
    pub async fn get_salts(&self) -> Result<Salts, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "KeySalts")]
            key_salts: Salts,
        }
        let res: Res = self.json(&Request::get("/core/v4/keys/salts")).await?;
        Ok(res.key_salts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// proton-crypto-account `tests/keys.rs` (`get_test_salts`).
    pub(crate) const TEST_KEY_ID: &str =
        "aTdvCsWuv2V_YQQ5nLKsWPkHWMrlHfUxL9aTWakz6blhwI0q_j4MKnxO29xMQ4slCRvo3lFLE8ljb3kvMP2PQQ==";
    pub(crate) const TEST_KEY_SALT: &str = "6bIzN4A8bOwmsiEuCPj74g==";

    fn salts() -> Salts {
        Salts(vec![
            Salt {
                id: "other".to_owned(),
                key_salt: Some(BASE64.encode([0u8; 16])),
            },
            Salt {
                id: TEST_KEY_ID.to_owned(),
                key_salt: Some(TEST_KEY_SALT.to_owned()),
            },
            Salt {
                id: "unsalted".to_owned(),
                key_salt: None,
            },
        ])
    }

    #[test]
    fn salted_key_pass_is_the_bcrypt_hash_suffix() {
        let pass = salts()
            .salt_for_key("password", TEST_KEY_ID)
            .expect("salted");
        assert_eq!(pass.as_bytes().len(), 31);
        // bcrypt's $2y$ base64 alphabet only.
        assert!(
            pass.as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/'))
        );
        let again = salts()
            .salt_for_key("password", TEST_KEY_ID)
            .expect("salted");
        assert_eq!(pass, again);
        let other = salts()
            .salt_for_key("password1", TEST_KEY_ID)
            .expect("salted");
        assert_ne!(pass, other);
        assert_eq!(format!("{pass:?}"), "SaltedKeyPass([redacted])");
        assert_eq!(SaltedKeyPass::from_secret(&pass.to_secret_string()), pass);
    }

    /// jBCrypt's published vectors at cost 10, cross-checked with
    /// libxcrypt (`crypt("abc", "$2y$10$WvvTPHKwdBJ3uk0Z37EMR.")`). The
    /// salts are the bcrypt-base64 salts decoded to their 16 raw bytes.
    #[test]
    fn matches_known_bcrypt_vectors() {
        let salts = Salts(vec![
            Salt {
                id: "abc".to_owned(),
                key_salt: Some("YxxVRJMyfDL5wm2b59GOTA==".to_owned()),
            },
            Salt {
                id: "zero".to_owned(),
                key_salt: Some(BASE64.encode([0u8; 16])),
            },
        ]);
        let abc = salts.salt_for_key("abc", "abc").expect("salted");
        assert_eq!(abc.as_bytes(), b"hLA2W6N9AEBhEgrAOljy2Ae5MtaSIUi");
        let zero = salts.salt_for_key("password", "zero").expect("salted");
        assert_eq!(zero.as_bytes(), b"7zqaLmaKtn.i7IjPfuPGY2Ah/mNM6Sy");
    }

    #[test]
    fn missing_salts_are_errors() {
        assert!(matches!(
            salts().salt_for_key("password", "missing"),
            Err(Error::NoSaltForKey(_))
        ));
        assert!(matches!(
            salts().salt_for_key("password", "unsalted"),
            Err(Error::NoSaltForKey(_))
        ));
    }
}
