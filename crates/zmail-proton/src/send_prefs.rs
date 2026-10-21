// Ported from proton-bridge `internal/services/smtp/smtp_prefs.go` at
// v3.27.1 (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. Contact keys
// arrive parsed, and an unknown contact scheme is an error where Bridge
// panics. Bridge's comments are kept with the rules they explain.

//! Deciding how to send to each recipient: Bridge's send preferences.

use anyhow::{Result, anyhow, bail};
use zmail_proton_api::{
    ContactSettings, EncryptionScheme, MULTIPART_MIXED, MailSettings, PublicKey, RecipientKey,
    SendPreferences, SignatureType, TEXT_PLAIN,
};

const PGP_INLINE: &str = "pgp-inline";
const PGP_MIME: &str = "pgp-mime";
/// A mix between PGP/Inline and PGP/MIME used by Proton.
const PM_INTERNAL: &str = "internal";

/// The contact's settings in the form the rules below read them
/// (`contactSettings`).
#[derive(Default)]
struct VCardData {
    keys: Vec<RecipientKey>,
    scheme: String,
    sign: bool,
    sign_is_set: bool,
    encrypt: bool,
    encrypt_untrusted: bool,
    mime_type: String,
}

/// Converts the API settings into the local settings (`newContactSettings`).
fn new_contact_settings(settings: &ContactSettings) -> Result<VCardData> {
    let mut metadata = VCardData {
        mime_type: settings.mime_type.clone().unwrap_or_default(),
        // An unset "encrypt untrusted" means yes.
        encrypt_untrusted: settings.encrypt_untrusted.unwrap_or(true),
        encrypt: settings.encrypt.unwrap_or(false),
        keys: settings.keys.clone(),
        ..VCardData::default()
    };
    if let Some(sign) = settings.sign {
        metadata.sign = sign;
        metadata.sign_is_set = true;
    }
    if let Some(scheme) = settings.scheme {
        metadata.scheme = match scheme {
            EncryptionScheme::PGP_MIME => PGP_MIME.to_owned(),
            EncryptionScheme::PGP_INLINE => PGP_INLINE.to_owned(),
            other => bail!("unknown contact scheme {}", other.0),
        };
    }
    Ok(metadata)
}

/// Works out how to send to one recipient (`buildSendPrefs`).
pub(crate) fn build_send_prefs(
    contact_settings: &ContactSettings,
    mail_settings: &MailSettings,
    pub_keys: &[PublicKey],
    mime_type: &str,
    is_internal: bool,
) -> Result<SendPreferences> {
    let mut builder = SendPrefsBuilder::default();
    builder
        .set_pgp_settings(
            new_contact_settings(contact_settings)?,
            pub_keys,
            is_internal,
        )
        .map_err(|error| anyhow!("failed to set PGP settings: {error:#}"))?;
    builder.set_encryption_preferences(mail_settings);
    builder.set_mime_preferences(mime_type);
    Ok(builder.build())
}

#[derive(Default)]
struct SendPrefsBuilder {
    internal: bool,
    encrypt: Option<bool>,
    sign: Option<bool>,
    scheme: Option<String>,
    mime_type: Option<String>,
    public_key: Option<RecipientKey>,
}

impl SendPrefsBuilder {
    fn with_encrypt_default(&mut self, value: bool) {
        self.encrypt.get_or_insert(value);
    }

    fn should_encrypt(&self) -> bool {
        self.encrypt.unwrap_or(false)
    }

    fn with_sign_default(&mut self) {
        self.sign.get_or_insert(true);
    }

    fn should_sign(&self) -> bool {
        self.sign.unwrap_or(false)
    }

    fn with_scheme_default(&mut self, value: &str) {
        self.scheme.get_or_insert_with(|| value.to_owned());
    }

    fn scheme(&self) -> &str {
        self.scheme.as_deref().unwrap_or_default()
    }

    fn with_mime_type(&mut self, value: &str) {
        self.mime_type = Some(value.to_owned());
    }

    fn with_mime_type_default(&mut self, value: &str) {
        self.mime_type.get_or_insert_with(|| value.to_owned());
    }

    /// Converts the PGP scheme with a string value into a number value
    /// (`build`). The result has the following possible values:
    ///
    /// ```text
    /// {
    ///     encrypt: true | false,
    ///     sign: true | false,
    ///     pgpScheme: 1 (protonmail custom scheme)
    ///              | 2 (Protonmail scheme for encrypted-to-outside email)
    ///              | 4 (no cryptographic scheme)
    ///              | 8 (PGP/INLINE)
    ///              | 16 (PGP/MIME),
    ///     mimeType: 'text/html' | 'text/plain' | 'multipart/mixed',
    ///     publicKey: OpenPGPKey | undefined/null
    /// }
    /// ```
    fn build(self) -> SendPreferences {
        let encryption_scheme = if self.internal {
            EncryptionScheme::INTERNAL
        } else if self.should_sign() && self.should_encrypt() {
            if self.scheme() == PGP_INLINE {
                EncryptionScheme::PGP_INLINE
            } else {
                EncryptionScheme::PGP_MIME
            }
        } else if self.should_sign() && !self.should_encrypt() {
            if self.scheme() == PGP_INLINE {
                EncryptionScheme::CLEAR
            } else {
                EncryptionScheme::CLEAR_MIME
            }
        } else {
            EncryptionScheme::CLEAR
        };
        SendPreferences {
            encrypt: self.should_encrypt(),
            signature_type: if self.should_sign() {
                SignatureType::DETACHED
            } else {
                SignatureType::NONE
            },
            encryption_scheme,
            mime_type: self.mime_type.clone().unwrap_or_default(),
            pub_key: self.public_key,
        }
    }

    /// Reflects the vCard content plus the public key info retrieved from
    /// the API via the GET KEYS route (`setPGPSettings`).
    fn set_pgp_settings(
        &mut self,
        vcard: VCardData,
        api_keys: &[PublicKey],
        is_internal: bool,
    ) -> Result<()> {
        // Sending internal. We are guaranteed to always receive API keys.
        if is_internal {
            self.internal = true;
            return self.set_internal_pgp_settings(&vcard, api_keys);
        }

        // Sending external but with keys supplied by WKD. Treated pretty
        // much the same as internal.
        if !api_keys.is_empty() {
            let usable = filter_usable_wkd_keys(api_keys);
            if !usable.is_empty() {
                return self.set_external_pgp_settings_with_wkd_keys(&vcard, &usable);
            }
        }

        // Sending external without any WKD keys. If we have a contact
        // saved, we can use its settings.
        self.set_external_pgp_settings_without_wkd_keys(&vcard)
    }

    /// Internal addresses: Proton's own domains, or a custom domain
    /// registered with Proton (`setInternalPGPSettings`).
    fn set_internal_pgp_settings(
        &mut self,
        vcard: &VCardData,
        api_keys: &[PublicKey],
    ) -> Result<()> {
        // We're guaranteed to get at least one valid (i.e. not expired,
        // revoked or marked as verification-only) public key from the
        // server.
        if api_keys.is_empty() {
            bail!("an API key is necessary but wasn't provided");
        }

        // We always encrypt and sign internal mail.
        self.encrypt = Some(true);
        self.sign = Some(true);

        // We use a custom scheme for internal messages.
        self.scheme = Some(PM_INTERNAL.to_owned());

        // If user has overridden the MIMEType for a contact, we use that.
        // Otherwise, we take the MIMEType from the composer.
        if !vcard.mime_type.is_empty() {
            self.with_mime_type(&vcard.mime_type);
        }

        self.public_key = Some(pick_sending_key(vcard, api_keys)?);
        Ok(())
    }

    fn set_external_pgp_settings_with_wkd_keys(
        &mut self,
        vcard: &VCardData,
        api_keys: &[PublicKey],
    ) -> Result<()> {
        // We're guaranteed to get at least one valid (i.e. not expired,
        // revoked or marked as verification-only) public key from the
        // server.
        if api_keys.is_empty() {
            bail!("an API key is necessary but wasn't provided");
        }

        self.encrypt = Some(vcard.encrypt_untrusted);
        if vcard.encrypt_untrusted {
            self.sign = Some(true);
        } else if vcard.sign_is_set {
            self.sign = Some(vcard.sign);
        }

        // If the contact has a specific Scheme preference, we set it
        // (otherwise we leave it unset to allow it to be filled in with the
        // default value later).
        if !vcard.scheme.is_empty() {
            self.scheme = Some(vcard.scheme.clone());
        }

        // Because the email is signed, the cryptographic scheme determines
        // the email format. A PGP/INLINE scheme forces to use plain text. A
        // PGP/MIME scheme forces the automatic format.
        match vcard.scheme.as_str() {
            PGP_MIME => self.mime_type = None,
            PGP_INLINE => self.with_mime_type(TEXT_PLAIN),
            _ => {}
        }

        self.public_key = Some(pick_sending_key(vcard, api_keys)?);
        Ok(())
    }

    fn set_external_pgp_settings_without_wkd_keys(&mut self, vcard: &VCardData) -> Result<()> {
        self.encrypt = Some(vcard.encrypt);

        if vcard.sign_is_set {
            self.sign = Some(vcard.sign);
        }

        // Sign must be enabled whenever encrypt is.
        if vcard.encrypt {
            self.sign = Some(true);
        }

        // If the contact has a specific Scheme preference, we set it
        // (otherwise we leave it unset to allow it to be filled in with the
        // default value later).
        if !vcard.scheme.is_empty() {
            self.scheme = Some(vcard.scheme.clone());
        }

        // If we are signing the message, the PGP scheme overrides the
        // MIMEType. Otherwise, we read the MIMEType from the vCard, if set.
        if vcard.sign {
            match vcard.scheme.as_str() {
                PGP_MIME => self.mime_type = None,
                PGP_INLINE => self.with_mime_type(TEXT_PLAIN),
                _ => {}
            }
        } else if !vcard.mime_type.is_empty() {
            self.with_mime_type(&vcard.mime_type);
        }

        if let Some(key) = vcard.keys.first() {
            self.public_key = Some(key.clone());
        }
        Ok(())
    }

    /// Sets the undefined values determined thus far using the (global)
    /// user mail settings (`setEncryptionPreferences`). The public key can
    /// still be undefined as we do not need it if the outgoing email is
    /// not encrypted.
    fn set_encryption_preferences(&mut self, mail_settings: &MailSettings) {
        // For internal addresses or external ones with WKD keys, this flag
        // should always be true. For external ones, an undefined flag
        // defaults to false.
        self.with_encrypt_default(false);

        // For internal addresses or external ones with WKD keys, this flag
        // should always be true. For external ones, an undefined flag
        // defaults to the user mail setting "Sign External messages".
        // Otherwise we keep the defined value unless it conflicts with the
        // encrypt flag (we do not allow to send encrypted but not signed).
        if mail_settings.sign > 0 {
            self.with_sign_default();
        }

        if self.should_encrypt() {
            self.sign = Some(true);
        }

        // If undefined, default to the user mail setting "Default PGP
        // scheme". Otherwise keep the defined value.
        match mail_settings.pgp_scheme {
            EncryptionScheme::PGP_INLINE => self.with_scheme_default(PGP_INLINE),
            EncryptionScheme::PGP_MIME => self.with_scheme_default(PGP_MIME),
            _ => {}
        }

        // Its value is constrained by the sign flag and the PGP scheme:
        //  - Sign flag = true → For a PGP/Inline scheme, the MIME type must
        //    be 'plain/text'. Otherwise we default to the user mail setting
        //    "Composer mode"
        //  - Sign flag = false → If undefined, default to the user mail
        //    setting "Composer mode". Otherwise keep the defined value.
        if self.should_sign() && self.scheme() == PGP_INLINE {
            self.with_mime_type(TEXT_PLAIN);
        } else {
            self.with_mime_type_default(&mail_settings.draft_mime_type);
        }
    }

    /// `setMIMEPreferences`: if the sign flag (that we just determined
    /// above) is true, then the MIME type is determined by the PGP scheme
    /// (also determined above): we should use 'text/plain' for a
    /// PGP/Inline scheme, and 'multipart/mixed' otherwise. Otherwise we use
    /// the MIME type from the encryption preferences, unless the plain text
    /// option has been selecting in the composer, which should enforce
    /// 'text/plain' and override the encryption preference.
    fn set_mime_preferences(&mut self, composer_mime_type: &str) {
        if !self.internal && self.should_sign() {
            if self.scheme() == PGP_INLINE {
                self.with_mime_type(TEXT_PLAIN);
            } else {
                self.with_mime_type(MULTIPART_MIXED);
            }
        } else if composer_mime_type == TEXT_PLAIN {
            self.with_mime_type(TEXT_PLAIN);
        }
    }
}

/// Removes WKD keys that are not importable by the crypto library
/// (`filterUsableWKDKeys`). WKD keys are retrieved from external sources
/// and may have incompatible formats.
fn filter_usable_wkd_keys(raw_api_keys: &[PublicKey]) -> Vec<PublicKey> {
    raw_api_keys
        .iter()
        .filter(|key| RecipientKey::from_armored(&key.public_key).is_ok())
        .cloned()
        .collect()
}

/// Determines which key to use to encrypt outgoing mail
/// (`pickSendingKey`).
///
///  1. If there are pinned keys in the vCard, those should be given
///     preference (assuming the fingerprint matches one of the keys served
///     by the API).
///  2. If there are pinned keys in the vCard but no matching keys were
///     served by the API, we use one of the API keys but first show a
///     modal to the user to ask them to confirm that they trust the API
///     key. (Bridge does not ask either.)
///  3. If there are no pinned keys, then the client should encrypt with
///     the first valid key served by the API (in principle the server
///     already validates the keys and the first one provided should be
///     valid).
fn pick_sending_key(vcard: &VCardData, raw_api_keys: &[PublicKey]) -> Result<RecipientKey> {
    let api_keys = raw_api_keys
        .iter()
        .map(|key| RecipientKey::from_armored(&key.public_key))
        .collect::<Result<Vec<_>, _>>()?;
    let matched = match_fingerprints(&vcard.keys, &api_keys);
    matched
        .into_iter()
        .next()
        .or_else(|| api_keys.into_iter().next())
        .ok_or_else(|| anyhow!("an API key is necessary but wasn't provided"))
}

/// The keys of `b` whose fingerprint is also in `a` (`matchFingerprints`).
fn match_fingerprints(a: &[RecipientKey], b: &[RecipientKey]) -> Vec<RecipientKey> {
    let pinned: std::collections::HashSet<String> =
        a.iter().map(RecipientKey::fingerprint).collect();
    b.iter()
        .filter(|key| pinned.contains(&key.fingerprint()))
        .cloned()
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    //! Cases from Bridge's `smtp_prefs_test.go`, with the contact keys and
    //! API keys it uses replaced by Zmail's test keys.

    use super::*;
    use zmail_proton_api::KeyFlags;

    pub(crate) const KEY_A: &str = include_str!("testdata/public-key-a.asc");
    const KEY_B: &str = include_str!("testdata/public-key-b.asc");

    fn api_key(armored: &str) -> PublicKey {
        PublicKey {
            flags: KeyFlags(3),
            public_key: armored.to_owned(),
        }
    }

    fn settings(sign: i64, pgp_scheme: EncryptionScheme, draft: &str) -> MailSettings {
        MailSettings {
            sign,
            pgp_scheme,
            draft_mime_type: draft.to_owned(),
            ..MailSettings::default()
        }
    }

    struct Expected {
        encrypt: bool,
        signature: SignatureType,
        scheme: EncryptionScheme,
        mime: &'static str,
        key: Option<&'static str>,
    }

    fn check(prefs: SendPreferences, expected: Expected) {
        assert_eq!(prefs.encrypt, expected.encrypt, "encrypt");
        assert_eq!(prefs.signature_type, expected.signature, "signature");
        assert_eq!(prefs.encryption_scheme, expected.scheme, "scheme");
        assert_eq!(prefs.mime_type, expected.mime, "MIME type");
        assert_eq!(
            prefs.pub_key.map(|key| key.fingerprint()),
            expected
                .key
                .map(|key| RecipientKey::from_armored(key).expect("key").fingerprint()),
            "key"
        );
    }

    #[test]
    fn internal_recipients_are_encrypted_and_signed() {
        // "internal, no contact" in Bridge's table.
        let prefs = build_send_prefs(
            &ContactSettings::default(),
            &settings(0, EncryptionScheme(0), "text/html"),
            &[api_key(KEY_A)],
            "text/html",
            true,
        )
        .expect("prefs");
        check(
            prefs,
            Expected {
                encrypt: true,
                signature: SignatureType::DETACHED,
                scheme: EncryptionScheme::INTERNAL,
                mime: "text/html",
                key: Some(KEY_A),
            },
        );

        // A plain-text composer forces plain text; a contact pinning the
        // second key picks it.
        let pinned = ContactSettings {
            keys: vec![RecipientKey::from_armored(KEY_B).expect("key")],
            ..ContactSettings::default()
        };
        let prefs = build_send_prefs(
            &pinned,
            &settings(0, EncryptionScheme(0), "text/html"),
            &[api_key(KEY_A), api_key(KEY_B)],
            "text/plain",
            true,
        )
        .expect("prefs");
        check(
            prefs,
            Expected {
                encrypt: true,
                signature: SignatureType::DETACHED,
                scheme: EncryptionScheme::INTERNAL,
                mime: "text/plain",
                key: Some(KEY_B),
            },
        );

        assert!(
            build_send_prefs(
                &ContactSettings::default(),
                &MailSettings::default(),
                &[],
                "text/html",
                true
            )
            .is_err(),
            "internal recipients need a key"
        );
    }

    #[test]
    fn external_recipients_follow_the_sign_setting() {
        // "external, no contact, no sign": clear, in the composer's format.
        let prefs = build_send_prefs(
            &ContactSettings::default(),
            &settings(0, EncryptionScheme(0), "text/html"),
            &[],
            "text/html",
            false,
        )
        .expect("prefs");
        check(
            prefs,
            Expected {
                encrypt: false,
                signature: SignatureType::NONE,
                scheme: EncryptionScheme::CLEAR,
                mime: "text/html",
                key: None,
            },
        );

        // "external, no contact, sign" with the default scheme: clear MIME.
        let prefs = build_send_prefs(
            &ContactSettings::default(),
            &settings(1, EncryptionScheme::PGP_MIME, "text/html"),
            &[],
            "text/plain",
            false,
        )
        .expect("prefs");
        check(
            prefs,
            Expected {
                encrypt: false,
                signature: SignatureType::DETACHED,
                scheme: EncryptionScheme::CLEAR_MIME,
                mime: "multipart/mixed",
                key: None,
            },
        );

        // Signing with PGP/Inline: clear, plain text.
        let prefs = build_send_prefs(
            &ContactSettings::default(),
            &settings(1, EncryptionScheme::PGP_INLINE, "text/html"),
            &[],
            "text/html",
            false,
        )
        .expect("prefs");
        check(
            prefs,
            Expected {
                encrypt: false,
                signature: SignatureType::DETACHED,
                scheme: EncryptionScheme::CLEAR,
                mime: "text/plain",
                key: None,
            },
        );
    }

    #[test]
    fn external_recipients_with_keys() {
        // WKD keys: encrypted (untrusted keys are used unless the contact
        // says not to), PGP/MIME by default.
        let prefs = build_send_prefs(
            &ContactSettings::default(),
            &settings(0, EncryptionScheme(0), "text/html"),
            &[api_key("not a key"), api_key(KEY_A)],
            "text/html",
            false,
        )
        .expect("prefs");
        check(
            prefs,
            Expected {
                encrypt: true,
                signature: SignatureType::DETACHED,
                scheme: EncryptionScheme::PGP_MIME,
                mime: "multipart/mixed",
                key: Some(KEY_A),
            },
        );

        // A contact with a pinned key, encrypt on and PGP/Inline.
        let contact = ContactSettings {
            encrypt: Some(true),
            scheme: Some(EncryptionScheme::PGP_INLINE),
            keys: vec![RecipientKey::from_armored(KEY_B).expect("key")],
            ..ContactSettings::default()
        };
        let prefs = build_send_prefs(
            &contact,
            &settings(0, EncryptionScheme(0), "text/html"),
            &[],
            "text/html",
            false,
        )
        .expect("prefs");
        check(
            prefs,
            Expected {
                encrypt: true,
                signature: SignatureType::DETACHED,
                scheme: EncryptionScheme::PGP_INLINE,
                mime: "text/plain",
                key: Some(KEY_B),
            },
        );

        let bad = ContactSettings {
            scheme: Some(EncryptionScheme::CLEAR),
            ..ContactSettings::default()
        };
        assert!(build_send_prefs(&bad, &MailSettings::default(), &[], "text/html", false).is_err());
    }
}
