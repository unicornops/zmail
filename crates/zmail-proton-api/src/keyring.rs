// Ported from go-proton-api `keyring.go` (`Key`, `Keys`, `Unlock`,
// `TryUnlock`, `getPassphraseFromToken`, `unlock`) and `keys_types.go`
// (`KeyState`) at 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust on rPGP (`pgp`)
// instead of gopenpgp (ADR 0004). Keys stay armored until unlocked, and
// unlocking removes the password from every secret (sub)key. Upstream
// relies on go-crypto validating private key material when it is
// decrypted; rPGP does not, so `check_key` signs and verifies (and
// encrypts and decrypts) a probe with each unlocked key to catch key
// overwriting. `Primary` and `ByID` return `Option` instead of panicking.
// `ExtractSignatures`, `DecodeKeyPacket` and key list signing are not
// ported.
// Modified by UnicornOps on 2026-10-24: hands rPGP the rand 0.10 thread RNG
// that the rpgp/rpgp#573 fork (RustCrypto 0.14 stack) takes; no change in
// behaviour.
// Modified by UnicornOps on 2026-10-24: a key's self-signatures are checked
// before it is unlocked, as go-crypto does when it reads a key (`cert.rs`):
// parts that do not verify and public-only subkeys are dropped, and a key
// without a valid self-signature does not unlock. Detached signatures are
// only checked with keys whose self-signature allows signing.
// Modified by UnicornOps on 2026-11-10: a key only unlocks when its primary
// key and every secret subkey is password-protected. gopenpgp's
// `Key.Unlock` refuses a key with no protected part ("key is not locked");
// rPGP's `remove_password` leaves unprotected material as it is, so without
// the check any passphrase would "unlock" a key the server made up. Zmail
// is stricter than upstream for a key with some parts protected and some
// not, which gopenpgp accepts. Decrypted data is read up to a limit, so a
// compressed message cannot expand without bound (`MAX_DECRYPTED_BYTES`).

//! User and address keys, and unlocking them into a [`KeyRing`].
//!
//! User keys are locked with the salted key password. Address keys are
//! either locked with a random token that is encrypted to, and signed by,
//! the user keys, or (on older "legacy" accounts) with the salted key
//! password itself.

use std::{fmt, io::Read};

use pgp::{
    composed::{Deserializable, DetachedSignature, Message, MessageBuilder, SignedSecretKey},
    crypto::{hash::HashAlgorithm, sym::SymmetricKeyAlgorithm},
    types::{EncryptionKey, KeyDetails, Password, SigningKey, VerifyingKey},
};
use serde::{Deserialize, Deserializer};
use zeroize::Zeroizing;

use crate::{boolean::api_bool, cert, response::Error};

/// Key flags (go-proton-api's `KeyState`, a bit set).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct KeyFlags(pub u32);

impl KeyFlags {
    /// The key is not compromised: signatures from it can be trusted.
    pub const TRUSTED: u32 = 1;
    /// The key is still in use: mail can be encrypted to it.
    pub const ACTIVE: u32 = 2;

    /// Whether signatures from the key can be trusted.
    pub fn is_trusted(self) -> bool {
        self.0 & Self::TRUSTED != 0
    }

    /// Whether mail can be encrypted to the key.
    pub fn is_active(self) -> bool {
        self.0 & Self::ACTIVE != 0
    }
}

/// A locked private key as the API returns it.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Key {
    /// Key ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// The armored, password-protected private key.
    #[serde(rename = "PrivateKey")]
    pub private_key: String,
    /// For address keys: the key's passphrase, encrypted to the user keys.
    #[serde(rename = "Token", default, deserialize_with = "non_empty")]
    pub token: Option<String>,
    /// For address keys: the user keys' signature over the token.
    #[serde(rename = "Signature", default, deserialize_with = "non_empty")]
    pub signature: Option<String>,
    /// Whether this is the primary key.
    #[serde(rename = "Primary", default, deserialize_with = "api_bool")]
    pub primary: bool,
    /// Whether the key is active; inactive keys are never unlocked.
    #[serde(rename = "Active", default, deserialize_with = "api_bool")]
    pub active: bool,
    /// Key flags.
    #[serde(rename = "Flags", default)]
    pub flags: KeyFlags,
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Key")
            .field("id", &self.id)
            .field("has_token", &self.token.is_some())
            .field("primary", &self.primary)
            .field("active", &self.active)
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

fn non_empty<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.filter(|value| !value.is_empty()))
}

/// A list of keys (go-proton-api's `Keys`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Keys(pub Vec<Key>);

impl Keys {
    /// The primary key, if any (`Primary`).
    pub fn primary(&self) -> Option<&Key> {
        self.0.iter().find(|key| key.primary)
    }

    /// The key with the given ID, if any (`ByID`).
    pub fn by_id(&self, key_id: &str) -> Option<&Key> {
        self.0.iter().find(|key| key.id == key_id)
    }

    /// Unlocks every active key that opens with `passphrase` (or, for
    /// address keys with a token, with `user_keyring`), skipping the rest.
    /// Fails if none unlock (`Unlock`).
    pub fn unlock(
        &self,
        passphrase: &[u8],
        user_keyring: Option<&KeyRing>,
    ) -> Result<KeyRing, Error> {
        let keys: Vec<UnlockedKey> = self
            .0
            .iter()
            .filter(|key| key.active)
            // Upstream logs each failure and carries on.
            .filter_map(|key| key.unlock(passphrase, user_keyring).ok())
            .collect();
        if keys.is_empty() {
            return Err(Error::Key("not able to unlock any key".to_owned()));
        }
        Ok(KeyRing { keys })
    }

    /// Like [`Keys::unlock`], but `None` when nothing unlocks (`TryUnlock`).
    pub fn try_unlock(&self, passphrase: &[u8], user_keyring: Option<&KeyRing>) -> Option<KeyRing> {
        self.unlock(passphrase, user_keyring).ok()
    }
}

impl Key {
    /// Unlocks this key: with the token when it has one, otherwise with
    /// `passphrase` (`Key.Unlock`).
    pub fn unlock(
        &self,
        passphrase: &[u8],
        user_keyring: Option<&KeyRing>,
    ) -> Result<UnlockedKey, Error> {
        let secret = match (&self.token, &self.signature) {
            (Some(token), Some(signature)) => {
                let user_keyring = user_keyring
                    .ok_or_else(|| Error::Key("no user key was provided".to_owned()))?;
                passphrase_from_token(user_keyring, token, signature)?
            }
            _ => Zeroizing::new(passphrase.to_vec()),
        };
        let (key, _) = SignedSecretKey::from_string(&self.private_key).map_err(key_error)?;
        // Only the parts whose self-signatures verify are unlocked or used.
        let mut key = cert::verify_secret(key)?;
        ensure_locked(&key)?;
        let password = Password::from(secret.as_slice());
        key.primary_key
            .remove_password(&password)
            .map_err(key_error)?;
        for subkey in &mut key.secret_subkeys {
            subkey.key.remove_password(&password).map_err(key_error)?;
        }
        check_key(&key)?;
        Ok(UnlockedKey {
            id: self.id.clone(),
            primary: self.primary,
            flags: self.flags,
            key,
        })
    }
}

/// Refuses a key with any secret part that is not password-protected.
///
/// The password is what ties a key to the account: a user key opens with
/// the salted key password, an address key with a token the user keys
/// signed. A key that needs no password proves neither, so the server
/// could list one of its own.
fn ensure_locked(key: &SignedSecretKey) -> Result<(), Error> {
    let locked = key.primary_key.secret_params().is_encrypted()
        && key
            .secret_subkeys
            .iter()
            .all(|subkey| subkey.key.secret_params().is_encrypted());
    if locked {
        Ok(())
    } else {
        Err(Error::Key("key is not locked".to_owned()))
    }
}

/// Decrypts an address key token with the user keys and checks the user
/// keys' signature over it (`getPassphraseFromToken`).
fn passphrase_from_token(
    user_keyring: &KeyRing,
    token: &str,
    signature: &str,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let passphrase = user_keyring.decrypt_armored_within(token, MAX_TOKEN_BYTES)?;
    user_keyring.verify_detached_armored(&passphrase, signature)?;
    Ok(passphrase)
}

/// Checks that an unlocked key's private material matches its public
/// material, as go-crypto does when it decrypts a private key: every key
/// that can sign must produce a signature its public key verifies, and
/// every key that can encrypt must decrypt a message encrypted to it.
/// A server that tampered with the key (a "key overwriting" attack) fails
/// here instead of later leaking key material through bad signatures.
fn check_key(key: &SignedSecretKey) -> Result<(), Error> {
    let primary = key.primary_key.public_key();
    if primary.algorithm().can_sign() {
        check_signing(&key.primary_key, primary)?;
    }
    if primary.algorithm().can_encrypt() {
        check_encryption(key, primary)?;
    }
    for subkey in &key.secret_subkeys {
        let public = subkey.key.public_key();
        if public.algorithm().can_sign() {
            check_signing(&subkey.key, public)?;
        }
        if public.algorithm().can_encrypt() {
            check_encryption(key, public)?;
        }
    }
    Ok(())
}

const PROBE: &[u8] = b"zmail key check";

fn check_signing(secret: &impl SigningKey, public: &impl VerifyingKey) -> Result<(), Error> {
    DetachedSignature::sign_binary_data(
        &mut rand::rng(),
        secret,
        &Password::empty(),
        HashAlgorithm::Sha256,
        PROBE,
    )
    .and_then(|signature| signature.verify(public, PROBE))
    .map_err(|error| Error::Key(format!("key check failed: {error}")))
}

fn check_encryption(key: &SignedSecretKey, public: &impl EncryptionKey) -> Result<(), Error> {
    let mut rng = rand::rng();
    let mut builder = MessageBuilder::from_bytes("", PROBE.to_vec())
        .seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES256);
    builder
        .encrypt_to_key(&mut rng, public)
        .map_err(key_error)?;
    let encrypted = builder.to_vec(&mut rng).map_err(key_error)?;
    let decrypted = decrypt_bytes(&encrypted, &[key])?;
    if decrypted.as_slice() == PROBE {
        Ok(())
    } else {
        Err(Error::Key("key check failed: wrong plaintext".to_owned()))
    }
}

/// An unlocked private key. Its secret material is zeroized on drop.
pub struct UnlockedKey {
    /// Key ID.
    pub id: String,
    /// Whether this is the primary key.
    pub primary: bool,
    /// Key flags.
    pub flags: KeyFlags,
    key: SignedSecretKey,
}

impl UnlockedKey {
    /// The unlocked rPGP key (no password needed for its operations).
    pub fn secret_key(&self) -> &SignedSecretKey {
        &self.key
    }
}

impl fmt::Debug for UnlockedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnlockedKey")
            .field("id", &self.id)
            .field("primary", &self.primary)
            .field("flags", &self.flags)
            .field("fingerprint", &self.key.fingerprint())
            .finish_non_exhaustive()
    }
}

/// A set of unlocked keys (gopenpgp's `KeyRing`).
#[derive(Debug, Default)]
pub struct KeyRing {
    keys: Vec<UnlockedKey>,
}

impl KeyRing {
    /// The unlocked keys.
    pub fn keys(&self) -> &[UnlockedKey] {
        &self.keys
    }

    /// The number of unlocked keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether no key is unlocked.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The first key (gopenpgp's `FirstKey`): the one go-proton-api
    /// encrypts drafts and attachments with.
    pub fn first(&self) -> Option<&UnlockedKey> {
        self.keys.first()
    }

    /// The primary key, if it is unlocked.
    pub fn primary(&self) -> Option<&UnlockedKey> {
        self.keys.iter().find(|key| key.primary)
    }

    /// Decrypts an armored message with any of the keys. The plaintext is
    /// zeroized on drop; signatures inside the message are not checked.
    /// Fails if the plaintext is over [`MAX_DECRYPTED_BYTES`].
    pub fn decrypt_armored(&self, armored: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.decrypt_armored_within(armored, MAX_DECRYPTED_BYTES)
    }

    /// Like [`KeyRing::decrypt_armored`], for data known to be small:
    /// fails if the plaintext is over `limit` bytes.
    pub fn decrypt_armored_within(
        &self,
        armored: &str,
        limit: usize,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let (message, _) = Message::from_string(armored).map_err(key_error)?;
        let secret_keys: Vec<&SignedSecretKey> = self.keys.iter().map(|key| &key.key).collect();
        decrypt_message(message, &secret_keys, limit)
    }

    /// Decrypts a binary OpenPGP message (such as an attachment's key
    /// packets followed by its data packets) with any of the keys. Fails
    /// if the plaintext is over [`MAX_DECRYPTED_BYTES`].
    pub fn decrypt_binary(&self, message: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        let secret_keys: Vec<&SignedSecretKey> = self.keys.iter().map(|key| &key.key).collect();
        decrypt_bytes(message, &secret_keys)
    }

    /// Verifies an armored detached signature over `data` with any of the
    /// keys: a primary key or subkey whose self-signature allows signing.
    pub fn verify_detached_armored(&self, data: &[u8], armored: &str) -> Result<(), Error> {
        let (signature, _) = DetachedSignature::from_string(armored).map_err(key_error)?;
        let verified = self.keys.iter().any(|unlocked| {
            let key = &unlocked.key;
            let primary = key.primary_key.public_key();
            (cert::primary_verifies(&key.details, primary)
                && signature.verify(primary, data).is_ok())
                || cert::verifying_subkeys(&key.secret_subkeys, |subkey| {
                    (subkey.key.public_key(), &subkey.signatures)
                })
                .any(|subkey| signature.verify(subkey.key.public_key(), data).is_ok())
        });
        if verified {
            Ok(())
        } else {
            Err(Error::Key("signature verification failed".to_owned()))
        }
    }
}

/// The most plaintext one message body or attachment may decrypt to.
///
/// A compressed OpenPGP message of a few kilobytes can expand to many
/// gigabytes, and anyone who has the user's public key can send one, so
/// decryption stops with an error here instead of running out of memory.
/// Proton limits a message with its attachments to 25 MB; this is five
/// times that, so no mail Proton accepts comes near it.
pub const MAX_DECRYPTED_BYTES: usize = 128 * 1024 * 1024;

/// The most an address key token may decrypt to. Tokens are 64 characters.
const MAX_TOKEN_BYTES: usize = 64 * 1024;

fn decrypt_bytes(bytes: &[u8], keys: &[&SignedSecretKey]) -> Result<Zeroizing<Vec<u8>>, Error> {
    let message = Message::from_bytes(bytes).map_err(key_error)?;
    decrypt_message(message, keys, MAX_DECRYPTED_BYTES)
}

fn decrypt_message(
    message: Message<'_>,
    keys: &[&SignedSecretKey],
    limit: usize,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let unlocked = Password::empty();
    let passwords = vec![&unlocked; keys.len()];
    let mut message = message
        .decrypt_with_keys(passwords, keys.to_vec())
        .map_err(key_error)?;
    if message.is_compressed() {
        message = message.decompress().map_err(key_error)?;
    }
    // Decompression happens as the message is read: read one byte past the
    // limit at most, and refuse the message if that byte is there.
    let mut data = Zeroizing::new(Vec::new());
    let allowed = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    (&mut message)
        .take(allowed)
        .read_to_end(&mut data)
        .map_err(|error| Error::Key(format!("reading decrypted data: {error}")))?;
    if data.len() > limit {
        return Err(Error::Key(format!(
            "the decrypted data is over the {limit}-byte limit"
        )));
    }
    Ok(data)
}

fn key_error(error: pgp::errors::Error) -> Error {
    Error::Key(error.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    //! Fixtures from proton-crypto-account's `tests/keys.rs` and
    //! `tests/common.rs` (proton-crypto-rs
    //! c468e282625206eda9b48d539949e6183b1da32a, Copyright (c) 2025 Proton
    //! AG, MIT License), in `testdata/keys/`:
    //!
    //! - `user-key-salted.asc`: a user key locked with the salted key
    //!   password for "password" and salt `6bIzN4A8bOwmsiEuCPj74g==`.
    //! - `user-key-password.asc`: a user key locked with "password".
    //! - `address-key.asc` with its token and token signature: an address
    //!   key whose token is encrypted to, and signed by,
    //!   `user-key-password.asc`.

    use super::*;

    pub(crate) const USER_KEY_SALTED: &str = include_str!("testdata/keys/user-key-salted.asc");
    pub(crate) const USER_KEY_PASSWORD: &str = include_str!("testdata/keys/user-key-password.asc");
    pub(crate) const ADDRESS_KEY: &str = include_str!("testdata/keys/address-key.asc");
    pub(crate) const ADDRESS_KEY_TOKEN: &str = include_str!("testdata/keys/address-key-token.asc");
    pub(crate) const ADDRESS_KEY_TOKEN_SIGNATURE: &str =
        include_str!("testdata/keys/address-key-token-signature.asc");
    pub(crate) const USER_KEY_ID: &str =
        "aTdvCsWuv2V_YQQ5nLKsWPkHWMrlHfUxL9aTWakz6blhwI0q_j4MKnxO29xMQ4slCRvo3lFLE8ljb3kvMP2PQQ==";
    pub(crate) const USER_KEY_SALT: &str = "6bIzN4A8bOwmsiEuCPj74g==";

    pub(crate) fn key(id: &str, armored: &str) -> Key {
        Key {
            id: id.to_owned(),
            private_key: armored.to_owned(),
            token: None,
            signature: None,
            primary: true,
            active: true,
            flags: KeyFlags(3),
        }
    }

    pub(crate) fn address_key() -> Key {
        Key {
            token: Some(ADDRESS_KEY_TOKEN.to_owned()),
            signature: Some(ADDRESS_KEY_TOKEN_SIGNATURE.to_owned()),
            ..key("address-key", ADDRESS_KEY)
        }
    }

    pub(crate) fn salted_password() -> crate::SaltedKeyPass {
        crate::Salts(vec![crate::Salt {
            id: USER_KEY_ID.to_owned(),
            key_salt: Some(USER_KEY_SALT.to_owned()),
        }])
        .salt_for_key("password", USER_KEY_ID)
        .expect("salted")
    }

    #[test]
    fn unlocks_user_keys_with_the_salted_key_password() {
        let keys = Keys(vec![key(USER_KEY_ID, USER_KEY_SALTED)]);
        let keyring = keys
            .unlock(salted_password().as_bytes(), None)
            .expect("unlock");
        assert_eq!(keyring.len(), 1);
        assert_eq!(
            keyring.primary().map(|key| key.id.as_str()),
            Some(USER_KEY_ID)
        );

        // The unsalted password, or a wrong one, unlocks nothing.
        assert!(keys.unlock(b"password", None).is_err());
        let wrong = crate::Salts(vec![crate::Salt {
            id: USER_KEY_ID.to_owned(),
            key_salt: Some(USER_KEY_SALT.to_owned()),
        }])
        .salt_for_key("password1", USER_KEY_ID)
        .expect("salted");
        assert!(keys.try_unlock(wrong.as_bytes(), None).is_none());
    }

    /// go-proton-api `TestKeyring_Unlock`: keys that do not open are
    /// skipped, inactive keys are never tried.
    #[test]
    fn skips_keys_that_do_not_unlock() {
        let mut inactive = key("inactive", USER_KEY_PASSWORD);
        inactive.active = false;
        let keys = Keys(vec![
            key("good-1", USER_KEY_PASSWORD),
            key(USER_KEY_ID, USER_KEY_SALTED),
            key("good-2", USER_KEY_PASSWORD),
            inactive,
        ]);
        assert!(keys.unlock(b"ugly_phrase", None).is_err());
        let keyring = keys.unlock(b"password", None).expect("unlock");
        let ids: Vec<&str> = keyring.keys().iter().map(|key| key.id.as_str()).collect();
        assert_eq!(ids, ["good-1", "good-2"]);
        assert_eq!(keys.by_id(USER_KEY_ID).map(|key| key.primary), Some(true));
        assert!(keys.by_id("missing").is_none());
    }

    #[test]
    fn unlocks_address_keys_through_the_signed_token() {
        let user_keyring = Keys(vec![key("user", USER_KEY_PASSWORD)])
            .unlock(b"password", None)
            .expect("user keys");
        let address = Keys(vec![address_key()]);

        // The passphrase argument is ignored when the key has a token.
        let keyring = address
            .unlock(b"irrelevant", Some(&user_keyring))
            .expect("address keys");
        assert_eq!(keyring.len(), 1);

        // Without the user keys, or with the wrong ones, nothing unlocks.
        assert!(address.unlock(b"password", None).is_err());
        let other_user = Keys(vec![key(USER_KEY_ID, USER_KEY_SALTED)])
            .unlock(salted_password().as_bytes(), None)
            .expect("other user key");
        assert!(address.unlock(b"password", Some(&other_user)).is_err());
    }

    #[test]
    fn rejects_a_token_with_a_bad_signature() {
        let user_keyring = Keys(vec![key("user", USER_KEY_PASSWORD)])
            .unlock(b"password", None)
            .expect("user keys");
        // A valid signature, but over different data: verification must
        // fail even though the token decrypts.
        let token = user_keyring
            .decrypt_armored(ADDRESS_KEY_TOKEN)
            .expect("token decrypts");
        assert!(
            user_keyring
                .verify_detached_armored(&token, ADDRESS_KEY_TOKEN_SIGNATURE)
                .is_ok()
        );
        assert!(
            user_keyring
                .verify_detached_armored(b"another token", ADDRESS_KEY_TOKEN_SIGNATURE)
                .is_err()
        );
    }

    #[test]
    fn unlocks_legacy_address_keys_with_the_salted_key_password() {
        // A legacy address key has no token: it is locked with the salted
        // key password like the user keys.
        let legacy = Keys(vec![key("legacy-address", USER_KEY_SALTED)]);
        let user_keyring = Keys(vec![key("user", USER_KEY_PASSWORD)])
            .unlock(b"password", None)
            .expect("user keys");
        let keyring = legacy
            .unlock(salted_password().as_bytes(), Some(&user_keyring))
            .expect("legacy address key");
        assert_eq!(keyring.len(), 1);
    }

    /// A server that swaps in other private material for a key's public
    /// key (key overwriting) must not get an unlocked key back.
    #[test]
    fn check_key_catches_mismatched_private_material() {
        let unlocked = |armored: &str, password: &str| {
            let (mut key, _) = SignedSecretKey::from_string(armored).expect("parse");
            let password = Password::from(password);
            key.primary_key.remove_password(&password).expect("primary");
            for subkey in &mut key.secret_subkeys {
                subkey.key.remove_password(&password).expect("subkey");
            }
            key
        };
        let a = unlocked(USER_KEY_PASSWORD, "password");
        let b = unlocked(USER_KEY_SALTED, &salted_password().to_secret_string());
        check_key(&a).expect("a is consistent");
        check_key(&b).expect("b is consistent");

        let mut primary_swapped = a.clone();
        primary_swapped.primary_key = pgp::packet::SecretKey::new(
            a.primary_key.public_key().clone(),
            b.primary_key.secret_params().clone(),
        )
        .expect("packet");
        assert!(check_key(&primary_swapped).is_err());

        let mut subkey_swapped = a.clone();
        subkey_swapped.secret_subkeys[0].key = pgp::packet::SecretSubkey::new(
            a.secret_subkeys[0].key.public_key().clone(),
            b.secret_subkeys[0].key.secret_params().clone(),
        )
        .expect("packet");
        assert!(check_key(&subkey_swapped).is_err());
    }

    fn armored(key: &SignedSecretKey) -> String {
        key.to_armored_string(pgp::composed::ArmorOptions::default())
            .expect("armor")
    }

    /// gopenpgp's `Key.Unlock` refuses a key that is not locked. rPGP's
    /// `remove_password` leaves such a key alone, so without the check any
    /// passphrase "unlocks" a key the server made up.
    #[test]
    fn refuses_an_unprotected_user_key() {
        let made_up = cert::fixtures::generate(true, 1, 0);
        let unprotected = key("made-up", &armored(&made_up));
        for passphrase in [&b""[..], b"password", salted_password().as_bytes()] {
            let error = unprotected.unlock(passphrase, None).expect_err("refused");
            assert!(error.to_string().contains("key is not locked"), "{error}");
        }
        assert!(Keys(vec![unprotected.clone()]).unlock(b"", None).is_err());

        // The same key, locked, unlocks with its password only.
        let locked = key(
            "locked",
            &armored(&cert::fixtures::locked(&made_up, "secret")),
        );
        assert!(locked.unlock(b"secret", None).is_ok());
        assert!(locked.unlock(b"", None).is_err());

        // Listed next to the user's real key, only the real key unlocks.
        let keys = Keys(vec![unprotected, key(USER_KEY_ID, USER_KEY_SALTED)]);
        let keyring = keys
            .unlock(salted_password().as_bytes(), None)
            .expect("the real key");
        let ids: Vec<&str> = keyring.keys().iter().map(|key| key.id.as_str()).collect();
        assert_eq!(ids, [USER_KEY_ID]);
    }

    /// An address key with no token is a legacy key, locked with the
    /// salted key password. An unprotected one listed first would become
    /// the key drafts are encrypted to and signed with.
    #[test]
    fn refuses_an_unprotected_address_key_without_a_token() {
        let user_keyring = Keys(vec![key("user", USER_KEY_PASSWORD)])
            .unlock(b"password", None)
            .expect("user keys");
        let made_up = armored(&cert::fixtures::generate(true, 1, 0));
        let unprotected = Key {
            primary: false,
            ..key("made-up", &made_up)
        };
        assert!(
            Keys(vec![unprotected.clone()])
                .unlock(b"password", Some(&user_keyring))
                .is_err()
        );

        // In front of the real address key, it is skipped.
        let keyring = Keys(vec![unprotected.clone(), address_key()])
            .unlock(b"password", Some(&user_keyring))
            .expect("the real address key");
        assert_eq!(keyring.len(), 1);
        assert_eq!(
            keyring.first().map(|key| key.id.as_str()),
            Some("address-key")
        );

        // Borrowing a real key's token and signature does not help either.
        let with_token = Key {
            token: Some(ADDRESS_KEY_TOKEN.to_owned()),
            signature: Some(ADDRESS_KEY_TOKEN_SIGNATURE.to_owned()),
            ..unprotected
        };
        let error = with_token
            .unlock(b"password", Some(&user_keyring))
            .expect_err("refused");
        assert!(error.to_string().contains("key is not locked"), "{error}");
    }

    /// Every secret part must be locked. gopenpgp accepts a key with some
    /// parts locked and some not; Zmail does not.
    #[test]
    fn refuses_a_mix_of_locked_and_unprotected_parts() {
        let plain = cert::fixtures::generate(true, 2, 0);
        let locked = cert::fixtures::locked(&plain, "secret");
        key("locked", &armored(&locked))
            .unlock(b"secret", None)
            .expect("fully locked key unlocks");

        let mut plain_subkey = locked.clone();
        plain_subkey.secret_subkeys[1] = plain.secret_subkeys[1].clone();
        let mut plain_primary = locked.clone();
        plain_primary.primary_key = plain.primary_key.clone();
        let mut only_primary_locked = plain.clone();
        only_primary_locked.primary_key = locked.primary_key.clone();

        for mixed in [&plain_subkey, &plain_primary, &only_primary_locked] {
            let error = key("mixed", &armored(mixed))
                .unlock(b"secret", None)
                .expect_err("refused");
            assert!(error.to_string().contains("key is not locked"), "{error}");
        }
    }

    fn zeros<'a>(
        keyring: &KeyRing,
        plain_len: u64,
    ) -> pgp::composed::MessageBuilder<
        'a,
        std::io::Take<std::io::Repeat>,
        pgp::composed::EncryptionSeipdV1,
    > {
        let key = keyring.first().expect("key").secret_key();
        let mut rng = rand::rng();
        let mut builder = MessageBuilder::from_reader("", std::io::repeat(0).take(plain_len))
            .seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES256);
        builder.compression(pgp::types::CompressionAlgorithm::ZLIB);
        builder
            .encrypt_to_key(&mut rng, &key.secret_subkeys[0].key.public_key())
            .expect("encrypt");
        builder
    }

    /// A message that is small as sent and large once decompressed:
    /// `plain_len` zero bytes, compressed and encrypted to `keyring`.
    pub(crate) fn compressed_zeros(keyring: &KeyRing, plain_len: u64) -> Vec<u8> {
        zeros(keyring, plain_len)
            .to_vec(&mut rand::rng())
            .expect("message")
    }

    /// [`compressed_zeros`], armored.
    pub(crate) fn compressed_zeros_armored(keyring: &KeyRing, plain_len: u64) -> String {
        zeros(keyring, plain_len)
            .to_armored_string(&mut rand::rng(), pgp::composed::ArmorOptions::default())
            .expect("message")
    }

    /// A compression bomb fails with an error at the limit instead of
    /// being expanded into memory.
    #[test]
    fn stops_decrypting_at_the_limit() {
        let keyring = Keys(vec![key("user", USER_KEY_PASSWORD)])
            .unlock(b"password", None)
            .expect("user keys");
        let keys: Vec<&SignedSecretKey> = keyring.keys().iter().map(|key| &key.key).collect();
        let decrypt = |bytes: &[u8], limit| {
            decrypt_message(Message::from_bytes(bytes).expect("message"), &keys, limit)
        };

        // 16 MiB of plaintext in a message of a few kilobytes.
        let plain_len = 16 * 1024 * 1024;
        let small = compressed_zeros(&keyring, plain_len);
        assert!(small.len() < 64 * 1024, "{} bytes", small.len());
        let limit = usize::try_from(plain_len).expect("fits");
        assert_eq!(decrypt(&small, limit).expect("at the limit").len(), limit);
        for limit in [limit - 1, 1024 * 1024, 0] {
            let error = decrypt(&small, limit).expect_err("over the limit");
            assert!(error.to_string().contains("-byte limit"), "{error}");
        }
        assert_eq!(
            keyring.decrypt_binary(&small).expect("decrypts").len(),
            limit
        );

        // The limit bodies and attachments get.
        let bomb = compressed_zeros(&keyring, MAX_DECRYPTED_BYTES as u64 + 1);
        assert!(bomb.len() < 1024 * 1024, "{} bytes", bomb.len());
        let error = keyring.decrypt_binary(&bomb).expect_err("over the limit");
        assert!(error.to_string().contains("-byte limit"), "{error}");
    }

    #[test]
    fn stops_decrypting_armored_data_at_the_limit() {
        let keyring = Keys(vec![key("user", USER_KEY_PASSWORD)])
            .unlock(b"password", None)
            .expect("user keys");
        let armored = compressed_zeros_armored(&keyring, 1024 * 1024);
        assert_eq!(
            keyring.decrypt_armored(&armored).expect("decrypts").len(),
            1024 * 1024
        );
        assert!(
            keyring
                .decrypt_armored_within(&armored, 1024 * 1024)
                .is_ok()
        );
        let error = keyring
            .decrypt_armored_within(&armored, 1024 * 1024 - 1)
            .expect_err("over the limit");
        assert!(error.to_string().contains("-byte limit"), "{error}");
    }

    #[test]
    fn decodes_api_keys() {
        let keys: Keys = serde_json::from_value(serde_json::json!([
            {"ID": "k1", "PrivateKey": "armored", "Token": null, "Signature": "", "Primary": 1, "Active": 1, "Flags": 3, "Version": 3},
            {"ID": "k2", "PrivateKey": "armored", "Token": "t", "Signature": "s", "Primary": 0, "Active": 0, "Flags": 1},
        ]))
        .expect("keys");
        assert_eq!(keys.primary().map(|key| key.id.as_str()), Some("k1"));
        assert!(keys.0[0].token.is_none() && keys.0[0].signature.is_none());
        assert!(keys.0[0].flags.is_active() && keys.0[0].flags.is_trusted());
        assert_eq!(keys.0[1].token.as_deref(), Some("t"));
        assert!(!keys.0[1].active && !keys.0[1].flags.is_active());
        assert!(!format!("{:?}", keys.0[0]).contains("armored"));
    }
}
