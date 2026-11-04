//! The MIME form of an outgoing message, for PGP/MIME and clear MIME
//! packages.
//!
//! Bridge sends those recipients the MIME message its SMTP client handed
//! over (`MIMEBody`). Zmail's outbox holds a composed message instead, so
//! this writes the MIME an SMTP client would send: the From, To, Cc,
//! Reply-To, Subject, Date, Message-ID, In-Reply-To and References headers
//! (never Bcc), the text as quoted-printable
//! UTF-8, and the attachments as base64 parts of a `multipart/mixed`
//! message. Zmail's own code, written with the MIME helpers of the
//! `message.rs` port.

use chrono::Utc;
use zmail_core::threading::format_message_ids;
use zmail_proton_api::EmailAddress;

use crate::message::{
    PartHeader, address_list, base64_lines, format_address, media_with_params, new_boundary,
    q_encode, quoted_printable, rfc1123z, write_multipart,
};

/// One attachment to send.
pub(crate) struct OutgoingFile {
    /// File name.
    pub(crate) name: String,
    /// MIME type, possibly with parameters.
    pub(crate) mime_type: String,
    /// The data.
    pub(crate) data: Vec<u8>,
}

/// The message headers and text the MIME form is written from.
pub(crate) struct MimeSource<'a> {
    pub(crate) from: &'a EmailAddress,
    pub(crate) to: &'a [EmailAddress],
    pub(crate) cc: &'a [EmailAddress],
    /// The sending identity's reply-to; empty for no `Reply-To`.
    pub(crate) reply_to: &'a [EmailAddress],
    pub(crate) subject: &'a str,
    pub(crate) text: &'a str,
    /// The Message-ID, without angle brackets.
    pub(crate) message_id: Option<&'a str>,
    /// The In-Reply-To id, without angle brackets.
    pub(crate) in_reply_to: Option<&'a str>,
    /// The References ids, oldest first.
    pub(crate) references: &'a [String],
}

/// Writes the message as MIME with CRLF line ends.
pub(crate) fn build_mime_body(source: &MimeSource<'_>, attachments: &[OutgoingFile]) -> String {
    let mut header = PartHeader::default();
    header.set("From", &format_address(source.from));
    if !source.to.is_empty() {
        header.set("To", &address_list(source.to));
    }
    if !source.cc.is_empty() {
        header.set("Cc", &address_list(source.cc));
    }
    if !source.reply_to.is_empty() {
        header.set("Reply-To", &address_list(source.reply_to));
    }
    header.set("Subject", &q_encode(source.subject));
    header.set("Date", &rfc1123z(Utc::now()));
    if let Some(message_id) = source.message_id {
        header.set("Message-ID", &format_message_ids(&[message_id]));
    }
    if let Some(in_reply_to) = source.in_reply_to {
        header.set("In-Reply-To", &format_message_ids(&[in_reply_to]));
    }
    if !source.references.is_empty() {
        header.set("References", &format_message_ids(source.references));
    }
    header.set("MIME-Version", "1.0");

    let mut out = Vec::new();
    if attachments.is_empty() {
        text_header(&mut header);
        header.write(&mut out);
        out.extend_from_slice(&quoted_printable(source.text.as_bytes()));
    } else {
        let boundary = new_boundary();
        header.set_content_type("multipart/mixed", &[("boundary", &boundary)]);
        header.write(&mut out);
        let mut text = PartHeader::default();
        text_header(&mut text);
        let mut parts = vec![(text, quoted_printable(source.text.as_bytes()))];
        parts.extend(attachments.iter().map(attachment_part));
        write_multipart(&mut out, &boundary, &parts);
    }
    // Every byte above is ASCII: encoded words, quoted-printable, base64.
    String::from_utf8_lossy(&out).into_owned()
}

fn text_header(header: &mut PartHeader) {
    header.set_content_type("text/plain", &[("charset", "utf-8")]);
    header.set("Content-Transfer-Encoding", "quoted-printable");
}

fn attachment_part(attachment: &OutgoingFile) -> (PartHeader, Vec<u8>) {
    let name = q_encode(&attachment.name);
    let media = attachment
        .mime_type
        .split(';')
        .next()
        .map(str::trim)
        .filter(|media| !media.is_empty())
        .unwrap_or("application/octet-stream");
    let mut header = PartHeader::default();
    header.set_content_type(media, &[("name", &name)]);
    header.set(
        "Content-Disposition",
        &media_with_params("attachment", &[("filename", &name)]),
    );
    header.set("Content-Transfer-Encoding", "base64");
    (header, base64_lines(&attachment.data))
}

#[cfg(test)]
mod tests {
    use mail_parser::MessageParser;

    use super::*;

    fn address(name: &str, address: &str) -> EmailAddress {
        EmailAddress {
            name: name.to_owned(),
            address: address.to_owned(),
        }
    }

    #[test]
    fn writes_a_message_mail_readers_parse_back() {
        let from = address("Zoë", "zmail@proton.me");
        let to = [address("", "bob@example.com")];
        let body = build_mime_body(
            &MimeSource {
                from: &from,
                to: &to,
                cc: &[],
                reply_to: &[
                    address("Help, Desk", "desk@proton.me"),
                    address("", "ops@example.com"),
                ],
                subject: "Grüße",
                text: "Hello\nsecond line",
                message_id: Some("m1@proton.me"),
                in_reply_to: Some("parent@example.com"),
                references: &[
                    "root@example.com".to_owned(),
                    "parent@example.com".to_owned(),
                ],
            },
            &[OutgoingFile {
                name: "notes.txt".to_owned(),
                mime_type: "text/plain; charset=utf-8".to_owned(),
                data: b"a,b\n1,2\n".to_vec(),
            }],
        );
        assert!(body.is_ascii());
        assert!(!body.contains("Bcc"));
        let message = MessageParser::default()
            .parse(body.as_bytes())
            .expect("parses");
        assert_eq!(message.subject(), Some("Grüße"));
        assert_eq!(
            message
                .from()
                .and_then(|from| from.first())
                .and_then(|a| a.name()),
            Some("Zoë")
        );
        assert_eq!(
            message.body_text(0).as_deref(),
            Some("Hello\r\nsecond line")
        );
        let reply_to = message
            .reply_to()
            .expect("a Reply-To")
            .mailboxes()
            .map(|address| (address.name(), address.address()))
            .collect::<Vec<_>>();
        assert_eq!(
            reply_to,
            [
                (Some("Help, Desk"), Some("desk@proton.me")),
                (None, Some("ops@example.com")),
            ]
        );
        assert_eq!(message.message_id(), Some("m1@proton.me"));
        assert_eq!(
            message.in_reply_to().and_then(|ids| ids.last()),
            Some("parent@example.com")
        );
        assert_eq!(
            message.references().map(|ids| ids.len()),
            Some(2),
            "References: {:?}",
            message.references()
        );
        let attachment = message.attachments().next().expect("attachment");
        assert_eq!(attachment.attachment_name(), Some("notes.txt"));
        assert_eq!(attachment.decoded().as_ref(), b"a,b\n1,2\n");
    }

    #[test]
    fn writes_a_single_part_without_attachments() {
        let from = address("", "zmail@proton.me");
        let body = build_mime_body(
            &MimeSource {
                from: &from,
                to: &[],
                cc: &[address("", "carol@example.com")],
                reply_to: &[],
                subject: "Hi",
                text: "Plain",
                message_id: None,
                in_reply_to: None,
                references: &[],
            },
            &[],
        );
        assert!(body.contains("Content-Type: text/plain; charset=\"utf-8\"\r\n"));
        assert!(body.contains("Cc: <carol@example.com>\r\n"));
        assert!(!body.contains("To:"));
        assert!(!body.contains("Reply-To"));
        assert!(!body.contains("Message-ID") && !body.contains("In-Reply-To"));
        assert!(body.ends_with("\r\n\r\nPlain"));
    }
}
