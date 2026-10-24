// Ported from proton-bridge `internal/services/smtp/smtp_prefs.go` at
// v3.27.1 (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. Contact keys
// arrive parsed, and an unknown contact scheme is an error where Bridge
// panics. Bridge's comments are kept with the rules they explain.
// Modified by UnicornOps on 2026-10-24: `pickSendingKey` only considers
// served keys the API flags as active that can encrypt now, and when the
// contact card pins keys and none is served it fails with
// `PinnedKeyChanged` unless the user trusted the served key for this
// recipient, where Bridge sends to the first served key (ADR 0004). For
// WKD keys the key is only picked when the mail is encrypted.

//! Deciding how to send to each recipient: Bridge's send preferences.

use anyhow::{Context as _, Result, bail};
use zmail_core::key_change::{KeyTrust, PinnedKeyChanged};
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

/// The recipient a send preference is for, and the served keys the user
/// trusted although the contact card pins others.
pub(crate) struct Recipient<'a> {
    /// The recipient's address.
    pub(crate) address: &'a str,
    /// The account's key trust decisions.
    pub(crate) trusted: &'a [KeyTrust],
}

/// Works out how to send to one recipient (`buildSendPrefs`).
pub(crate) fn build_send_prefs(
    recipient: &Recipient<'_>,
    contact_settings: &ContactSettings,
    mail_settings: &MailSettings,
    pub_keys: &[PublicKey],
    mime_type: &str,
    is_internal: bool,
) -> Result<SendPreferences> {
    let mut builder = SendPrefsBuilder::default();
    // `context` rather than Bridge's flattened message, so a
    // `PinnedKeyChanged` stays findable in the error chain.
    builder
        .set_pgp_settings(
            recipient,
            new_contact_settings(contact_settings)?,
            pub_keys,
            is_internal,
        )
        .context("failed to set PGP settings")?;
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
        recipient: &Recipient<'_>,
        vcard: VCardData,
        api_keys: &[PublicKey],
        is_internal: bool,
    ) -> Result<()> {
        // Sending internal. We are guaranteed to always receive API keys.
        if is_internal {
            self.internal = true;
            return self.set_internal_pgp_settings(recipient, &vcard, api_keys);
        }

        // Sending external but with keys supplied by WKD. Treated pretty
        // much the same as internal.
        if !api_keys.is_empty() {
            let usable = filter_usable_wkd_keys(api_keys);
            if !usable.is_empty() {
                return self.set_external_pgp_settings_with_wkd_keys(recipient, &vcard, &usable);
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
        recipient: &Recipient<'_>,
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

        self.public_key = Some(pick_sending_key(recipient, vcard, api_keys)?);
        Ok(())
    }

    fn set_external_pgp_settings_with_wkd_keys(
        &mut self,
        recipient: &Recipient<'_>,
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

        // The key is only used to encrypt, and picking it can stop the
        // send (a pinned key that changed), so Zmail only picks it then.
        if vcard.encrypt_untrusted {
            self.public_key = Some(pick_sending_key(recipient, vcard, api_keys)?);
        }
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
///     key. (Bridge does not ask and sends to the first API key; Zmail
///     fails with [`PinnedKeyChanged`] until the user trusts that key for
///     this recipient, and then uses it.)
///  3. If there are no pinned keys, then the client should encrypt with
///     the first valid key served by the API (in principle the server
///     already validates the keys and the first one provided should be
///     valid).
///
/// Zmail only takes the served keys the API flags as active
/// (go-proton-api's `KeyStateActive`: "the key is still in use (i.e. not
/// obsolete, we can encrypt messages to it)") and that have a key mail
/// can be encrypted to now, rather than trusting the server to have
/// filtered them.
fn pick_sending_key(
    recipient: &Recipient<'_>,
    vcard: &VCardData,
    raw_api_keys: &[PublicKey],
) -> Result<RecipientKey> {
    let mut api_keys = Vec::new();
    for key in raw_api_keys {
        let parsed = RecipientKey::from_armored(&key.public_key)?;
        if key.flags.is_active() && parsed.can_encrypt() {
            api_keys.push(parsed);
        }
    }
    if api_keys.is_empty() {
        bail!(
            "no key served for {} can be encrypted to",
            recipient.address
        );
    }
    if vcard.keys.is_empty() {
        return Ok(api_keys.swap_remove(0));
    }
    if let Some(key) = match_fingerprints(&vcard.keys, &api_keys)
        .into_iter()
        .next()
    {
        return Ok(key);
    }
    let trusted = api_keys.iter().find(|key| {
        recipient
            .trusted
            .iter()
            .any(|trust| trust.covers(recipient.address, &key.fingerprint()))
    });
    match trusted {
        Some(key) => Ok(key.clone()),
        None => Err(PinnedKeyChanged::new(
            recipient.address,
            vcard.keys.iter().map(RecipientKey::fingerprint),
            api_keys.iter().map(RecipientKey::fingerprint),
        )
        .into()),
    }
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

    /// A recipient with no key trust decisions.
    const BOB: Recipient<'static> = Recipient {
        address: "bob@example.org",
        trusted: &[],
    };

    pub(crate) const KEY_A: &str = include_str!("testdata/public-key-a.asc");
    const KEY_B: &str = include_str!("testdata/public-key-b.asc");
    const KEY_C: &str = include_str!("testdata/public-key-c.asc");
    /// A valid key with nothing to encrypt to: an EdDSA primary key alone.
    const KEY_SIGN_ONLY: &str = include_str!("testdata/public-key-sign-only.asc");

    fn api_key(armored: &str) -> PublicKey {
        flagged(armored, KeyFlags::TRUSTED | KeyFlags::ACTIVE)
    }

    fn flagged(armored: &str, flags: u32) -> PublicKey {
        PublicKey {
            flags: KeyFlags(flags),
            public_key: armored.to_owned(),
        }
    }

    fn fingerprint(armored: &str) -> String {
        RecipientKey::from_armored(armored)
            .expect("key")
            .fingerprint()
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
            &BOB,
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
            &BOB,
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
                &BOB,
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
            &BOB,
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
            &BOB,
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
            &BOB,
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
            &BOB,
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
            &BOB,
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
        assert!(
            build_send_prefs(
                &BOB,
                &bad,
                &MailSettings::default(),
                &[],
                "text/html",
                false
            )
            .is_err()
        );
    }

    /// Bridge trusts the server to serve only keys that can be encrypted
    /// to; Zmail checks the API flags and the keys themselves.
    #[test]
    fn only_active_keys_that_can_encrypt_are_encrypted_to() {
        let send = |keys: &[PublicKey], internal: bool| {
            build_send_prefs(
                &BOB,
                &ContactSettings::default(),
                &settings(0, EncryptionScheme(0), "text/html"),
                keys,
                "text/html",
                internal,
            )
        };
        let skipped = [
            // Obsolete: not compromised, but no longer encrypted to.
            flagged(KEY_A, KeyFlags::TRUSTED),
            // No flags at all.
            flagged(KEY_C, 0),
            // Flagged active, but it has no key to encrypt to.
            api_key(KEY_SIGN_ONLY),
        ];
        for internal in [true, false] {
            let mut keys = skipped.to_vec();
            keys.push(api_key(KEY_B));
            let prefs = send(&keys, internal).expect("prefs");
            assert!(prefs.encrypt);
            assert_eq!(
                prefs.pub_key.map(|key| key.fingerprint()),
                Some(fingerprint(KEY_B)),
                "internal: {internal}"
            );
            for key in &skipped {
                assert!(send(std::slice::from_ref(key), internal).is_err());
            }
        }

        // Unencrypted mail to an external contact needs no usable key.
        let clear = ContactSettings {
            encrypt_untrusted: Some(false),
            ..ContactSettings::default()
        };
        let prefs = build_send_prefs(
            &BOB,
            &clear,
            &settings(0, EncryptionScheme(0), "text/html"),
            &skipped,
            "text/html",
            false,
        )
        .expect("prefs");
        assert!(!prefs.encrypt && prefs.pub_key.is_none());
    }

    fn changed(error: &anyhow::Error) -> &PinnedKeyChanged {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<PinnedKeyChanged>())
            .unwrap_or_else(|| panic!("not a key change: {error:#}"))
    }

    #[test]
    fn a_pin_that_matches_no_served_key_stops_the_send_until_trusted() {
        let pinned = ContactSettings {
            keys: vec![RecipientKey::from_armored(KEY_B).expect("key")],
            ..ContactSettings::default()
        };
        let send = |recipient: &Recipient<'_>, keys: &[PublicKey], internal: bool| {
            build_send_prefs(
                recipient,
                &pinned,
                &settings(0, EncryptionScheme(0), "text/html"),
                keys,
                "text/html",
                internal,
            )
        };

        for internal in [true, false] {
            let error = send(&BOB, &[api_key(KEY_A)], internal).expect_err("aborts");
            let change = changed(&error);
            assert_eq!(change.recipient, "bob@example.org");
            assert_eq!(
                change.pinned_fingerprints,
                [fingerprint(KEY_B).to_ascii_uppercase()]
            );
            assert_eq!(
                change.served_fingerprints,
                [fingerprint(KEY_A).to_ascii_uppercase()]
            );
        }

        // Trusting exactly that key for exactly that recipient sends to it.
        let trusted = [KeyTrust {
            recipient: "bob@example.org".to_owned(),
            fingerprint: fingerprint(KEY_A).to_ascii_uppercase(),
        }];
        let bob = Recipient {
            address: "Bob@example.org",
            trusted: &trusted,
        };
        let prefs = send(&bob, &[api_key(KEY_A)], true).expect("trusted");
        assert_eq!(
            prefs.pub_key.map(|key| key.fingerprint()),
            Some(fingerprint(KEY_A))
        );

        // Another served key still stops the send, and so does the same
        // key for another recipient.
        let error = send(&bob, &[api_key(KEY_C)], true).expect_err("aborts");
        assert_eq!(
            changed(&error).served_fingerprints,
            [fingerprint(KEY_C).to_ascii_uppercase()]
        );
        let alice = Recipient {
            address: "alice@example.org",
            trusted: &trusted,
        };
        assert!(send(&alice, &[api_key(KEY_A)], true).is_err());

        // A served pinned key is used, trusted or not.
        let prefs = send(&bob, &[api_key(KEY_A), api_key(KEY_B)], true).expect("pinned");
        assert_eq!(
            prefs.pub_key.map(|key| key.fingerprint()),
            Some(fingerprint(KEY_B))
        );
        // A pinned key the API marks obsolete no longer counts as served.
        let error = send(
            &BOB,
            &[flagged(KEY_B, KeyFlags::TRUSTED), api_key(KEY_A)],
            true,
        )
        .expect_err("aborts");
        assert_eq!(
            changed(&error).served_fingerprints,
            [fingerprint(KEY_A).to_ascii_uppercase()]
        );
    }
}
