// Ported from go-proton-api `contact.go` (`GetContact`,
// `GetAllContactEmails`, `GetAllContactEmailsPaged`,
// `getContactEmailsImpl`), `contact_types.go` (`ContactSettings`,
// `Contact`, `ContactMetadata`, `ContactCards`, `ContactEmail`,
// `GetSettings`) and `contact_card.go` (`Cards`, `Card`, `CardType`,
// `CardGroup`, the `X-PM-*` field names, `decode`, `getGroup`) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust, reading cards
// with Zmail's own vCard decoder (`vcard.rs`) instead of go-vcard. Only
// reading is ported. A `KEY` value without a comma is an error where Go
// would panic.

//! Contacts, and what a contact card says about sending to an address.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Deserialize;

use crate::{
    client::Client, crypto::RecipientKey, keyring::KeyRing, manager::Request,
    message::null_as_empty, package::EncryptionScheme, paging::MAX_PAGE_SIZE, response::Error,
    vcard,
};

/// `X-PM-SCHEME`.
pub const FIELD_PM_SCHEME: &str = "X-PM-SCHEME";
/// `X-PM-SIGN`.
pub const FIELD_PM_SIGN: &str = "X-PM-SIGN";
/// `X-PM-ENCRYPT`.
pub const FIELD_PM_ENCRYPT: &str = "X-PM-ENCRYPT";
/// `X-PM-ENCRYPT-UNTRUSTED`.
pub const FIELD_PM_ENCRYPT_UNTRUSTED: &str = "X-PM-ENCRYPT-UNTRUSTED";
/// `X-PM-MIMETYPE`.
pub const FIELD_PM_MIME_TYPE: &str = "X-PM-MIMETYPE";

/// What the user's contact card says about sending to an address
/// (`ContactSettings`). Every field is optional; the default is "no
/// contact".
#[derive(Debug, Clone, Default)]
pub struct ContactSettings {
    /// Preferred body format.
    pub mime_type: Option<String>,
    /// Preferred PGP scheme (PGP/MIME or PGP/Inline).
    pub scheme: Option<EncryptionScheme>,
    /// Whether to sign.
    pub sign: Option<bool>,
    /// Whether to encrypt.
    pub encrypt: Option<bool>,
    /// Whether to encrypt to keys found through WKD.
    pub encrypt_untrusted: Option<bool>,
    /// Pinned public keys.
    pub keys: Vec<RecipientKey>,
}

/// A card's protection, a bit set (`CardType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct CardType(pub i64);

impl CardType {
    /// In the clear (`CardTypeClear`).
    pub const CLEAR: Self = Self(0);
    /// Encrypted to the user keys (`CardTypeEncrypted`).
    pub const ENCRYPTED: Self = Self(1);
    /// Signed by the user keys (`CardTypeSigned`).
    pub const SIGNED: Self = Self(2);
}

/// One vCard of a contact (`Card`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Card {
    /// How the card is protected.
    #[serde(rename = "Type", default)]
    pub kind: CardType,
    /// The vCard, or an armored message holding it.
    #[serde(rename = "Data", default)]
    pub data: String,
    /// The armored detached signature, for signed cards.
    #[serde(rename = "Signature", default)]
    pub signature: Option<String>,
}

impl Card {
    /// Decrypts and verifies the card as its type says, and parses it
    /// (`decode`).
    ///
    /// Nothing in the card is read before its signature is checked, and
    /// oversized cards are refused before any decryption or verification.
    fn decode(&self, kr: &KeyRing) -> Result<vcard::Card, Error> {
        // Armor adds about a third; the vCard reader caps the plaintext.
        if self.data.len() > 2 * vcard::MAX_CARD_BYTES {
            return Err(Error::Key("the contact card is too large".to_owned()));
        }
        let mut data = self.data.clone();
        if self.kind.0 & CardType::ENCRYPTED.0 != 0 {
            let plain = kr.decrypt_armored(&data)?;
            data = String::from_utf8_lossy(&plain).into_owned();
        }
        if self.kind.0 & CardType::SIGNED.0 != 0 {
            let signature = self
                .signature
                .as_deref()
                .ok_or_else(|| Error::Key("the signed card has no signature".to_owned()))?;
            // `NewPlainMessageFromString` canonicalises line endings.
            let canonical = data.replace("\r\n", "\n").replace('\n', "\r\n");
            kr.verify_detached_armored(canonical.as_bytes(), signature)?;
        }
        vcard::Card::decode(&data).map_err(Error::Key)
    }
}

/// A contact (`Contact`: `ContactMetadata` and `ContactCards`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Contact {
    /// Contact ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// Display name.
    #[serde(rename = "Name", default)]
    pub name: String,
    /// The contact's addresses.
    #[serde(rename = "ContactEmails", default, deserialize_with = "null_as_empty")]
    pub contact_emails: Vec<ContactEmail>,
    /// The vCards.
    #[serde(rename = "Cards", default, deserialize_with = "null_as_empty")]
    pub cards: Vec<Card>,
}

/// One address of a contact (`ContactEmail`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ContactEmail {
    /// ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// Display name.
    #[serde(rename = "Name", default)]
    pub name: String,
    /// The address.
    #[serde(rename = "Email", default)]
    pub email: String,
    /// The contact it belongs to.
    #[serde(rename = "ContactID", default)]
    pub contact_id: String,
}

impl Contact {
    /// Reads the send settings for `email` from the contact's card of
    /// `card_type` (`GetSettings`). No card, an address the card does not
    /// list, or a card that does not decrypt or verify, gives no settings.
    pub fn get_settings(
        &self,
        kr: &KeyRing,
        email: &str,
        card_type: CardType,
    ) -> Result<ContactSettings, Error> {
        let Some(card) = self.cards.iter().find(|card| card.kind == card_type) else {
            return Ok(ContactSettings::default());
        };
        // `GetGroup`: a decode failure or a missing address is "no
        // settings", as upstream.
        let Ok(decoded) = card.decode(kr) else {
            return Ok(ContactSettings::default());
        };
        let Some(group) = decoded
            .get("EMAIL")
            .find(|field| field.value == email)
            .map(|field| field.group.clone())
        else {
            return Ok(ContactSettings::default());
        };
        let values = |name: &str| -> Vec<String> {
            decoded
                .get(name)
                .filter(|field| field.group == group)
                .map(|field| field.value.clone())
                .collect()
        };

        let mut settings = ContactSettings::default();
        if let Some(scheme) = values(FIELD_PM_SCHEME).first() {
            match scheme.as_str() {
                "pgp-inline" => settings.scheme = Some(EncryptionScheme::PGP_INLINE),
                "pgp-mime" => settings.scheme = Some(EncryptionScheme::PGP_MIME),
                _ => {}
            }
        }
        if let Some(mime_type) = values(FIELD_PM_MIME_TYPE).first() {
            settings.mime_type = Some(mime_type.clone());
        }
        if let Some(sign) = values(FIELD_PM_SIGN).first() {
            settings.sign = Some(parse_bool(sign)?);
        }
        if let Some(encrypt) = values(FIELD_PM_ENCRYPT).first() {
            settings.encrypt = Some(parse_bool(encrypt)?);
        }
        if let Some(untrusted) = values(FIELD_PM_ENCRYPT_UNTRUSTED).first() {
            settings.encrypt_untrusted = Some(parse_bool(untrusted)?);
        }
        for key in values("KEY") {
            let (_, data) = key
                .split_once(',')
                .ok_or_else(|| Error::Key(format!("a contact key without data: {key:?}")))?;
            let bytes = BASE64
                .decode(data.trim())
                .map_err(|error| Error::Key(format!("a contact key is not base64: {error}")))?;
            settings.keys.push(RecipientKey::from_bytes(&bytes)?);
        }
        Ok(settings)
    }
}

/// Go's `strconv.ParseBool`.
fn parse_bool(value: &str) -> Result<bool, Error> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        other => Err(Error::Key(format!(
            "a contact setting is not a boolean: {other:?}"
        ))),
    }
}

impl Client {
    /// A contact with its cards (`GetContact`).
    pub async fn get_contact(&self, contact_id: &str) -> Result<Contact, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "Contact")]
            contact: Contact,
        }

        let res: Res = self
            .json(&Request::get(format!("/contacts/v4/{contact_id}")))
            .await?;
        Ok(res.contact)
    }

    /// Every contact address equal to `email` (`GetAllContactEmails`).
    pub async fn get_all_contact_emails(&self, email: &str) -> Result<Vec<ContactEmail>, Error> {
        let page_size = MAX_PAGE_SIZE;
        let (total, mut emails) = self.contact_emails_page(email, 0, page_size).await?;
        if total <= page_size {
            return Ok(emails);
        }
        for page in 1..total / page_size + 1 {
            let (_, batch) = self.contact_emails_page(email, page, page_size).await?;
            emails.extend(batch);
        }
        Ok(emails)
    }

    async fn contact_emails_page(
        &self,
        email: &str,
        page: usize,
        page_size: usize,
    ) -> Result<(usize, Vec<ContactEmail>), Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "ContactEmails", default, deserialize_with = "null_as_empty")]
            contact_emails: Vec<ContactEmail>,
            #[serde(rename = "Total", default)]
            total: usize,
        }

        let res: Res = self
            .json(
                &Request::get("/contacts/v4/emails")
                    .query("Page", page)
                    .query("PageSize", page_size)
                    .query("Email", email),
            )
            .await?;
        Ok((res.total, res.contact_emails))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::{
        Keys,
        tests::{USER_KEY_PASSWORD, key},
    };

    const CARD: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Bob\r\n\
        item1.EMAIL;PREF=1:bob@example.com\r\nitem1.X-PM-ENCRYPT:true\r\n\
        item1.X-PM-SIGN:true\r\nitem1.X-PM-SCHEME:pgp-inline\r\n\
        item1.X-PM-MIMETYPE:text/plain\r\nitem2.EMAIL:other@example.com\r\n\
        item2.X-PM-SIGN:false\r\nEND:VCARD\r\n";

    fn keyring() -> KeyRing {
        Keys(vec![key("k", USER_KEY_PASSWORD)])
            .unlock(b"password", None)
            .expect("unlock")
    }

    fn signed(card: &str, kr: &KeyRing) -> Card {
        let key = kr.first().expect("key").secret_key();
        let signature = pgp::composed::DetachedSignature::sign_binary_data(
            rand::thread_rng(),
            &key.primary_key,
            &pgp::types::Password::empty(),
            pgp::crypto::hash::HashAlgorithm::Sha256,
            card.as_bytes(),
        )
        .expect("sign")
        .to_armored_string(pgp::composed::ArmorOptions::default())
        .expect("armor");
        Card {
            kind: CardType::SIGNED,
            data: card.to_owned(),
            signature: Some(signature),
        }
    }

    #[test]
    fn reads_the_settings_of_a_signed_card_group() {
        let kr = keyring();
        let contact = Contact {
            cards: vec![signed(CARD, &kr)],
            ..Contact::default()
        };
        let settings = contact
            .get_settings(&kr, "bob@example.com", CardType::SIGNED)
            .expect("settings");
        assert_eq!(settings.encrypt, Some(true));
        assert_eq!(settings.sign, Some(true));
        assert_eq!(settings.scheme, Some(EncryptionScheme::PGP_INLINE));
        assert_eq!(settings.mime_type.as_deref(), Some("text/plain"));
        assert_eq!(settings.encrypt_untrusted, None);

        // Another group of the same card.
        let other = contact
            .get_settings(&kr, "other@example.com", CardType::SIGNED)
            .expect("settings");
        assert_eq!(other.sign, Some(false));
        assert_eq!(other.encrypt, None);

        // An address the card does not list, or a tampered card: nothing.
        let none = contact
            .get_settings(&kr, "nobody@example.com", CardType::SIGNED)
            .expect("settings");
        assert!(none.sign.is_none());
        let mut tampered = contact.clone();
        tampered.cards[0].data = CARD.replace("X-PM-ENCRYPT:true", "X-PM-ENCRYPT:false");
        let settings = tampered
            .get_settings(&kr, "bob@example.com", CardType::SIGNED)
            .expect("settings");
        assert!(
            settings.encrypt.is_none(),
            "a bad signature gives no settings"
        );
    }

    #[test]
    fn reads_pinned_keys_and_rejects_bad_booleans() {
        let kr = keyring();
        let public = kr.first().expect("key").recipient_key();
        let bytes = public.to_bytes().expect("bytes");
        let card = format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nitem1.EMAIL:bob@example.com\r\n\
             item1.KEY;PREF=1:data:application/pgp-keys;base64,{}\r\nEND:VCARD\r\n",
            BASE64.encode(bytes)
        );
        let contact = Contact {
            cards: vec![signed(&card, &kr)],
            ..Contact::default()
        };
        let settings = contact
            .get_settings(&kr, "bob@example.com", CardType::SIGNED)
            .expect("settings");
        assert_eq!(settings.keys.len(), 1);
        assert_eq!(settings.keys[0].fingerprint(), public.fingerprint());

        let bad = CARD.replace("X-PM-SIGN:true", "X-PM-SIGN:maybe");
        let contact = Contact {
            cards: vec![signed(&bad, &kr)],
            ..Contact::default()
        };
        assert!(
            contact
                .get_settings(&kr, "bob@example.com", CardType::SIGNED)
                .is_err()
        );
        assert_eq!(parse_bool("True").ok(), Some(true));
    }

    #[test]
    fn hostile_cards_give_errors_or_nothing_never_a_panic() {
        let kr = keyring();
        let settings = |card: &str| {
            Contact {
                cards: vec![signed(card, &kr)],
                ..Contact::default()
            }
            .get_settings(&kr, "bob@example.com", CardType::SIGNED)
        };
        let card = |lines: &str| {
            format!(
                "BEGIN:VCARD\r\nVERSION:4.0\r\nitem1.EMAIL:bob@example.com\r\n{lines}END:VCARD\r\n"
            )
        };

        // Duplicate X-PM-* fields: the first one counts, as upstream.
        let duplicate = settings(&card(
            "item1.X-PM-ENCRYPT:false\r\nitem1.X-PM-ENCRYPT:true\r\n\
             item1.X-PM-SCHEME:pgp-mime\r\nitem1.X-PM-SCHEME:pgp-inline\r\n",
        ))
        .expect("settings");
        assert_eq!(duplicate.encrypt, Some(false));
        assert_eq!(duplicate.scheme, Some(EncryptionScheme::PGP_MIME));

        // Fields of another group, and unknown fields, are not read.
        let other = settings(&card(
            "item2.X-PM-ENCRYPT:true\r\nitem1.X-PM-UNKNOWN:whatever\r\n\
             item1.X-PM-SCHEME:something-else\r\n",
        ))
        .expect("settings");
        assert_eq!(other.encrypt, None);
        assert_eq!(other.scheme, None);

        // Bad keys are errors: not base64, not a key, no data part.
        for key in [
            "data:application/pgp-keys;base64,!!!not base64!!!",
            "data:application/pgp-keys;base64,QUJDREVG",
            "no-comma-at-all",
        ] {
            assert!(
                settings(&card(&format!("item1.KEY:{key}\r\n"))).is_err(),
                "accepted key {key:?}"
            );
        }

        // A malformed, unterminated or oversized card gives no settings.
        for text in [
            "BEGIN:VCARD\r\nitem1.EMAIL:bob@example.com\r\nitem1.X-PM-SIGN:true\r\n".to_owned(),
            card(&format!(
                "NOTE:{}\r\n",
                "a".repeat(vcard::MAX_LINE_BYTES + 1)
            )),
            card("not a property line\r\nitem1.X-PM-SIGN:true\r\n"),
        ] {
            let parsed = settings(&text).expect("no error, no settings");
            assert!(parsed.sign.is_none());
        }
        let oversized = Contact {
            cards: vec![Card {
                kind: CardType::SIGNED,
                data: "a".repeat(2 * vcard::MAX_CARD_BYTES + 1),
                signature: Some(String::new()),
            }],
            ..Contact::default()
        };
        assert!(
            oversized
                .get_settings(&kr, "bob@example.com", CardType::SIGNED)
                .expect("settings")
                .sign
                .is_none()
        );

        // A signed card without its signature is not trusted.
        let unsigned = Contact {
            cards: vec![Card {
                signature: None,
                ..signed(&card("item1.X-PM-SIGN:true\r\n"), &kr)
            }],
            ..Contact::default()
        };
        assert!(
            unsigned
                .get_settings(&kr, "bob@example.com", CardType::SIGNED)
                .expect("settings")
                .sign
                .is_none()
        );
    }
}
