//! OpenPGP operations for drafts, attachments and send packages.
//!
//! These are the gopenpgp calls go-proton-api makes when it creates
//! drafts, uploads attachments and builds send packages (`Encrypt`,
//! `SplitMessage`, `EncryptAttachment`, `SignDetached`,
//! `EncryptSessionKey`), done with rPGP (ADR 0004). Zmail's own code, not
//! a port; the gopenpgp behaviour it matches is noted on each function.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pgp::{
    composed::{
        ArmorOptions, Deserializable, DetachedSignature, MessageBuilder, RawSessionKey,
        SignedPublicKey, SignedSecretKey,
    },
    crypto::{hash::HashAlgorithm, sym::SymmetricKeyAlgorithm},
    packet::{DataMode, PacketTrait, PublicKeyEncryptedSessionKey},
    ser::Serialize,
    types::{EncryptionKey, KeyDetails, Password, SigningKey},
};

use crate::{keyring::UnlockedKey, response::Error};

/// The symmetric algorithm for everything Zmail encrypts (gopenpgp's
/// default cipher).
const SYMMETRIC: SymmetricKeyAlgorithm = SymmetricKeyAlgorithm::AES256;

/// A message or attachment session key (gopenpgp's `SessionKey`).
#[derive(Clone)]
pub struct SessionKey {
    algorithm: SymmetricKeyAlgorithm,
    key: RawSessionKey,
}

impl SessionKey {
    /// The key, base64, as send packages carry it for clear recipients
    /// (`GetBase64Key`).
    pub fn base64_key(&self) -> String {
        BASE64.encode(self.key.as_ref())
    }

    /// The algorithm name the API expects (gopenpgp's `Algo`).
    pub fn algorithm_name(&self) -> &'static str {
        match self.algorithm {
            SymmetricKeyAlgorithm::AES128 => "aes128",
            SymmetricKeyAlgorithm::AES192 => "aes192",
            _ => "aes256",
        }
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionKey([redacted])")
    }
}

/// Data encrypted with a fresh session key, as separate parts
/// (gopenpgp's `PGPSplitMessage`): the session key, and the encrypted data
/// packet without any key packet.
#[derive(Debug)]
pub struct Split {
    /// The session key.
    pub key: SessionKey,
    /// The encrypted data packet.
    pub data_packet: Vec<u8>,
}

/// A recipient's public key.
#[derive(Debug, Clone)]
pub struct RecipientKey(SignedPublicKey);

impl RecipientKey {
    /// Parses an armored public key (`NewKeyFromArmored`).
    pub fn from_armored(armored: &str) -> Result<Self, Error> {
        SignedPublicKey::from_string(armored)
            .map(|(key, _)| Self(key))
            .map_err(key_error)
    }

    /// Parses a binary public key (`NewKey`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        SignedPublicKey::from_bytes(bytes)
            .map(Self)
            .map_err(key_error)
    }

    /// The primary key's fingerprint, lower-case hex (`GetFingerprint`).
    pub fn fingerprint(&self) -> String {
        hex(self.0.fingerprint().as_bytes())
    }

    /// The key, binary (`Serialize`), as contact cards hold it.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        self.0.to_bytes().map_err(key_error)
    }

    /// Encrypts a session key to this key (`EncryptSessionKey`), returning
    /// the key packet. Like go-crypto, the first subkey that can encrypt
    /// is used, else the primary key.
    pub fn encrypt_session_key(&self, session: &SessionKey) -> Result<Vec<u8>, Error> {
        let subkey = self
            .0
            .public_subkeys
            .iter()
            .map(|subkey| &subkey.key)
            .find(|key| key.algorithm().can_encrypt());
        match subkey {
            Some(subkey) => session_key_packet(session, subkey),
            None if self.0.primary_key.algorithm().can_encrypt() => {
                session_key_packet(session, &self.0.primary_key)
            }
            None => Err(Error::Key("the recipient key cannot encrypt".to_owned())),
        }
    }
}

fn session_key_packet(session: &SessionKey, key: &impl EncryptionKey) -> Result<Vec<u8>, Error> {
    let packet = PublicKeyEncryptedSessionKey::from_session_key_v3(
        &mut rand_pgp::rng(),
        &session.key,
        session.algorithm,
        key,
    )
    .map_err(key_error)?;
    let mut out = Vec::new();
    packet.to_writer_with_header(&mut out).map_err(key_error)?;
    Ok(out)
}

/// gopenpgp's `NewPlainMessageFromString` canonicalises line endings to
/// CRLF and marks the message as text.
fn canonicalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

impl UnlockedKey {
    /// This key's public key.
    pub fn recipient_key(&self) -> RecipientKey {
        RecipientKey(self.secret_key().to_public_key())
    }

    /// The armored public key (`GetArmoredPublicKey`).
    pub fn armored_public_key(&self) -> Result<String, Error> {
        self.secret_key()
            .to_public_key()
            .to_armored_string(ArmorOptions::default())
            .map_err(key_error)
    }

    /// The primary key's fingerprint, lower-case hex (`GetFingerprint`).
    pub fn fingerprint_hex(&self) -> String {
        hex(self.secret_key().fingerprint().as_bytes())
    }

    /// The key's first user ID, as go-crypto's `Identity.Name` has it
    /// (the whole user ID, such as `alice@proton.me <alice@proton.me>`).
    pub fn identity_name(&self) -> String {
        self.secret_key()
            .details
            .users
            .first()
            .map(|user| String::from_utf8_lossy(user.id.id()).into_owned())
            .unwrap_or_default()
    }

    /// Encrypts text to this key, unsigned, armored: go-proton-api's draft
    /// body encryption (`kr.Encrypt(NewPlainMessageFromString(body), nil)`).
    pub fn encrypt_text_armored(&self, text: &str) -> Result<String, Error> {
        let mut rng = rand_pgp::rng();
        let mut builder = MessageBuilder::from_bytes("", canonicalize(text).into_bytes())
            .seipd_v1(&mut rng, SYMMETRIC);
        builder.data_mode(DataMode::Utf8).map_err(key_error)?;
        // As for key packets: the first encryption subkey, else the primary.
        let public = self.secret_key().to_public_key();
        match public
            .public_subkeys
            .iter()
            .find(|subkey| subkey.key.algorithm().can_encrypt())
        {
            Some(subkey) => builder.encrypt_to_key(&mut rng, &subkey.key),
            None => builder.encrypt_to_key(&mut rng, &public.primary_key),
        }
        .map_err(key_error)?;
        builder
            .to_armored_string(&mut rng, ArmorOptions::default())
            .map_err(key_error)
    }

    /// Encrypts and signs text with a fresh session key and splits the
    /// result: go-proton-api's `encSplit` (`kr.Encrypt(msg, kr)` then
    /// `SplitMessage`), which send packages carry.
    pub fn encrypt_text_split(&self, text: &str) -> Result<Split, Error> {
        let mut rng = rand_pgp::rng();
        let signer = signing_key(self.secret_key())?;
        let mut builder = MessageBuilder::from_bytes("", canonicalize(text).into_bytes())
            .seipd_v1(&mut rng, SYMMETRIC);
        builder.data_mode(DataMode::Utf8).map_err(key_error)?;
        builder
            .sign_text()
            .sign(signer, Password::empty(), HashAlgorithm::Sha256);
        split(builder.session_key().clone(), builder.to_vec(&mut rng))
    }

    /// Encrypts an attachment with a fresh session key, unsigned, and
    /// splits the result (`EncryptAttachment`).
    pub fn encrypt_attachment(&self, data: &[u8], file_name: &str) -> Result<Split, Error> {
        let mut rng = rand_pgp::rng();
        let builder = MessageBuilder::from_bytes(file_name.to_owned(), data.to_vec())
            .seipd_v1(&mut rng, SYMMETRIC);
        split(builder.session_key().clone(), builder.to_vec(&mut rng))
    }

    /// The key packet that gives this key the session key.
    pub fn encrypt_session_key(&self, session: &SessionKey) -> Result<Vec<u8>, Error> {
        self.recipient_key().encrypt_session_key(session)
    }

    /// A binary detached signature over binary data (`SignDetached` of a
    /// `NewPlainMessage`).
    pub fn sign_detached(&self, data: &[u8]) -> Result<Vec<u8>, Error> {
        fn sign(key: &impl SigningKey, data: &[u8]) -> Result<Vec<u8>, Error> {
            DetachedSignature::sign_binary_data(
                &mut rand_pgp::rng(),
                key,
                &Password::empty(),
                HashAlgorithm::Sha256,
                data,
            )
            .and_then(|signature| signature.to_bytes())
            .map_err(key_error)
        }

        let key = self.secret_key();
        if key.primary_key.public_key().algorithm().can_sign() {
            return sign(&key.primary_key, data);
        }
        match signing_subkey(key) {
            Some(subkey) => sign(&subkey.key, data),
            None => Err(cannot_sign()),
        }
    }
}

/// With no key packets added, the builder's output is the data packet.
fn split(key: RawSessionKey, output: pgp::errors::Result<Vec<u8>>) -> Result<Split, Error> {
    Ok(Split {
        key: SessionKey {
            algorithm: SYMMETRIC,
            key,
        },
        data_packet: output.map_err(key_error)?,
    })
}

/// The key that signs: the primary key when it can, else the first
/// signing subkey (go-crypto's choice for an unlocked entity).
fn signing_key(key: &SignedSecretKey) -> Result<&dyn SigningKey, Error> {
    if key.primary_key.public_key().algorithm().can_sign() {
        return Ok(&key.primary_key);
    }
    signing_subkey(key)
        .map(|subkey| &subkey.key as &dyn SigningKey)
        .ok_or_else(cannot_sign)
}

fn signing_subkey(key: &SignedSecretKey) -> Option<&pgp::composed::SignedSecretSubKey> {
    key.secret_subkeys
        .iter()
        .find(|subkey| subkey.key.public_key().algorithm().can_sign())
}

fn cannot_sign() -> Error {
    Error::Key("the address key cannot sign".to_owned())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn key_error(error: pgp::errors::Error) -> Error {
    Error::Key(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::{
        KeyRing, Keys,
        tests::{USER_KEY_PASSWORD, key},
    };

    fn keyring() -> KeyRing {
        Keys(vec![key("k", USER_KEY_PASSWORD)])
            .unlock(b"password", None)
            .expect("unlock")
    }

    #[test]
    fn split_text_reassembles_into_a_signed_decryptable_message() {
        let keyring = keyring();
        let key = keyring.first().expect("key");
        let split = key.encrypt_text_split("hello\nsplit").expect("split");
        assert_eq!(split.key.algorithm_name(), "aes256");
        assert_eq!(split.key.base64_key().len(), 44);
        let mut message = key.encrypt_session_key(&split.key).expect("key packet");
        message.extend_from_slice(&split.data_packet);
        let plain = keyring.decrypt_binary(&message).expect("decrypts");
        assert_eq!(plain.as_slice(), b"hello\r\nsplit");
    }

    #[test]
    fn attachments_split_and_sign_detached() {
        let keyring = keyring();
        let key = keyring.first().expect("key");
        let split = key
            .encrypt_attachment(b"\x00binary\xff", "a.bin")
            .expect("attachment");
        let mut message = key.encrypt_session_key(&split.key).expect("key packet");
        message.extend_from_slice(&split.data_packet);
        assert_eq!(
            keyring
                .decrypt_binary(&message)
                .expect("decrypts")
                .as_slice(),
            b"\x00binary\xff"
        );

        let signature = key.sign_detached(b"attachment").expect("signature");
        let parsed = DetachedSignature::from_bytes(signature.as_slice()).expect("parses");
        parsed
            .verify(key.secret_key().primary_key.public_key(), b"attachment")
            .expect("verifies");
    }

    #[test]
    fn armored_drafts_and_public_keys() {
        let keyring = keyring();
        let key = keyring.first().expect("key");
        let armored = key.encrypt_text_armored("draft body").expect("armored");
        assert!(armored.starts_with("-----BEGIN PGP MESSAGE-----"));
        assert_eq!(
            keyring
                .decrypt_armored(&armored)
                .expect("decrypts")
                .as_slice(),
            b"draft body"
        );

        let public = RecipientKey::from_armored(&key.armored_public_key().expect("public"))
            .expect("public key");
        assert_eq!(public.fingerprint(), key.fingerprint_hex());
        assert_eq!(public.fingerprint().len(), 40);
        assert!(!key.identity_name().is_empty());
    }
}
