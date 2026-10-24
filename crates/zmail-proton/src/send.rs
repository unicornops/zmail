// Ported from proton-bridge `internal/services/smtp/smtp.go`
// (`smtpSendMail`, `sendWithKey`, `createDraft`, `createAttachments`,
// `getRecipients`, `getContactSettings`, `constructEmail`),
// `smtp_packages.go` (`createSendReq`,
// `recipients`), `errors.go` (the API error codes) and
// `internal/usertypes/types.go` (`SanitizeEmail`) at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust for Zmail's
// outbox, which hands over a composed plain-text message rather than MIME
// from an SMTP client, so there is no MIME parsing: the plain body is both
// the "rich" and the plain body, as Bridge's parser makes them for a
// text/plain message. Zmail's outbox carries no In-Reply-To or References
// yet, so there is no parent message and the draft action is Forward, as
// in Bridge for a new message. The send recorder (duplicate detection) is
// replaced by the outbox claiming each job once. Attachments are uploaded
// and recipients looked up one at a time rather than in parallel, and the
// attachments' session keys are kept from encryption rather than decrypted
// from the upload response, so a send decrypts nothing the server supplies
// (ADR 0004). If sending fails before the draft is sent, the
// draft is deleted, so outbox retries do not leave drafts behind; Bridge
// leaves it. The MIME body for PGP/MIME and clear MIME recipients is
// written from the composed message (`outgoing.rs`) rather than taken from
// an SMTP client.
// Modified by UnicornOps on 2026-10-24: takes the account's key trust
// decisions for recipients whose contact card pins other keys (see
// `send_prefs.rs`, ADR 0004).

//! Sending a message through Proton: Bridge's SMTP service.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use zmail_core::{OutgoingMessage, key_change::KeyTrust};
use zmail_proton_api::{
    ATTACHMENT_DISPOSITION, Address, AddressStatus, CardType, Client, ContactSettings,
    CreateAttachmentReq, CreateDraftAction, CreateDraftReq, DraftTemplate, EmailAddress,
    EncryptionScheme, Error, KeyRing, MULTIPART_MIXED, MailSettings, Message, RecipientType,
    SendDraftReq, SendPreferences, SessionKey, TEXT_HTML, TEXT_PLAIN, UnlockedKey,
};

use crate::{
    ProtonSession,
    outgoing::{MimeSource, OutgoingFile, build_mime_body},
    send_prefs::{Recipient, build_send_prefs},
};

/// `errCodeAddressDoesNotExist`.
const ADDRESS_DOES_NOT_EXIST: i64 = 33102;
/// `errCodeValidationFailed`.
const VALIDATION_FAILED: i64 = 2001;
/// `errCodeMessageTooLarge`.
const MESSAGE_TOO_LARGE: i64 = 2024;
/// `errCodeInvalidListOfRecipients`.
const INVALID_LIST_OF_RECIPIENTS: i64 = 2002;

/// Sends a composed message from one of the account's addresses
/// (`smtpSendMail`). `trusted` holds the served keys the user trusted for
/// recipients whose contact card pins other keys.
pub(crate) async fn send_message(
    session: &ProtonSession,
    message: &OutgoingMessage,
    trusted: &[KeyTrust],
) -> Result<()> {
    let client = session.client();
    let mut addresses = client.get_addresses().await?;
    addresses.sort_by_key(|address| address.order);
    let emails: Vec<String> = addresses
        .iter()
        .map(|address| address.email.clone())
        .collect();
    let to: Vec<String> = message
        .to
        .iter()
        .chain(&message.cc)
        .chain(&message.bcc)
        .cloned()
        .collect();
    if to.is_empty() {
        bail!("sending through Proton needs at least one recipient");
    }

    let from = message.from_email.as_str();
    let from_addr = get_addr(&addresses, from)
        .ok_or_else(|| anyhow!("{from} is not an address of this Proton account"))?;
    if !from_addr.send || from_addr.status != AddressStatus::ENABLED {
        bail!("cannot send emails from address: {}", from_addr.email);
    }

    // Load the user's mail settings.
    let settings = client
        .get_mail_settings()
        .await
        .context("failed to get mail settings")?;

    let keys = session.unlocked_keys().await?;
    let addr_kr = keys
        .addresses
        .get(&from_addr.id)
        .with_context(|| format!("the keys of {} are not unlocked", from_addr.email))?;
    // Use the first key for encrypting the message.
    let addr_key = addr_kr
        .first()
        .context("failed to get first key: the address has no keys")?;

    let mut attachments: Vec<OutgoingFile> = message
        .attachments
        .iter()
        .map(|attachment| OutgoingFile {
            name: attachment.filename.clone(),
            mime_type: attachment.content_type.clone(),
            data: attachment.bytes.clone(),
        })
        .collect();

    // If we have to attach the public key, do it now.
    if settings.attach_public_key {
        let fingerprint = addr_key.fingerprint_hex().to_uppercase();
        attachments.push(OutgoingFile {
            name: format!(
                "publickey - {} - 0x{}.asc",
                addr_key.identity_name(),
                &fingerprint[..fingerprint.len().min(8)]
            ),
            mime_type: "application/pgp-keys".to_owned(),
            data: addr_key.armored_public_key()?.into_bytes(),
        });
    }

    send_with_key(
        client,
        &SendContext {
            settings: &settings,
            user_kr: &keys.user,
            addr_key,
            emails: &emails,
            from,
            to: &to,
            trusted,
        },
        message,
        &attachments,
    )
    .await
    .context("failed to send the message through Proton")
}

/// Finds the account address a message is sent from (`GetAddr`).
fn get_addr<'a>(addresses: &'a [Address], email: &str) -> Option<&'a Address> {
    let email = sanitize_email(email);
    addresses
        .iter()
        .find(|address| address.email.eq_ignore_ascii_case(&email))
}

/// Drops a `+suffix` from the local part (`SanitizeEmail`).
fn sanitize_email(email: &str) -> String {
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 {
        return email.to_owned();
    }
    let local = parts[0].split('+').next().unwrap_or_default();
    format!("{local}@{}", parts[1])
}

/// What `sendWithKey` works with besides the message.
struct SendContext<'a> {
    /// The user's mail settings.
    settings: &'a MailSettings,
    /// The user keys, which verify contact cards.
    user_kr: &'a KeyRing,
    /// The sending address's first key.
    addr_key: &'a UnlockedKey,
    /// The account's addresses.
    emails: &'a [String],
    /// The sending address.
    from: &'a str,
    /// Every recipient (the SMTP envelope).
    to: &'a [String],
    /// The served keys the user trusted despite a contact card's pins.
    trusted: &'a [KeyTrust],
}

/// Sends the message with the given address key (`sendWithKey`).
async fn send_with_key(
    client: &Client,
    context: &SendContext<'_>,
    message: &OutgoingMessage,
    attachments: &[OutgoingFile],
) -> Result<()> {
    let SendContext {
        settings,
        user_kr,
        addr_key,
        emails,
        from,
        to,
        trusted,
    } = *context;
    // A plain-text message: the body is the plain body and the rich body.
    let mime_type = TEXT_PLAIN;
    let body = message.text_body.as_str();
    let address = |email: &String| EmailAddress {
        name: String::new(),
        address: email.clone(),
    };
    let template = DraftTemplate {
        subject: message.subject.clone(),
        body: body.to_owned(),
        mime_type: mime_type.to_owned(),
        sender: EmailAddress {
            name: message.from_name.clone().unwrap_or_default(),
            address: from.to_owned(),
        },
        to_list: message.to.iter().map(address).collect(),
        cc_list: message.cc.iter().map(address).collect(),
        bcc_list: message.bcc.iter().map(address).collect(),
        ..DraftTemplate::default()
    };
    let mime_body = build_mime_body(
        &MimeSource {
            from: &template.sender,
            to: &template.to_list,
            cc: &template.cc_list,
            subject: &template.subject,
            text: body,
        },
        attachments,
    );

    let draft = create_draft(client, addr_key, emails, from, to, template)
        .await
        .map_err(|error| draft_error(&error).unwrap_or_else(|| anyhow!(error)))
        .context("failed to create draft")?;

    let prepared = async {
        let att_keys = create_attachments(client, addr_key, &draft.metadata.id, attachments)
            .await
            .context("failed to create attachments")?;
        let recipients = get_recipients(client, user_kr, settings, &draft, trusted)
            .await
            .context("failed to get recipients")?;
        create_send_req(addr_key, &mime_body, body, body, &recipients, &att_keys)
            .context("failed to create packages")
    }
    .await;
    let req = match prepared {
        Ok(req) => req,
        Err(error) => {
            // Nothing was sent: remove the draft so a retry starts clean.
            let _ = client
                .delete_messages(std::slice::from_ref(&draft.metadata.id))
                .await;
            return Err(error);
        }
    };

    client
        .send_draft(&draft.metadata.id, &req)
        .await
        .context("failed to send draft")?;
    Ok(())
}

/// Bridge's typed errors for draft creation.
fn draft_error(error: &Error) -> Option<anyhow::Error> {
    if error.status() != Some(422) {
        return None;
    }
    match error.code()?.0 {
        VALIDATION_FAILED => Some(anyhow!("Proton rejected the message: validation failed")),
        MESSAGE_TOO_LARGE => Some(anyhow!("the message is too large for Proton")),
        INVALID_LIST_OF_RECIPIENTS => Some(anyhow!("Proton rejected the list of recipients")),
        _ => None,
    }
}

/// Creates the draft to send (`createDraft`).
async fn create_draft(
    client: &Client,
    addr_key: &UnlockedKey,
    emails: &[String],
    from: &str,
    to: &[String],
    mut template: DraftTemplate,
) -> Result<Message, Error> {
    // Check sender: set the sender if it's missing.
    if template.sender.address.is_empty() {
        template.sender.address = from.to_owned();
    }

    // Check that the sending address is owned by the user, and if so,
    // sanitize it.
    let sanitized = sanitize_email(&template.sender.address);
    let Some(owned) = emails
        .iter()
        .find(|email| email.eq_ignore_ascii_case(&sanitized))
    else {
        return Err(Error::Package(format!(
            "address {:?} is not owned by user",
            template.sender.address
        )));
    };
    template.sender.address = construct_email(&template.sender.address, owned);

    // Check ToList: ensure that ToList only contains addresses we actually
    // plan to send to.
    template
        .to_list
        .retain(|address| to.contains(&address.address));

    // Check BCCList: any recipients not present in the ToList or CCList are
    // BCC recipients.
    for recipient in to {
        let listed = template
            .to_list
            .iter()
            .chain(&template.cc_list)
            .chain(&template.bcc_list)
            .any(|address| &address.address == recipient);
        if !listed {
            template.bcc_list.push(EmailAddress {
                name: String::new(),
                address: recipient.clone(),
            });
        }
    }

    client
        .create_draft(
            addr_key,
            CreateDraftReq {
                message: template,
                // No reply: Bridge forwards.
                action: CreateDraftAction::Forward,
                ..CreateDraftReq::default()
            },
        )
        .await
}

/// Uploads the attachments to the draft and returns their session keys by
/// attachment ID (`createAttachments`).
async fn create_attachments(
    client: &Client,
    addr_key: &UnlockedKey,
    draft_id: &str,
    attachments: &[OutgoingFile],
) -> Result<BTreeMap<String, SessionKey>> {
    let mut keys = BTreeMap::new();
    for attachment in attachments {
        // Zmail attachments have no content ID, so they are never inline.
        let req = CreateAttachmentReq {
            message_id: draft_id.to_owned(),
            filename: attachment.name.clone(),
            // Exclude name from params since this is already provided
            // using Filename.
            mime_type: without_name_params(&attachment.mime_type),
            disposition: ATTACHMENT_DISPOSITION.to_owned(),
            content_id: String::new(),
            body: attachment.data.clone(),
        };
        let (uploaded, key) = client
            .upload_attachment(addr_key, &req)
            .await
            .context("failed to upload attachment")?;
        keys.insert(uploaded.id, key);
    }
    Ok(keys)
}

/// Drops the `name` and `filename` parameters from a media type.
fn without_name_params(media_type: &str) -> String {
    let mut parts = media_type.split(';').map(str::trim);
    let mut out = parts.next().unwrap_or_default().to_owned();
    for param in parts.filter(|param| !param.is_empty()) {
        let key = param
            .split('=')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if key != "name" && key != "filename" {
            out.push_str("; ");
            out.push_str(param);
        }
    }
    out
}

/// Works out how to send to each recipient of the draft
/// (`getRecipients`).
async fn get_recipients(
    client: &Client,
    user_kr: &KeyRing,
    settings: &MailSettings,
    draft: &Message,
    trusted: &[KeyTrust],
) -> Result<Recipients> {
    let meta = &draft.metadata;
    let mut recipients = Recipients::new();
    for address in meta
        .to_list
        .iter()
        .chain(&meta.cc_list)
        .chain(&meta.bcc_list)
    {
        let recipient = address.address.clone();
        let (pub_keys, recipient_type) = match client.get_public_keys(&recipient).await {
            Ok(keys) => keys,
            Err(error)
                if error.status() == Some(422)
                    && error.code().map(|code| code.0) == Some(ADDRESS_DOES_NOT_EXIST) =>
            {
                bail!("recipient address does not exist: {recipient}");
            }
            Err(error) => {
                return Err(anyhow!(error))
                    .with_context(|| format!("failed to get public key for {recipient}"));
            }
        };
        let contact_settings = get_contact_settings(client, user_kr, &recipient)
            .await
            .with_context(|| format!("failed to get contact settings for {recipient}"))?;
        let prefs = build_send_prefs(
            &Recipient {
                address: &recipient,
                trusted,
            },
            &contact_settings,
            settings,
            &pub_keys,
            &draft.mime_type,
            recipient_type == RecipientType::Internal,
        )?;
        recipients.insert(recipient, prefs);
    }
    Ok(recipients)
}

/// What the user's contact card says about a recipient
/// (`getContactSettings`).
async fn get_contact_settings(
    client: &Client,
    user_kr: &KeyRing,
    recipient: &str,
) -> Result<ContactSettings> {
    let contacts = client
        .get_all_contact_emails(recipient)
        .await
        .context("failed to get contact data")?;
    let Some(contact) = contacts.iter().find(|contact| contact.email == recipient) else {
        return Ok(ContactSettings::default());
    };
    let contact = client
        .get_contact(&contact.contact_id)
        .await
        .context("failed to get contact")?;
    Ok(contact.get_settings(user_kr, recipient, CardType::SIGNED)?)
}

/// Each recipient's send preferences, by address (`recipients`).
type Recipients = BTreeMap<String, SendPreferences>;

/// The recipients with one of the schemes (`recipients.scheme`).
fn with_scheme(recipients: &Recipients, schemes: &[EncryptionScheme]) -> Recipients {
    recipients
        .iter()
        .filter(|(_, prefs)| schemes.contains(&prefs.encryption_scheme))
        .map(|(address, prefs)| (address.clone(), prefs.clone()))
        .collect()
}

/// The recipients with one of the formats (`recipients.content`).
fn with_content(recipients: &Recipients, mime_types: &[&str]) -> Recipients {
    recipients
        .iter()
        .filter(|(_, prefs)| mime_types.contains(&prefs.mime_type.as_str()))
        .map(|(address, prefs)| (address.clone(), prefs.clone()))
        .collect()
}

/// Builds the send packages (`createSendReq`).
fn create_send_req(
    key: &UnlockedKey,
    mime_body: &str,
    rich_body: &str,
    plain_body: &str,
    recipients: &Recipients,
    att_keys: &BTreeMap<String, SessionKey>,
) -> Result<SendDraftReq> {
    let mut req = SendDraftReq::default();

    let mime = with_scheme(
        recipients,
        &[EncryptionScheme::PGP_MIME, EncryptionScheme::CLEAR_MIME],
    );
    if !mime.is_empty() {
        req.add_mime_package(key, mime_body, &mime)?;
    }

    let text = with_scheme(
        recipients,
        &[
            EncryptionScheme::INTERNAL,
            EncryptionScheme::CLEAR,
            EncryptionScheme::PGP_INLINE,
        ],
    );
    if !text.is_empty() {
        let html = with_content(&text, &[TEXT_HTML]);
        if !html.is_empty() {
            req.add_text_package(key, rich_body, TEXT_HTML, &html, att_keys)?;
        }
        let plain = with_content(&text, &[TEXT_PLAIN]);
        if !plain.is_empty() {
            req.add_text_package(key, plain_body, TEXT_PLAIN, &plain, att_keys)?;
        }
        if !with_content(&text, &[MULTIPART_MIXED]).is_empty() {
            bail!("invalid MIME type for MIME package: {MULTIPART_MIXED}");
        }
    }

    Ok(req)
}

/// Keeps a `+suffix` of the header address on the account address
/// (`constructEmail`).
fn construct_email(header_email: &str, address_email: &str) -> String {
    let split_at_header: Vec<&str> = header_email.split('@').collect();
    if split_at_header.len() != 2 {
        return address_email.to_owned();
    }
    let split_plus: Vec<&str> = split_at_header[0].split('+').collect();
    if split_plus.len() != 2 {
        return address_email.to_owned();
    }
    let split_at_address: Vec<&str> = address_email.split('@').collect();
    if split_at_address.len() != 2 {
        return address_email.to_owned();
    }
    format!(
        "{}+{}@{}",
        split_at_address[0], split_plus[1], split_at_address[1]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use zmail_proton_api::{RecipientKey, SignatureType};

    use crate::send_prefs::tests::KEY_A;

    #[test]
    fn keeps_plus_suffixes_as_bridge_does() {
        assert_eq!(
            construct_email("alice+news@proton.me", "Alice@proton.me"),
            "Alice+news@proton.me"
        );
        assert_eq!(
            construct_email("alice@proton.me", "Alice@proton.me"),
            "Alice@proton.me"
        );
        assert_eq!(sanitize_email("alice+news@proton.me"), "alice@proton.me");
        assert_eq!(sanitize_email("not-an-address"), "not-an-address");
    }

    #[test]
    fn strips_name_parameters_from_attachment_types() {
        assert_eq!(
            without_name_params("text/csv; charset=utf-8; name=\"r.csv\""),
            "text/csv; charset=utf-8"
        );
        assert_eq!(
            without_name_params("application/pdf;FileName=x.pdf"),
            "application/pdf"
        );
        assert_eq!(without_name_params("image/png"), "image/png");
    }

    fn prefs(scheme: EncryptionScheme, mime: &str) -> SendPreferences {
        SendPreferences {
            encryption_scheme: scheme,
            mime_type: mime.to_owned(),
            ..SendPreferences::default()
        }
    }

    #[test]
    fn groups_recipients_by_scheme_and_format() {
        let recipients = Recipients::from([
            ("a".to_owned(), prefs(EncryptionScheme::CLEAR, TEXT_PLAIN)),
            ("b".to_owned(), prefs(EncryptionScheme::INTERNAL, TEXT_HTML)),
            (
                "c".to_owned(),
                prefs(EncryptionScheme::CLEAR_MIME, MULTIPART_MIXED),
            ),
        ]);
        let text = with_scheme(
            &recipients,
            &[EncryptionScheme::INTERNAL, EncryptionScheme::CLEAR],
        );
        assert_eq!(text.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(
            with_content(&text, &[TEXT_HTML]).keys().collect::<Vec<_>>(),
            ["b"]
        );
    }

    #[test]
    fn builds_one_package_per_format() {
        // A key of our own: the test user key, unlocked.
        let keyring = zmail_proton_api::Keys(vec![test_key()])
            .unlock(b"password", None)
            .expect("unlock");
        let key = keyring.first().expect("key");
        let internal = SendPreferences {
            encrypt: true,
            pub_key: Some(RecipientKey::from_armored(KEY_A).expect("key")),
            signature_type: SignatureType::DETACHED,
            ..prefs(EncryptionScheme::INTERNAL, TEXT_HTML)
        };
        // PGP/MIME to a key we hold, so the package can be opened here.
        let pgp_mime = SendPreferences {
            encrypt: true,
            pub_key: Some(key.recipient_key()),
            signature_type: SignatureType::DETACHED,
            ..prefs(EncryptionScheme::PGP_MIME, MULTIPART_MIXED)
        };
        let clear_mime = SendPreferences {
            signature_type: SignatureType::DETACHED,
            ..prefs(EncryptionScheme::CLEAR_MIME, MULTIPART_MIXED)
        };
        let recipients = Recipients::from([
            ("a".to_owned(), prefs(EncryptionScheme::CLEAR, TEXT_PLAIN)),
            ("b".to_owned(), internal),
            ("c".to_owned(), pgp_mime),
            ("d".to_owned(), clear_mime),
        ]);
        let req = create_send_req(
            key,
            "MIME-Version: 1.0\r\n\r\nhi",
            "<p>hi</p>",
            "hi",
            &recipients,
            &BTreeMap::new(),
        )
        .expect("packages");
        let types: Vec<&str> = req
            .packages
            .iter()
            .map(|package| package.mime_type.as_str())
            .collect();
        assert_eq!(types, [MULTIPART_MIXED, TEXT_HTML, TEXT_PLAIN]);

        // One MIME package serves both MIME schemes: a key packet for the
        // PGP/MIME recipient, the session key for the clear one.
        let mime = &req.packages[0];
        assert_eq!(mime.kind, EncryptionScheme(16 | 32));
        assert!(mime.body_key.is_some());
        assert!(mime.addresses["d"].body_key_packet.is_empty());
        let mut message = BASE64
            .decode(&mime.addresses["c"].body_key_packet)
            .expect("b64");
        message.extend(BASE64.decode(&mime.body).expect("b64"));
        assert_eq!(
            keyring
                .decrypt_binary(&message)
                .expect("decrypts")
                .as_slice(),
            b"MIME-Version: 1.0\r\n\r\nhi"
        );
    }

    fn test_key() -> zmail_proton_api::Key {
        serde_json::from_value(serde_json::json!({
            "ID": "k",
            "PrivateKey": include_str!("../../zmail-proton-api/src/testdata/keys/user-key-password.asc"),
            "Primary": 1,
            "Active": 1,
            "Flags": 3,
        }))
        .expect("key")
    }
}
