// Ported from proton-bridge `pkg/message/decrypt.go` (`DecryptMessage`),
// `pkg/message/build.go` (`BuildRFC822Into` and its helpers) and
// `pkg/message/build_custom.go` at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust on rPGP with a
// small MIME writer in place of go-message. The job options are fixed to
// the ones Bridge's IMAP service uses (ignore decryption errors, sanitize
// dates and MBOX lines, add the X-Pm-* headers and the internal ID
// reference). PGP/MIME messages are always rebuilt as Bridge's
// "encrypted" form: Zmail does not extract and re-attach the embedded
// signature (`writeMultipartSignedRFC822`) because it does not verify
// signatures yet.

//! Decrypting a Proton message and rebuilding it as RFC 822.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, TimeZone, Utc};
use rand::RngCore;
use zeroize::Zeroizing;
use zmail_proton_api::{Attachment, EmailAddress, Headers, KeyRing, Message};

/// Bridge's `InternalIDDomain`.
const INTERNAL_ID_DOMAIN: &str = "protonmail.internalid";

/// One attachment after decryption (Bridge's `DecryptedAttachment`).
pub(crate) struct DecryptedAttachment {
    /// The key packets.
    packet: Vec<u8>,
    /// The encrypted data packets as downloaded.
    encrypted: Vec<u8>,
    /// The plaintext, or the reason it could not be decrypted.
    data: Result<Zeroizing<Vec<u8>>, String>,
}

/// A message after decryption (Bridge's `DecryptedMessage`).
pub(crate) struct DecryptedMessage {
    pub(crate) msg: Message,
    body: Result<Zeroizing<Vec<u8>>, String>,
    attachments: Vec<DecryptedAttachment>,
}

#[cfg(test)]
impl DecryptedMessage {
    /// Whether the body could not be decrypted.
    pub(crate) fn body_failed(&self) -> bool {
        self.body.is_err()
    }
}

/// Decrypts the body and every attachment (`DecryptMessage`). `keyring` is
/// `None` when the message's address has no unlocked key; everything then
/// counts as undecryptable.
pub(crate) fn decrypt_message(
    keyring: Option<&KeyRing>,
    msg: Message,
    attachment_data: Vec<Vec<u8>>,
) -> DecryptedMessage {
    let no_key = || "no unlocked key for this message's address".to_owned();
    let body = match keyring {
        Some(keyring) => keyring
            .decrypt_armored(&msg.body)
            .map_err(|error| error.to_string()),
        None => Err(no_key()),
    };
    let attachments = msg
        .attachments
        .iter()
        .zip(attachment_data)
        .map(|(attachment, encrypted)| {
            let packet = match BASE64.decode(&attachment.key_packets) {
                Ok(packet) => packet,
                Err(error) => {
                    return DecryptedAttachment {
                        packet: Vec::new(),
                        encrypted,
                        data: Err(format!("invalid attachment packet: {error}")),
                    };
                }
            };
            let data = match keyring {
                Some(keyring) => {
                    let mut message = packet.clone();
                    message.extend_from_slice(&encrypted);
                    keyring
                        .decrypt_binary(&message)
                        .map_err(|error| error.to_string())
                }
                None => Err(no_key()),
            };
            DecryptedAttachment {
                packet,
                encrypted,
                data,
            }
        })
        .collect();
    DecryptedMessage {
        msg,
        body,
        attachments,
    }
}

/// Builds the RFC 822 message (`BuildRFC822Into` with Bridge's IMAP job
/// options).
pub(crate) fn build_rfc822(decrypted: &mut DecryptedMessage) -> Vec<u8> {
    sanitize_mbox_header_line(decrypted);
    let mut out = Vec::new();
    if !decrypted.msg.attachments.is_empty() {
        build_multipart_rfc822(decrypted, &mut out);
    } else if decrypted.msg.mime_type == "multipart/mixed" {
        build_pgp_rfc822(decrypted, &mut out);
    } else {
        build_simple_rfc822(decrypted, &mut out);
    }
    out
}

fn build_simple_rfc822(decrypted: &DecryptedMessage, out: &mut Vec<u8>) {
    let Ok(body) = &decrypted.body else {
        // Bridge's IgnoreDecryptionErrors path.
        build_multipart_rfc822(decrypted, out);
        return;
    };
    let mut header = message_header(&decrypted.msg);
    text_part_header(&mut header, body, &decrypted.msg.mime_type);
    header.write(out);
    out.extend_from_slice(&quoted_printable(body));
}

fn build_multipart_rfc822(decrypted: &DecryptedMessage, out: &mut Vec<u8>) {
    let mut header = message_header(&decrypted.msg);
    let boundary = new_boundary();
    header.set_content_type("multipart/mixed", &[("boundary", &boundary)]);
    header.write(out);

    let mut parts = Vec::new();
    let (inline, attached): (Vec<_>, Vec<_>) = decrypted
        .msg
        .attachments
        .iter()
        .zip(&decrypted.attachments)
        .partition(|(attachment, _)| attachment.is_inline());
    if inline.is_empty() {
        parts.push(text_part(decrypted));
    } else {
        // `writeRelatedParts`.
        let related = new_boundary();
        let mut related_parts = vec![text_part(decrypted)];
        related_parts.extend(
            inline
                .iter()
                .map(|(attachment, data)| attachment_part(attachment, data)),
        );
        let mut header = PartHeader::default();
        header.set_content_type("multipart/related", &[("boundary", &related)]);
        let mut body = Vec::new();
        write_multipart(&mut body, &related, &related_parts);
        parts.push((header, body));
    }
    parts.extend(
        attached
            .iter()
            .map(|(attachment, data)| attachment_part(attachment, data)),
    );
    write_multipart(out, &boundary, &parts);
}

/// The body as a text part, or Bridge's custom part when it could not be
/// decrypted (`writeTextPart`, `writeCustomTextPart`).
fn text_part(decrypted: &DecryptedMessage) -> (PartHeader, Vec<u8>) {
    match &decrypted.body {
        Ok(body) => {
            let mut header = PartHeader::default();
            text_part_header(&mut header, body, &decrypted.msg.mime_type);
            (header, quoted_printable(body))
        }
        Err(error) => {
            let mut header = PartHeader::default();
            header.set_content_type(&decrypted.msg.mime_type, &[]);
            let comment = format!("This message could not be decrypted: {error}");
            (header, with_armor_comment(&decrypted.msg.body, &comment))
        }
    }
}

/// `writeAttachmentPart` and `writeCustomAttachmentPart`.
fn attachment_part(attachment: &Attachment, data: &DecryptedAttachment) -> (PartHeader, Vec<u8>) {
    match &data.data {
        Ok(plain) => {
            let header = attachment_part_header(attachment);
            let body = if attachment.mime_type == "message/rfc822" {
                plain.to_vec()
            } else {
                base64_lines(plain)
            };
            (header, body)
        }
        Err(error) => {
            let mut message = data.packet.clone();
            message.extend_from_slice(&data.encrypted);
            let filename = q_encode(&format!("{}.pgp", attachment.name));
            let mut header = PartHeader::default();
            header.set_content_type("application/octet-stream", &[("name", &filename)]);
            header.set(
                "Content-Disposition",
                &media_with_params(&attachment.disposition, &[("filename", &filename)]),
            );
            let comment = format!("This attachment could not be decrypted: {error}");
            (header, armor(&message, &comment))
        }
    }
}

/// `buildPGPRFC822` without signature extraction, and its fallback.
fn build_pgp_rfc822(decrypted: &DecryptedMessage, out: &mut Vec<u8>) {
    let mut header = message_header(&decrypted.msg);
    let Ok(body) = &decrypted.body else {
        // `buildPGPMIMEFallbackRFC822`.
        let boundary = new_boundary();
        header.set_content_type(
            "multipart/encrypted",
            &[
                ("boundary", &boundary),
                ("protocol", "application/pgp-encrypted"),
            ],
        );
        header.write(out);
        let mut version = PartHeader::default();
        version.set_content_type("application/pgp-encrypted", &[]);
        version.set("Content-Description", "PGP/MIME version identification");
        let mut data = PartHeader::default();
        data.set_content_type("application/octet-stream", &[("name", "encrypted.asc")]);
        data.set(
            "Content-Disposition",
            &media_with_params("inline", &[("filename", "encrypted.asc")]),
        );
        data.set("Content-Description", "OpenPGP encrypted message");
        write_multipart(
            out,
            &boundary,
            &[
                (version, b"Version: 1".to_vec()),
                (data, decrypted.msg.body.clone().into_bytes()),
            ],
        );
        return;
    };
    // `writeMultipartEncryptedRFC822`: the decrypted body is a MIME entity;
    // its own header fields fill in what the message header lacks.
    let (body_header, body_data) = split_header_body(body);
    header.del("Content-Type");
    for (name, value) in body_header {
        if !header.has(&name) {
            header.set(&name, &value);
        }
    }
    header.write(out);
    out.extend_from_slice(body_data);
}

/// The message header (`getMessageHeader` with Bridge's IMAP options).
fn message_header(msg: &Message) -> PartHeader {
    let meta = &msg.metadata;
    let mut header = PartHeader::from_parsed(&msg.parsed_headers);
    if !meta.subject.is_empty() {
        header.set_if_needed("Subject", &q_encode(&meta.subject));
    }
    if let Some(sender) = meta.sender.as_ref().filter(|sender| !address_empty(sender)) {
        header.set_if_needed("From", &format_address(sender));
    }
    if !meta.reply_tos.is_empty()
        && !meta.is_draft()
        && (meta.reply_tos.len() != 1 || !address_empty(&meta.reply_tos[0]))
    {
        header.set_if_needed("Reply-To", &address_list(&meta.reply_tos));
    }
    for (name, list) in [
        ("To", &meta.to_list),
        ("Cc", &meta.cc_list),
        ("Bcc", &meta.bcc_list),
    ] {
        if !list.is_empty() {
            header.set_if_needed(name, &address_list(list));
        }
    }
    // `setMessageIDIfNeeded`.
    if header.get("Message-Id").is_none_or(str::is_empty) {
        let id = if meta.external_id.is_empty() {
            format!("<{}@{INTERNAL_ID_DOMAIN}>", meta.id)
        } else {
            format!("<{}>", meta.external_id)
        };
        header.set("Message-Id", &id);
    }
    // SanitizeDate.
    let date = header
        .get("Date")
        .and_then(|date| DateTime::parse_from_rfc2822(date.trim()).ok());
    if date.is_none_or(|date| date.timestamp() < 0) {
        header.set_if_needed("Date", &rfc1123z(sanitize_message_date(meta.time)));
        if !header.has("X-Original-Date") {
            let original = date
                .map(|date| date.with_timezone(&Utc))
                .unwrap_or_else(|| sanitize_message_date(0));
            header.set("X-Original-Date", &rfc1123z(original));
        }
    }
    // AddInternalID, AddExternalID, AddMessageDate.
    header.set_if_needed("X-Pm-Internal-Id", &meta.id);
    if !meta.external_id.is_empty() {
        header.set("X-Pm-External-Id", &format!("<{}>", meta.external_id));
    }
    header.set_if_needed(
        "X-Pm-Date",
        &rfc1123z(Utc.timestamp_opt(meta.time, 0).single().unwrap_or_default()),
    );
    // AddMessageIDReference.
    let references = header.values("References");
    if !references
        .iter()
        .any(|reference| reference.contains(&meta.id))
    {
        let mut all = references;
        all.push(format!("<{}@{INTERNAL_ID_DOMAIN}>", meta.id));
        header.set("References", &all.join(" "));
    }
    header
}

/// `SanitizeMessageDate`: dates before the epoch become RFC 822's own.
fn sanitize_message_date(seconds: i64) -> DateTime<Utc> {
    match Utc.timestamp_opt(seconds, 0).single() {
        Some(date) if seconds > 0 => date,
        _ => Utc
            .with_ymd_and_hms(1982, 8, 13, 0, 0, 0)
            .single()
            .unwrap_or_default(),
    }
}

fn rfc1123z(date: DateTime<Utc>) -> String {
    date.format("%a, %d %b %Y %H:%M:%S %z").to_string()
}

/// `getTextPartHeader`.
fn text_part_header(header: &mut PartHeader, body: &[u8], mime_type: &str) {
    let mime_type = if mime_type.is_empty() {
        "text/plain"
    } else {
        mime_type
    };
    if std::str::from_utf8(body).is_ok() {
        header.set_content_type(mime_type, &[("charset", "utf-8")]);
    } else {
        header.set_content_type(mime_type, &[]);
    }
    header.set("Content-Transfer-Encoding", "quoted-printable");
}

/// `getAttachmentPartHeader`.
fn attachment_part_header(attachment: &Attachment) -> PartHeader {
    let mut header = PartHeader::from_parsed(&attachment.headers);
    let name = q_encode(&attachment.name);
    let mime_type = if attachment.mime_type.is_empty() {
        "application/octet-stream"
    } else {
        attachment
            .mime_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
    };
    header.set_content_type(mime_type, &[("name", &name), ("filename", &name)]);
    let disposition = if attachment.disposition.is_empty() {
        "attachment"
    } else {
        &attachment.disposition
    };
    header.set(
        "Content-Disposition",
        &media_with_params(disposition, &[("filename", &name)]),
    );
    if attachment.mime_type == "message/rfc822" {
        header.del("Content-Transfer-Encoding");
    } else {
        header.set("Content-Transfer-Encoding", "base64");
    }
    header
}

/// `sanitizeMBOXHeaderLine`: drops `From ` and `>From ` lines in the header
/// section of a body that is itself a message.
fn sanitize_mbox_header_line(decrypted: &mut DecryptedMessage) {
    let Ok(body) = &mut decrypted.body else {
        return;
    };
    while let Some(start) = mbox_header_line(body) {
        let Some(end) = body[start..].iter().position(|byte| *byte == b'\n') else {
            return;
        };
        if end == 0 {
            return;
        }
        body.drain(start..start + end + 1);
    }
}

/// `indexMBOXHeaderLine` with `extractHeaderPartEnd`.
fn mbox_header_line(body: &[u8]) -> Option<usize> {
    let header_end = find(body, b"\n\n").or_else(|| find(body, b"\r\n\r\n"))?;
    let header = &body[..header_end];
    let has = |name: &str| {
        header.split(|byte| *byte == b'\n').any(|line| {
            line.iter()
                .position(|byte| *byte == b':')
                .filter(|colon| *colon > 0)
                .is_some_and(|colon| line[..colon].eq_ignore_ascii_case(name.as_bytes()))
        })
    };
    if !(has("from") && has("to") && has("date")) {
        return None;
    }
    (0..header_end)
        .filter(|&index| index == 0 || body[index] == b'\n')
        .map(|index| if index == 0 { 0 } else { index + 1 })
        .find(|&line| body[line..].starts_with(b"From ") || body[line..].starts_with(b">From "))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// A small MIME writer (go-message's Writer and textproto in Bridge).
// ---------------------------------------------------------------------------

/// Header fields in order, values already encoded.
#[derive(Debug, Default, Clone)]
struct PartHeader {
    fields: Vec<(String, String)>,
}

impl PartHeader {
    fn from_parsed(headers: &Headers) -> Self {
        Self {
            fields: headers
                .fields
                .iter()
                .flat_map(|(name, values)| {
                    values
                        .iter()
                        .map(move |value| (name.clone(), value.clone()))
                })
                .collect(),
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn values(&self, name: &str) -> Vec<String> {
        self.fields
            .iter()
            .filter(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
            .collect()
    }

    fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Replaces every `name` field with one, where the first one was.
    fn set(&mut self, name: &str, value: &str) {
        match self
            .fields
            .iter()
            .position(|(field, _)| field.eq_ignore_ascii_case(name))
        {
            Some(position) => {
                self.fields[position] = (name.to_owned(), value.to_owned());
                let mut index = 0;
                self.fields.retain(|(field, _)| {
                    let keep = index <= position || !field.eq_ignore_ascii_case(name);
                    index += 1;
                    keep
                });
            }
            None => self.fields.push((name.to_owned(), value.to_owned())),
        }
    }

    /// `setHeaderIfNeeded`: leaves an equal value where it is.
    fn set_if_needed(&mut self, name: &str, value: &str) {
        if self.get(name) != Some(value) {
            self.set(name, value);
        }
    }

    fn del(&mut self, name: &str) {
        self.fields
            .retain(|(field, _)| !field.eq_ignore_ascii_case(name));
    }

    fn set_content_type(&mut self, media: &str, params: &[(&str, &str)]) {
        self.set("Content-Type", &media_with_params(media, params));
    }

    fn write(&self, out: &mut Vec<u8>) {
        for (name, value) in &self.fields {
            // Header values must not carry bare line breaks.
            let value: String = value
                .chars()
                .map(|ch| if ch == '\r' || ch == '\n' { ' ' } else { ch })
                .collect();
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
}

fn media_with_params(media: &str, params: &[(&str, &str)]) -> String {
    let mut value = media.to_owned();
    for (key, param) in params {
        let quoted = param.replace('\\', "\\\\").replace('"', "\\\"");
        value.push_str(&format!("; {key}=\"{quoted}\""));
    }
    value
}

fn write_multipart(out: &mut Vec<u8>, boundary: &str, parts: &[(PartHeader, Vec<u8>)]) {
    for (header, body) in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        header.write(out);
        out.extend_from_slice(body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
}

/// `newBoundary`: 64 random hex characters.
fn new_boundary() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Splits a MIME entity into its header fields and body (`readHeaderBody`).
fn split_header_body(entity: &[u8]) -> (Vec<(String, String)>, &[u8]) {
    let (header_end, body_start) = match (find(entity, b"\r\n\r\n"), find(entity, b"\n\n")) {
        (Some(crlf), Some(lf)) if lf < crlf => (lf, lf + 2),
        (Some(crlf), _) => (crlf, crlf + 4),
        (None, Some(lf)) => (lf, lf + 2),
        (None, None) => return (Vec::new(), entity),
    };
    let text = String::from_utf8_lossy(&entity[..header_end]);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            fields.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    (fields, &entity[body_start..])
}

/// Quoted-printable with CRLF line ends and 76-character lines.
fn quoted_printable(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8);
    let lines: Vec<&[u8]> = data.split(|byte| *byte == b'\n').collect();
    for (index, line) in lines.iter().enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let mut width = 0;
        for (position, &byte) in line.iter().enumerate() {
            let last = position + 1 == line.len();
            let literal = (byte == b' ' || byte == b'\t') && !last
                || (33..=126).contains(&byte) && byte != b'=';
            let encoded_len = if literal { 1 } else { 3 };
            if width + encoded_len > 75 {
                out.extend_from_slice(b"=\r\n");
                width = 0;
            }
            if literal {
                out.push(byte);
            } else {
                out.extend_from_slice(format!("={byte:02X}").as_bytes());
            }
            width += encoded_len;
        }
        if index + 1 < lines.len() {
            out.extend_from_slice(b"\r\n");
        }
    }
    out
}

/// Base64 in 76-character lines.
fn base64_lines(data: &[u8]) -> Vec<u8> {
    let encoded = BASE64.encode(data);
    let mut out = Vec::with_capacity(encoded.len() + encoded.len() / 38);
    for chunk in encoded.as_bytes().chunks(76) {
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// An armored OpenPGP message with a `Comment` header (gopenpgp's
/// `GetArmoredWithCustomHeaders`; the optional checksum is left out).
fn armor(data: &[u8], comment: &str) -> Vec<u8> {
    let mut out = format!(
        "-----BEGIN PGP MESSAGE-----\r\nComment: {}\r\n\r\n",
        single_line(comment)
    )
    .into_bytes();
    out.extend_from_slice(&base64_lines(data));
    out.extend_from_slice(b"-----END PGP MESSAGE-----\r\n");
    out
}

/// Re-armors an armored message with a `Comment` header in place of its own
/// armor headers.
fn with_armor_comment(armored: &str, comment: &str) -> Vec<u8> {
    let mut lines = armored.lines();
    let mut out = String::new();
    match lines.next() {
        Some(first)
            if first
                .trim_start()
                .starts_with("-----BEGIN PGP MESSAGE-----") =>
        {
            out.push_str("-----BEGIN PGP MESSAGE-----\r\n");
            out.push_str(&format!("Comment: {}\r\n", single_line(comment)));
            // Skip the existing armor headers up to the blank line.
            let mut in_headers = true;
            for line in lines {
                if in_headers {
                    if line.trim().is_empty() {
                        in_headers = false;
                        out.push_str("\r\n");
                    }
                    continue;
                }
                out.push_str(line.trim_end_matches('\r'));
                out.push_str("\r\n");
            }
        }
        // Not armor at all: keep it as it is.
        _ => out.push_str(armored),
    }
    out.into_bytes()
}

fn single_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

/// RFC 2047 Q-encoding when needed (Go's `mime.QEncoding.Encode`).
fn q_encode(text: &str) -> String {
    let plain = text.chars().all(|ch| (' '..='~').contains(&ch)) && !text.contains("=?");
    if plain {
        return text.to_owned();
    }
    let mut words = Vec::new();
    let mut word = String::new();
    for ch in text.chars() {
        let mut encoded = String::new();
        if ch.is_ascii_alphanumeric() || "!*+-/".contains(ch) {
            encoded.push(ch);
        } else if ch == ' ' {
            encoded.push('_');
        } else {
            let mut buffer = [0u8; 4];
            for byte in ch.encode_utf8(&mut buffer).bytes() {
                encoded.push_str(&format!("={byte:02X}"));
            }
        }
        // An encoded word is at most 75 characters: "=?utf-8?q?" + "?=".
        if word.len() + encoded.len() > 63 {
            words.push(std::mem::take(&mut word));
        }
        word.push_str(&encoded);
    }
    words.push(word);
    words
        .iter()
        .map(|word| format!("=?utf-8?q?{word}?="))
        .collect::<Vec<_>>()
        .join(" ")
}

fn address_empty(address: &EmailAddress) -> bool {
    address.name.is_empty() && address.address.is_empty()
}

/// Go's `mail.Address.String`.
fn format_address(address: &EmailAddress) -> String {
    if address.name.is_empty() {
        return format!("<{}>", address.address);
    }
    let name = if address.name.is_ascii() {
        format!(
            "\"{}\"",
            address.name.replace('\\', "\\\\").replace('"', "\\\"")
        )
    } else {
        q_encode(&address.name)
    };
    format!("{name} <{}>", address.address)
}

fn address_list(addresses: &[EmailAddress]) -> String {
    addresses
        .iter()
        .map(format_address)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use mail_parser::MimeHeaders;

    use super::*;

    fn message(json: serde_json::Value) -> Message {
        serde_json::from_value(json).expect("message")
    }

    fn base_message() -> serde_json::Value {
        serde_json::json!({
            "ID": "msg-1",
            "AddressID": "addr-1",
            "Subject": "Héllo",
            "Sender": {"Name": "Alice", "Address": "alice@example.com"},
            "ToList": [{"Name": "", "Address": "zmail@proton.local"}],
            "Flags": 1,
            "Time": 1_792_490_400,
            "ParsedHeaders": {"Received": ["r1", "r2"], "Subject": "old", "Date": "not a date"},
            "Body": "-----BEGIN PGP MESSAGE-----\nVersion: ProtonMail\n\nwcBMA\n-----END PGP MESSAGE-----\n",
            "MIMEType": "text/plain",
        })
    }

    fn parse(raw: &[u8]) -> mail_parser::Message<'_> {
        mail_parser::MessageParser::default()
            .parse(raw)
            .expect("parses")
    }

    #[test]
    fn rebuilds_a_simple_message() {
        let mut decrypted = DecryptedMessage {
            msg: message(base_message()),
            body: Ok(Zeroizing::new(
                "Line one\r\nLine = two\r\n".as_bytes().to_vec(),
            )),
            attachments: Vec::new(),
        };
        let raw = build_rfc822(&mut decrypted);
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Received: r1\r\nReceived: r2\r\n"), "{text}");
        assert!(text.contains("Subject: =?utf-8?q?H=C3=A9llo?="), "{text}");
        assert!(text.contains("From: \"Alice\" <alice@example.com>"));
        assert!(text.contains("Message-Id: <msg-1@protonmail.internalid>"));
        assert!(text.contains("X-Pm-Internal-Id: msg-1"));
        assert!(text.contains("References: <msg-1@protonmail.internalid>"));
        assert!(
            text.contains("X-Original-Date:"),
            "an invalid date is replaced"
        );
        let parsed = parse(&raw);
        assert_eq!(parsed.subject(), Some("Héllo"));
        assert_eq!(
            parsed.body_text(0).as_deref(),
            Some("Line one\r\nLine = two\r\n")
        );
        assert_eq!(
            parsed.date().map(|date| date.to_timestamp()),
            Some(1_792_490_400)
        );
    }

    #[test]
    fn rebuilds_attachments_and_inline_parts() {
        let mut json = base_message();
        json["MIMEType"] = "text/html".into();
        json["Attachments"] = serde_json::json!([
            {"ID": "a1", "Name": "report.csv", "MIMEType": "text/csv", "Disposition": "attachment", "KeyPackets": ""},
            {"ID": "a2", "Name": "logo.png", "MIMEType": "image/png", "Disposition": "inline", "KeyPackets": "",
             "Headers": {"Content-Id": "<logo@x>"}},
        ]);
        let mut decrypted = DecryptedMessage {
            msg: message(json),
            body: Ok(Zeroizing::new(
                b"<p>Hi <img src=\"cid:logo@x\"></p>".to_vec(),
            )),
            attachments: vec![
                DecryptedAttachment {
                    packet: Vec::new(),
                    encrypted: Vec::new(),
                    data: Ok(Zeroizing::new(b"a,b\n1,2\n".to_vec())),
                },
                DecryptedAttachment {
                    packet: Vec::new(),
                    encrypted: Vec::new(),
                    data: Ok(Zeroizing::new(vec![0x89, b'P', b'N', b'G'])),
                },
            ],
        };
        let raw = build_rfc822(&mut decrypted);
        let parsed = parse(&raw);
        assert_eq!(
            parsed.body_html(0).as_deref(),
            Some("<p>Hi <img src=\"cid:logo@x\"></p>")
        );
        let names: Vec<_> = parsed
            .attachments()
            .filter_map(|part| part.attachment_name())
            .collect();
        assert_eq!(names, ["logo.png", "report.csv"]);
        let csv = parsed
            .attachments()
            .find(|part| part.attachment_name() == Some("report.csv"))
            .expect("csv");
        assert_eq!(csv.contents(), b"a,b\n1,2\n");
        assert!(String::from_utf8_lossy(&raw).contains("multipart/related"));
    }

    #[test]
    fn undecryptable_parts_keep_the_ciphertext_with_a_note() {
        let mut json = base_message();
        json["Attachments"] = serde_json::json!([
            {"ID": "a1", "Name": "secret.pdf", "MIMEType": "application/pdf", "Disposition": "attachment", "KeyPackets": "AAEC"},
        ]);
        let mut decrypted = decrypt_message(None, message(json), vec![vec![9, 9, 9]]);
        assert!(decrypted.body_failed());
        let raw = build_rfc822(&mut decrypted);
        let parsed = parse(&raw);
        let body = parsed.body_text(0).expect("text body");
        assert!(body.contains("-----BEGIN PGP MESSAGE-----"), "{body}");
        assert!(
            body.contains("This message could not be decrypted"),
            "{body}"
        );
        let attachment = parsed.attachments().next().expect("attachment");
        assert_eq!(attachment.attachment_name(), Some("secret.pdf.pgp"));
        let armored = String::from_utf8_lossy(attachment.contents());
        assert!(
            armored.contains("This attachment could not be decrypted"),
            "{armored}"
        );
        assert!(
            armored.contains(&BASE64.encode([0u8, 1, 2, 9, 9, 9])),
            "{armored}"
        );
    }

    #[test]
    fn rebuilds_pgp_mime_from_the_decrypted_entity() {
        let mut json = base_message();
        json["MIMEType"] = "multipart/mixed".into();
        json["ParsedHeaders"] = serde_json::json!({
            "Date": "Tue, 20 Oct 2026 10:00:00 +0000",
            "Content-Type": "multipart/encrypted",
        });
        let entity = b"Content-Type: text/plain; charset=utf-8\r\n\r\nInner body\r\n".to_vec();
        let mut decrypted = DecryptedMessage {
            msg: message(json),
            body: Ok(Zeroizing::new(entity)),
            attachments: Vec::new(),
        };
        let raw = build_rfc822(&mut decrypted);
        let parsed = parse(&raw);
        assert_eq!(parsed.body_text(0).as_deref(), Some("Inner body\r\n"));
        assert!(!String::from_utf8_lossy(&raw).contains("multipart/encrypted"));
    }

    #[test]
    fn drops_mbox_from_lines_in_an_embedded_header() {
        let body = b"From someone@example.com Tue Oct 20 2026\nFrom: a@example.com\nTo: b@example.com\nDate: today\n\nbody".to_vec();
        let mut decrypted = DecryptedMessage {
            msg: message(base_message()),
            body: Ok(Zeroizing::new(body)),
            attachments: Vec::new(),
        };
        sanitize_mbox_header_line(&mut decrypted);
        let Ok(body) = &decrypted.body else {
            panic!("body");
        };
        assert!(body.starts_with(b"From: a@example.com"));
    }

    #[test]
    fn encodes_like_go() {
        assert_eq!(q_encode("Plain subject"), "Plain subject");
        assert_eq!(q_encode("Grüße"), "=?utf-8?q?Gr=C3=BC=C3=9Fe?=");
        assert_eq!(
            quoted_printable("a=b \r\ntrailing \r\n".as_bytes()),
            b"a=3Db=20\r\ntrailing=20\r\n".to_vec()
        );
        let long = "x".repeat(100);
        let encoded = quoted_printable(long.as_bytes());
        assert!(encoded.split(|b| *b == b'\n').all(|line| line.len() <= 77));
        assert_eq!(
            format_address(&EmailAddress {
                name: "Bob \"B\"".to_owned(),
                address: "bob@example.com".to_owned(),
            }),
            "\"Bob \\\"B\\\"\" <bob@example.com>"
        );
    }
}
