// Ported from go-proton-api `message_send_types.go` (`EncryptionScheme`,
// `SignatureType`, `MessageRecipient`, `MessagePackage`, `SessionKey`,
// `SendPreferences`, `SendDraftReq`, `AddMIMEPackage`, `AddTextPackage`,
// `newMIMEPackage`, `newTextPackage`, `encSplit`) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust on rPGP
// (`crypto.rs`). Recipients are kept in ordered maps, so packages are
// built (and checked) in address order rather than Go's random map order.
// `encSplit` takes the session key from the encryption instead of
// decrypting the key packet it has just made.

//! Send packages: the body encrypted once per format, with each
//! recipient's key packets or, for clear recipients, the session keys.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};

use crate::{
    crypto::{RecipientKey, SessionKey},
    keyring::UnlockedKey,
    response::Error,
};

/// `text/plain`.
pub const TEXT_PLAIN: &str = "text/plain";
/// `text/html`.
pub const TEXT_HTML: &str = "text/html";
/// `multipart/mixed`.
pub const MULTIPART_MIXED: &str = "multipart/mixed";

/// How a recipient's copy is protected (`EncryptionScheme`, a bit set
/// when a package serves several schemes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EncryptionScheme(pub i64);

impl EncryptionScheme {
    /// Proton to Proton (`InternalScheme`).
    pub const INTERNAL: Self = Self(1);
    /// Password-protected mail to outside Proton (`EncryptedOutsideScheme`).
    pub const ENCRYPTED_OUTSIDE: Self = Self(2);
    /// Unencrypted (`ClearScheme`).
    pub const CLEAR: Self = Self(4);
    /// PGP/Inline (`PGPInlineScheme`).
    pub const PGP_INLINE: Self = Self(8);
    /// PGP/MIME (`PGPMIMEScheme`).
    pub const PGP_MIME: Self = Self(16);
    /// Unencrypted but signed, as MIME (`ClearMIMEScheme`).
    pub const CLEAR_MIME: Self = Self(32);
}

/// How a recipient's copy is signed (`SignatureType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SignatureType(pub i64);

impl SignatureType {
    /// Not signed (`NoSignature`).
    pub const NONE: Self = Self(0);
    /// A detached signature (`DetachedSignature`).
    pub const DETACHED: Self = Self(1);
    /// An attached signature (`AttachedSignature`).
    pub const ATTACHED: Self = Self(2);
}

/// One recipient of a package (`MessageRecipient`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MessageRecipient {
    /// The recipient's scheme.
    #[serde(rename = "Type")]
    pub kind: EncryptionScheme,
    /// The signature type.
    #[serde(rename = "Signature")]
    pub signature: SignatureType,
    /// The body's session key, encrypted to the recipient, base64.
    #[serde(rename = "BodyKeyPacket", skip_serializing_if = "String::is_empty")]
    pub body_key_packet: String,
    /// Each attachment's session key, encrypted to the recipient, base64.
    #[serde(
        rename = "AttachmentKeyPackets",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub attachment_key_packets: BTreeMap<String, String>,
}

/// A session key in the clear, for clear recipients (`SessionKey`).
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PackageKey {
    /// The key, base64.
    #[serde(rename = "Key")]
    pub key: String,
    /// The algorithm (`aes256`).
    #[serde(rename = "Algorithm")]
    pub algorithm: String,
}

impl std::fmt::Debug for PackageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageKey")
            .field("algorithm", &self.algorithm)
            .finish_non_exhaustive()
    }
}

impl From<&SessionKey> for PackageKey {
    fn from(key: &SessionKey) -> Self {
        Self {
            key: key.base64_key(),
            algorithm: key.algorithm_name().to_owned(),
        }
    }
}

/// The body in one format, for the recipients that get that format
/// (`MessagePackage`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessagePackage {
    /// The recipients, by address.
    #[serde(rename = "Addresses")]
    pub addresses: BTreeMap<String, MessageRecipient>,
    /// The body's format.
    #[serde(rename = "MIMEType")]
    pub mime_type: String,
    /// Every recipient scheme in the package, or-ed together.
    #[serde(rename = "Type")]
    pub kind: EncryptionScheme,
    /// The encrypted body's data packet, base64.
    #[serde(rename = "Body")]
    pub body: String,
    /// The body's session key, when a clear recipient needs it.
    #[serde(rename = "BodyKey", skip_serializing_if = "Option::is_none")]
    pub body_key: Option<PackageKey>,
    /// The attachments' session keys, when a clear recipient needs them.
    #[serde(rename = "AttachmentKeys", skip_serializing_if = "BTreeMap::is_empty")]
    pub attachment_keys: BTreeMap<String, PackageKey>,
}

impl MessagePackage {
    fn new(mime_type: &str, data_packet: &[u8]) -> Self {
        Self {
            addresses: BTreeMap::new(),
            mime_type: mime_type.to_owned(),
            kind: EncryptionScheme::default(),
            body: BASE64.encode(data_packet),
            body_key: None,
            attachment_keys: BTreeMap::new(),
        }
    }
}

/// How to send to one recipient (`SendPreferences`).
#[derive(Debug, Clone, Default)]
pub struct SendPreferences {
    /// Whether to encrypt; then `pub_key` is the key to encrypt to.
    pub encrypt: bool,
    /// The recipient's key.
    pub pub_key: Option<RecipientKey>,
    /// How to sign.
    pub signature_type: SignatureType,
    /// The scheme, which decides the package.
    pub encryption_scheme: EncryptionScheme,
    /// The body format: `text/html`, `text/plain` or `multipart/mixed`.
    pub mime_type: String,
}

/// The packages to send a draft with (`SendDraftReq`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SendDraftReq {
    /// The packages.
    #[serde(rename = "Packages")]
    pub packages: Vec<MessagePackage>,
}

impl SendDraftReq {
    /// Adds a `multipart/mixed` package for PGP/MIME and clear MIME
    /// recipients (`AddMIMEPackage`).
    pub fn add_mime_package(
        &mut self,
        key: &UnlockedKey,
        mime_body: &str,
        prefs: &BTreeMap<String, SendPreferences>,
    ) -> Result<(), Error> {
        if let Some(pref) = prefs
            .values()
            .find(|pref| pref.mime_type != MULTIPART_MIXED)
        {
            return Err(package_error(format!(
                "invalid MIME type for MIME package: {}",
                pref.mime_type
            )));
        }
        let package = new_mime_package(key, mime_body, prefs)?;
        self.packages.push(package);
        Ok(())
    }

    /// Adds a `text/plain` or `text/html` package for internal, clear and
    /// PGP/Inline recipients (`AddTextPackage`).
    pub fn add_text_package(
        &mut self,
        key: &UnlockedKey,
        body: &str,
        mime_type: &str,
        prefs: &BTreeMap<String, SendPreferences>,
        att_keys: &BTreeMap<String, SessionKey>,
    ) -> Result<(), Error> {
        let package = new_text_package(key, body, mime_type, prefs, att_keys)?;
        self.packages.push(package);
        Ok(())
    }
}

fn new_mime_package(
    key: &UnlockedKey,
    mime_body: &str,
    prefs: &BTreeMap<String, SendPreferences>,
) -> Result<MessagePackage, Error> {
    let split = key.encrypt_text_split(mime_body)?;
    let mut package = MessagePackage::new(MULTIPART_MIXED, &split.data_packet);
    for (address, pref) in prefs {
        if pref.mime_type != MULTIPART_MIXED {
            return Err(package_error(format!(
                "invalid MIME type for MIME package: {}",
                pref.mime_type
            )));
        }
        if pref.signature_type != SignatureType::DETACHED {
            return Err(package_error(format!(
                "invalid signature type for MIME package: {}",
                pref.signature_type.0
            )));
        }
        let mut recipient = MessageRecipient {
            kind: pref.encryption_scheme,
            signature: pref.signature_type,
            ..MessageRecipient::default()
        };
        match pref.encryption_scheme {
            EncryptionScheme::PGP_MIME => {
                let pub_key = pref
                    .pub_key
                    .as_ref()
                    .ok_or_else(|| package_error(format!("missing public key for {address}")))?;
                recipient.body_key_packet = BASE64.encode(pub_key.encrypt_session_key(&split.key)?);
            }
            EncryptionScheme::CLEAR_MIME => {
                package.body_key = Some(PackageKey::from(&split.key));
            }
            other => {
                return Err(package_error(format!(
                    "invalid encryption scheme for MIME package: {}",
                    other.0
                )));
            }
        }
        package.addresses.insert(address.clone(), recipient);
        package.kind.0 |= pref.encryption_scheme.0;
    }
    Ok(package)
}

fn new_text_package(
    key: &UnlockedKey,
    body: &str,
    mime_type: &str,
    prefs: &BTreeMap<String, SendPreferences>,
    att_keys: &BTreeMap<String, SessionKey>,
) -> Result<MessagePackage, Error> {
    if mime_type != TEXT_PLAIN && mime_type != TEXT_HTML {
        return Err(package_error(format!(
            "invalid MIME type for package: {mime_type}"
        )));
    }
    let split = key.encrypt_text_split(body)?;
    let mut package = MessagePackage::new(mime_type, &split.data_packet);
    for (address, pref) in prefs {
        if pref.mime_type != mime_type {
            return Err(package_error(format!(
                "invalid MIME type for package: {}",
                pref.mime_type
            )));
        }
        if pref.signature_type == SignatureType::DETACHED && !pref.encrypt {
            if pref.encryption_scheme == EncryptionScheme::PGP_INLINE {
                return Err(package_error(format!(
                    "invalid encryption scheme for {address}: {}",
                    pref.encryption_scheme.0
                )));
            }
            if pref.encryption_scheme == EncryptionScheme::CLEAR && mime_type != TEXT_PLAIN {
                return Err(package_error(format!(
                    "invalid MIME type for clear package: {mime_type}"
                )));
            }
        }
        if pref.encryption_scheme == EncryptionScheme::INTERNAL && !pref.encrypt {
            return Err(package_error("internal packages must be encrypted"));
        }
        if pref.encryption_scheme == EncryptionScheme::PGP_INLINE && mime_type != TEXT_PLAIN {
            return Err(package_error(format!(
                "invalid MIME type for PGP inline package: {mime_type}"
            )));
        }
        match pref.encryption_scheme {
            EncryptionScheme::CLEAR => {
                package.body_key = Some(PackageKey::from(&split.key));
                for (att_id, att_key) in att_keys {
                    package
                        .attachment_keys
                        .insert(att_id.clone(), PackageKey::from(att_key));
                }
            }
            EncryptionScheme::INTERNAL | EncryptionScheme::PGP_INLINE => {}
            other => {
                return Err(package_error(format!(
                    "invalid encryption scheme for package: {}",
                    other.0
                )));
            }
        }
        let mut recipient = MessageRecipient {
            kind: pref.encryption_scheme,
            signature: pref.signature_type,
            ..MessageRecipient::default()
        };
        if pref.encrypt {
            let pub_key = pref
                .pub_key
                .as_ref()
                .ok_or_else(|| package_error(format!("missing public key for {address}")))?;
            if pref.signature_type != SignatureType::DETACHED {
                return Err(package_error(format!(
                    "invalid signature type for package: {}",
                    pref.signature_type.0
                )));
            }
            recipient.body_key_packet = BASE64.encode(pub_key.encrypt_session_key(&split.key)?);
            for (att_id, att_key) in att_keys {
                recipient.attachment_key_packets.insert(
                    att_id.clone(),
                    BASE64.encode(pub_key.encrypt_session_key(att_key)?),
                );
            }
        }
        package.addresses.insert(address.clone(), recipient);
        package.kind.0 |= pref.encryption_scheme.0;
    }
    Ok(package)
}

fn package_error(message: impl Into<String>) -> Error {
    Error::Package(message.into())
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

    fn internal(key: &UnlockedKey) -> SendPreferences {
        SendPreferences {
            encrypt: true,
            pub_key: Some(key.recipient_key()),
            signature_type: SignatureType::DETACHED,
            encryption_scheme: EncryptionScheme::INTERNAL,
            mime_type: TEXT_PLAIN.to_owned(),
        }
    }

    fn clear() -> SendPreferences {
        SendPreferences {
            encryption_scheme: EncryptionScheme::CLEAR,
            mime_type: TEXT_PLAIN.to_owned(),
            ..SendPreferences::default()
        }
    }

    #[test]
    fn text_packages_carry_key_packets_or_clear_keys() {
        let keyring = keyring();
        let key = keyring.first().expect("key");
        let att_key = key.encrypt_attachment(b"att", "a.txt").expect("att").key;
        let att_keys = BTreeMap::from([("att-1".to_owned(), att_key)]);
        let prefs = BTreeMap::from([
            ("in@proton.me".to_owned(), internal(key)),
            ("out@example.com".to_owned(), clear()),
        ]);
        let mut req = SendDraftReq::default();
        req.add_text_package(key, "hello", TEXT_PLAIN, &prefs, &att_keys)
            .expect("package");
        let package = &req.packages[0];
        assert_eq!(package.kind, EncryptionScheme(1 | 4));

        // The internal recipient decrypts the body with its key packet.
        let internal = &package.addresses["in@proton.me"];
        let mut message = BASE64.decode(&internal.body_key_packet).expect("b64");
        message.extend(BASE64.decode(&package.body).expect("b64"));
        assert_eq!(
            keyring
                .decrypt_binary(&message)
                .expect("decrypts")
                .as_slice(),
            b"hello"
        );
        assert!(internal.attachment_key_packets.contains_key("att-1"));

        let json = serde_json::to_value(&req).expect("json");
        let package = &json["Packages"][0];
        assert_eq!(package["MIMEType"], "text/plain");
        assert_eq!(package["BodyKey"]["Algorithm"], "aes256");
        assert!(package["AttachmentKeys"]["att-1"]["Key"].is_string());
        let clear = &package["Addresses"]["out@example.com"];
        assert_eq!(clear, &serde_json::json!({"Type": 4, "Signature": 0}));
        assert_eq!(package["Addresses"]["in@proton.me"]["Signature"], 1);
    }

    #[test]
    fn rejects_inconsistent_preferences_as_upstream_does() {
        let keyring = keyring();
        let key = keyring.first().expect("key");
        let none = BTreeMap::new();
        let mut req = SendDraftReq::default();

        let unencrypted_internal = SendPreferences {
            encrypt: false,
            ..internal(key)
        };
        let prefs = BTreeMap::from([("a".to_owned(), unencrypted_internal)]);
        assert!(
            req.add_text_package(key, "x", TEXT_PLAIN, &prefs, &none)
                .is_err()
        );

        let html_clear = BTreeMap::from([("a".to_owned(), clear())]);
        assert!(
            req.add_text_package(key, "x", TEXT_HTML, &html_clear, &none)
                .is_err(),
            "the recipient's format must match the package"
        );
        assert!(
            req.add_text_package(key, "x", MULTIPART_MIXED, &html_clear, &none)
                .is_err()
        );
        assert!(req.add_mime_package(key, "x", &html_clear).is_err());
        assert!(req.packages.is_empty());

        let clear_mime = SendPreferences {
            signature_type: SignatureType::DETACHED,
            encryption_scheme: EncryptionScheme::CLEAR_MIME,
            mime_type: MULTIPART_MIXED.to_owned(),
            ..SendPreferences::default()
        };
        let prefs = BTreeMap::from([("a".to_owned(), clear_mime)]);
        req.add_mime_package(key, "x", &prefs).expect("clear MIME");
        assert!(req.packages[0].body_key.is_some());
    }
}
