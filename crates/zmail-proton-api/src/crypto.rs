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
        SignedPublicKey, SignedPublicSubKey, SignedSecretKey, SignedSecretSubKey,
    },
    crypto::{hash::HashAlgorithm, sym::SymmetricKeyAlgorithm},
    packet::{DataMode, PacketTrait, PublicKeyEncryptedSessionKey},
    ser::Serialize,
    types::{EncryptionKey, KeyDetails, Password, SigningKey, Timestamp},
};

use crate::{
    cert::{self, Selected},
    keyring::UnlockedKey,
    response::Error,
};

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

/// A recipient's public key, checked when it was parsed: only the parts
/// whose signatures verify are kept, and a key without a valid
/// self-signature is refused (see `cert.rs`).
#[derive(Debug, Clone)]
pub struct RecipientKey(SignedPublicKey);

impl RecipientKey {
    /// Parses and checks an armored public key (`NewKeyFromArmored`).
    pub fn from_armored(armored: &str) -> Result<Self, Error> {
        let (key, _) = SignedPublicKey::from_string(armored).map_err(key_error)?;
        cert::verify_public(key).map(Self)
    }

    /// Parses and checks a binary public key (`NewKey`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let key = SignedPublicKey::from_bytes(bytes).map_err(key_error)?;
        cert::verify_public(key).map(Self)
    }

    /// The primary key's fingerprint, lower-case hex (`GetFingerprint`).
    pub fn fingerprint(&self) -> String {
        hex(self.0.fingerprint().as_bytes())
    }

    /// The key, binary (`Serialize`), as contact cards hold it.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        self.0.to_bytes().map_err(key_error)
    }

    /// Whether the key has a key that mail can be encrypted to now
    /// (gopenpgp's `CanEncrypt`).
    pub fn can_encrypt(&self) -> bool {
        self.encryption_key(Timestamp::now()).is_some()
    }

    /// Encrypts a session key to this key (`EncryptSessionKey`), returning
    /// the key packet. The key packet goes to the key go-crypto's
    /// `EncryptionKey` picks: the newest valid encryption subkey, else the
    /// primary key if it may encrypt.
    pub fn encrypt_session_key(&self, session: &SessionKey) -> Result<Vec<u8>, Error> {
        match self.encryption_key(Timestamp::now()) {
            Some(Selected::Subkey(subkey)) => session_key_packet(session, &subkey.key),
            Some(Selected::Primary) => session_key_packet(session, &self.0.primary_key),
            None => Err(Error::Key(format!(
                "the key {} has no valid key to encrypt to",
                self.fingerprint()
            ))),
        }
    }

    fn encryption_key(&self, now: Timestamp) -> Option<Selected<'_, SignedPublicSubKey>> {
        cert::encryption_key(
            &self.0.primary_key,
            &self.0.details,
            &self.0.public_subkeys,
            |subkey| (&subkey.key, &subkey.signatures),
            now,
        )
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
        // As for key packets: the key go-crypto's `EncryptionKey` picks,
        // and only one this key holds the secret for.
        let key = self.secret_key();
        match encryption_key(key)? {
            Selected::Subkey(subkey) => builder.encrypt_to_key(&mut rng, subkey.key.public_key()),
            Selected::Primary => builder.encrypt_to_key(&mut rng, key.primary_key.public_key()),
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
        let key = self.secret_key();
        let signer: &dyn SigningKey = match signing_key(key)? {
            Selected::Subkey(subkey) => &subkey.key,
            Selected::Primary => &key.primary_key,
        };
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

        match signing_key(self.secret_key())? {
            Selected::Subkey(subkey) => sign(&subkey.key, data),
            Selected::Primary => sign(&self.secret_key().primary_key, data),
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

/// The key that signs (go-crypto's `SigningKey`): the newest valid signing
/// subkey, else the primary key if it may sign.
fn signing_key(key: &SignedSecretKey) -> Result<Selected<'_, SignedSecretSubKey>, Error> {
    cert::signing_key(
        key.primary_key.public_key(),
        &key.details,
        &key.secret_subkeys,
        |subkey| (subkey.key.public_key(), &subkey.signatures),
        Timestamp::now(),
    )
    .ok_or_else(cannot_sign)
}

/// The key an address key encrypts to itself with (go-crypto's
/// `EncryptionKey`), among the keys it holds the secret for.
fn encryption_key(key: &SignedSecretKey) -> Result<Selected<'_, SignedSecretSubKey>, Error> {
    cert::encryption_key(
        key.primary_key.public_key(),
        &key.details,
        &key.secret_subkeys,
        |subkey| (subkey.key.public_key(), &subkey.signatures),
        Timestamp::now(),
    )
    .ok_or_else(|| Error::Key("the address key has no valid key to encrypt to".to_owned()))
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

    /// Hostile certificates: what a malicious server or a tampered contact
    /// card could hand Zmail.
    mod hostile {
        use pgp::composed::{SignedPublicSubKey, SignedSecretKey, SignedSecretSubKey};

        use super::*;
        use crate::{
            KeyFlags,
            cert::fixtures::{
                Binding, DAY, binding, generate, inject_secret, key_id_hex, key_revocation, now,
                recipients, set_signatures, subkey, subkey_id, subkey_revocation,
                user_certification,
            },
            keyring::Key,
        };

        fn session() -> SessionKey {
            SessionKey {
                algorithm: SYMMETRIC,
                key: RawSessionKey::from(vec![7u8; 32]),
            }
        }

        /// The key's public certificate, through the parser Zmail uses
        /// for keys from the API and contact cards.
        fn public(key: &SignedSecretKey) -> Result<RecipientKey, Error> {
            RecipientKey::from_armored(
                &key.to_public_key()
                    .to_armored_string(ArmorOptions::default())
                    .expect("armor"),
            )
        }

        /// The key as an address key from the API, unlocked.
        fn own(key: &SignedSecretKey) -> Result<KeyRing, Error> {
            Keys(vec![Key {
                id: "address-key".to_owned(),
                private_key: key
                    .to_armored_string(ArmorOptions::default())
                    .expect("armor"),
                token: None,
                signature: None,
                primary: true,
                active: true,
                flags: KeyFlags(3),
            }])
            .unlock(b"", None)
        }

        fn key_packet_recipients(key: &RecipientKey) -> Result<Vec<String>, Error> {
            key.encrypt_session_key(&session())
                .map(|packet| recipients(&packet))
        }

        #[test]
        fn recipient_keys_never_use_a_subkey_with_a_forged_binding() {
            let mut key = generate(true, 1, 0);
            let valid = subkey_id(&key, 0);
            let attacker = generate(true, 1, 0);
            let injected = subkey(&attacker, 0);
            // Newer than the real binding and listed first: it would win
            // if its binding were not checked.
            let forged = binding(&key, &attacker.primary_key, &injected, Binding::encrypt(0));
            key.secret_subkeys.insert(
                0,
                SignedSecretSubKey {
                    key: injected,
                    signatures: vec![forged],
                },
            );

            let recipient = public(&key).expect("the primary key is valid");
            assert_eq!(
                recipient.0.public_subkeys.len(),
                1,
                "the forged subkey is dropped"
            );
            assert_eq!(
                key_packet_recipients(&recipient).expect("encrypts"),
                [valid]
            );

            // With only the forged subkey there is nothing to encrypt to.
            key.secret_subkeys.remove(1);
            let recipient = public(&key).expect("the primary key is valid");
            assert!(!recipient.can_encrypt());
            assert!(key_packet_recipients(&recipient).is_err());
        }

        #[test]
        fn recipient_keys_skip_revoked_expired_and_future_subkeys_and_prefer_the_newest() {
            let mut key = generate(true, 5, 0);
            let ids: Vec<String> = (0..5).map(|index| subkey_id(&key, index)).collect();
            let sign = |key: &SignedSecretKey, index: usize, binding_: Binding| {
                binding(key, &key.primary_key, &subkey(key, index), binding_)
            };
            // 0: valid, bound three days ago.
            let older = sign(&key, 0, Binding::encrypt(3));
            set_signatures(&mut key, 0, vec![older]);
            // 1: the newest binding, but revoked a day ago.
            let revoked = sign(&key, 1, Binding::encrypt(1));
            let revocation = subkey_revocation(&key, &subkey(&key, 1), 1);
            set_signatures(&mut key, 1, vec![revoked, revocation]);
            // 2: bound a day ago, but expired two days after it was made,
            // five days ago.
            let expired = sign(
                &key,
                2,
                Binding {
                    expires_after: Some(2 * DAY),
                    ..Binding::encrypt(1)
                },
            );
            set_signatures(&mut key, 2, vec![expired]);
            // 3: valid, bound two days ago: the newest usable one.
            let newer = sign(&key, 3, Binding::encrypt(2));
            set_signatures(&mut key, 3, vec![newer]);
            // 4: a binding made tomorrow is not in effect yet.
            let future = sign(
                &key,
                4,
                Binding {
                    created: now() + DAY,
                    ..Binding::encrypt(0)
                },
            );
            set_signatures(&mut key, 4, vec![future]);

            let recipient = public(&key).expect("key");
            assert_eq!(recipient.0.public_subkeys.len(), 5, "all bindings verify");
            assert_eq!(
                key_packet_recipients(&recipient).expect("encrypts"),
                [ids[3].clone()]
            );

            // Without subkey 3, the older valid subkey; order does not matter.
            key.secret_subkeys.remove(3);
            key.secret_subkeys.reverse();
            let recipient = public(&key).expect("key");
            assert_eq!(
                key_packet_recipients(&recipient).expect("encrypts"),
                [ids[0].clone()]
            );
        }

        #[test]
        fn refuses_keys_whose_self_signature_does_not_verify() {
            let mut key = generate(true, 1, 0);
            let attacker = generate(true, 0, 0);
            let genuine = user_certification(&key, &key.primary_key, None);
            key.details.users[0].signatures = vec![genuine];
            assert!(public(&key).is_ok());

            let forged = user_certification(&key, &attacker.primary_key, None);
            key.details.users[0].signatures = vec![forged];
            assert!(
                public(&key).is_err(),
                "public key with a forged self-signature"
            );
            assert!(own(&key).is_err(), "own key with a forged self-signature");
            let bytes = key.to_public_key().to_bytes().expect("bytes");
            assert!(
                RecipientKey::from_bytes(&bytes).is_err(),
                "contact card key"
            );
        }

        #[test]
        fn revoked_or_expired_primary_keys_cannot_be_encrypted_to() {
            let mut revoked = generate(true, 1, 0);
            assert!(public(&revoked).expect("key").can_encrypt());
            let revocation = key_revocation(&revoked);
            revoked.details.revocation_signatures.push(revocation);
            let recipient = public(&revoked).expect("a revoked key still parses");
            assert!(!recipient.can_encrypt());
            assert!(key_packet_recipients(&recipient).is_err());

            // Made a week ago, expiring after a day.
            let mut expired = generate(true, 1, 0);
            let certification = user_certification(&expired, &expired.primary_key, Some(DAY));
            expired.details.users[0].signatures = vec![certification];
            let recipient = public(&expired).expect("an expired key still parses");
            assert!(!recipient.can_encrypt());
            assert!(key_packet_recipients(&recipient).is_err());
        }

        #[test]
        fn drafts_never_encrypt_to_public_only_subkeys() {
            let mut key = generate(true, 1, 0);
            let own_subkey = subkey_id(&key, 0);
            let attacker = generate(true, 1, 0);
            let injected = subkey(&attacker, 0);
            // Even a valid, newer binding does not make a subkey without
            // its secret usable for encrypting to ourselves.
            let bound = binding(&key, &key.primary_key, &injected, Binding::encrypt(0));
            key.public_subkeys.push(SignedPublicSubKey {
                key: injected.public_key().clone(),
                signatures: vec![bound],
            });

            let keyring = own(&key).expect("unlocks");
            let unlocked = keyring.first().expect("key");
            let draft = unlocked.encrypt_text_armored("draft").expect("draft");
            assert_eq!(
                recipients(draft.as_bytes()),
                std::slice::from_ref(&own_subkey)
            );
            assert_eq!(
                recipients(
                    &unlocked
                        .encrypt_session_key(&session())
                        .expect("key packet")
                ),
                [own_subkey]
            );
            assert_eq!(unlocked.recipient_key().0.public_subkeys.len(), 1);

            // With no secret encryption subkey left, drafts fail instead.
            key.secret_subkeys.clear();
            let keyring = own(&key).expect("unlocks");
            assert!(
                keyring
                    .first()
                    .expect("key")
                    .encrypt_text_armored("draft")
                    .is_err()
            );
        }

        #[test]
        fn secret_subkeys_with_bad_bindings_never_sign_or_encrypt() {
            // A primary key that only certifies, so signing needs a subkey.
            let mut key = generate(false, 2, 3);
            let attacker = generate(true, 0, 0);
            let ids: Vec<String> = (0..5).map(|index| subkey_id(&key, index)).collect();
            let forge = |key: &SignedSecretKey, index: usize, binding_: Binding| {
                binding(key, &attacker.primary_key, &subkey(key, index), binding_)
            };
            let bind = |key: &SignedSecretKey, index: usize, binding_: Binding| {
                binding(key, &key.primary_key, &subkey(key, index), binding_)
            };
            // Encryption subkeys: 0 forged (and newest), 1 valid.
            let forged = forge(&key, 0, Binding::encrypt(0));
            set_signatures(&mut key, 0, vec![forged]);
            let valid = bind(&key, 1, Binding::encrypt(2));
            set_signatures(&mut key, 1, vec![valid]);
            // Signing subkeys: 2 forged, 3 bound without a back-signature
            // (both newer), 4 valid.
            let forged = forge(&key, 2, Binding::sign(0));
            set_signatures(&mut key, 2, vec![forged]);
            let no_backsig = bind(
                &key,
                3,
                Binding {
                    backsig: false,
                    ..Binding::sign(0)
                },
            );
            set_signatures(&mut key, 3, vec![no_backsig]);
            let valid = bind(&key, 4, Binding::sign(2));
            set_signatures(&mut key, 4, vec![valid]);
            let signer = key.secret_subkeys[4].key.public_key().clone();

            let keyring = own(&key).expect("unlocks");
            let unlocked = keyring.first().expect("key");
            assert_eq!(unlocked.secret_key().secret_subkeys.len(), 2);
            let draft = unlocked.encrypt_text_armored("draft").expect("draft");
            assert_eq!(recipients(draft.as_bytes()), [ids[1].clone()]);
            let signature = unlocked.sign_detached(b"data").expect("signs");
            let parsed = DetachedSignature::from_bytes(signature.as_slice()).expect("parses");
            parsed
                .verify(&signer, b"data")
                .expect("made by the valid subkey");
            assert_eq!(key_id_hex(&signer), ids[4]);

            // With only bad signing subkeys, signing fails.
            key.secret_subkeys.remove(4);
            let keyring = own(&key).expect("unlocks");
            let unlocked = keyring.first().expect("key");
            assert!(unlocked.sign_detached(b"data").is_err());
            assert!(unlocked.encrypt_text_split("text").is_err());
        }

        #[test]
        fn signatures_from_unbound_subkeys_do_not_verify() {
            let mut key = generate(true, 0, 0);
            let attacker = generate(true, 0, 1);
            let injected = subkey(&attacker, 0);
            let forged = binding(&key, &attacker.primary_key, &injected, Binding::sign(0));
            inject_secret(&mut key, injected.clone(), vec![forged]);
            let keyring = own(&key).expect("unlocks");

            fn armored(signer: &impl SigningKey) -> String {
                DetachedSignature::sign_binary_data(
                    &mut rand_pgp::rng(),
                    signer,
                    &Password::empty(),
                    HashAlgorithm::Sha256,
                    &b"token"[..],
                )
                .and_then(|signature| signature.to_armored_string(ArmorOptions::default()))
                .expect("signature")
            }
            assert!(
                keyring
                    .verify_detached_armored(b"token", &armored(&key.primary_key))
                    .is_ok()
            );
            assert!(
                keyring
                    .verify_detached_armored(b"token", &armored(&injected))
                    .is_err()
            );
        }
    }
}
