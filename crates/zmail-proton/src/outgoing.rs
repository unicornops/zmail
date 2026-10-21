//! The MIME form of an outgoing message, for PGP/MIME and clear MIME
//! packages.
//!
//! Bridge sends those recipients the MIME message its SMTP client handed
//! over (`MIMEBody`). Zmail's outbox holds a composed message instead, so
//! this writes the MIME an SMTP client would send: the From, To, Cc,
//! Subject and Date headers (never Bcc), the text as quoted-printable
//! UTF-8, and the attachments as base64 parts of a `multipart/mixed`
//! message. Zmail's own code, written with the MIME helpers of the
//! `message.rs` port.

use chrono::Utc;
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
    pub(crate) subject: &'a str,
    pub(crate) text: &'a str,
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
    header.set("Subject", &q_encode(source.subject));
    header.set("Date", &rfc1123z(Utc::now()));
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
    use mail_parser::{MessageParser, MimeHeaders};

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
                subject: "Grüße",
                text: "Hello\nsecond line",
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
        let attachment = message.attachment(0).expect("attachment");
        assert_eq!(attachment.attachment_name(), Some("notes.txt"));
        assert_eq!(attachment.contents(), b"a,b\n1,2\n");
    }

    #[test]
    fn writes_a_single_part_without_attachments() {
        let from = address("", "zmail@proton.me");
        let body = build_mime_body(
            &MimeSource {
                from: &from,
                to: &[],
                cc: &[address("", "carol@example.com")],
                subject: "Hi",
                text: "Plain",
            },
            &[],
        );
        assert!(body.contains("Content-Type: text/plain; charset=\"utf-8\"\r\n"));
        assert!(body.contains("Cc: <carol@example.com>\r\n"));
        assert!(!body.contains("To:"));
        assert!(body.ends_with("\r\n\r\nPlain"));
    }
}
