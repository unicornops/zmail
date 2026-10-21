// Ported from go-proton-api `attachment.go` (`GetAttachment`,
// `UploadAttachment`) and `attachment_types.go` (`Attachment`,
// `Disposition`, `CreateAttachmentReq`) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. The upload's
// multipart form is written by hand (the fields in a fixed order) instead
// of with resty, and returns the session key with the attachment so the
// caller need not decrypt the key packets it just made.

//! Message attachments.

use rand::Rng;
use serde::Deserialize;

use crate::{
    client::Client, crypto::SessionKey, header_types::Headers, keyring::UnlockedKey,
    manager::Request, response::Error,
};

/// The `inline` disposition (`InlineDisposition`).
pub const INLINE_DISPOSITION: &str = "inline";
/// The `attachment` disposition (`AttachmentDisposition`).
pub const ATTACHMENT_DISPOSITION: &str = "attachment";

/// An attachment to upload to a draft (`CreateAttachmentReq`).
#[derive(Clone, PartialEq, Eq)]
pub struct CreateAttachmentReq {
    /// The draft's message ID.
    pub message_id: String,
    /// File name.
    pub filename: String,
    /// MIME type, with any parameters other than the name.
    pub mime_type: String,
    /// `inline` or `attachment`.
    pub disposition: String,
    /// Content ID, for inline attachments.
    pub content_id: String,
    /// The attachment's data, in the clear.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for CreateAttachmentReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateAttachmentReq")
            .field("message_id", &self.message_id)
            .field("mime_type", &self.mime_type)
            .field("disposition", &self.disposition)
            .field("size", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// An attachment of a message (go-proton-api's `Attachment`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Attachment {
    /// Attachment ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// File name.
    #[serde(rename = "Name", default)]
    pub name: String,
    /// Size in bytes (encrypted).
    #[serde(rename = "Size", default)]
    pub size: i64,
    /// MIME type.
    #[serde(rename = "MIMEType", default)]
    pub mime_type: String,
    /// `inline` or `attachment`.
    #[serde(rename = "Disposition", default)]
    pub disposition: String,
    /// The part headers from the original message.
    #[serde(rename = "Headers", default)]
    pub headers: Headers,
    /// Base64 key packets that decrypt the attachment's session key.
    #[serde(rename = "KeyPackets", default)]
    pub key_packets: String,
    /// Detached signature, if any.
    #[serde(rename = "Signature", default)]
    pub signature: Option<String>,
}

impl Attachment {
    /// Whether the attachment is shown inline (`InlineDisposition`).
    pub fn is_inline(&self) -> bool {
        self.disposition.eq_ignore_ascii_case("inline")
    }
}

impl Client {
    /// Signs, encrypts and uploads an attachment to a draft with the
    /// address's first key (`UploadAttachment`). Returns the uploaded
    /// attachment and its session key.
    pub async fn upload_attachment(
        &self,
        key: &UnlockedKey,
        req: &CreateAttachmentReq,
    ) -> Result<(Attachment, SessionKey), Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "Attachment")]
            attachment: Attachment,
        }

        let signature = key.sign_detached(&req.body)?;
        let encrypted = key.encrypt_attachment(&req.body, &req.filename)?;
        let key_packets = key.encrypt_session_key(&encrypted.key)?;
        let fields = [
            ("MessageID", req.message_id.as_bytes()),
            ("Filename", req.filename.as_bytes()),
            ("MIMEType", req.mime_type.as_bytes()),
            ("Disposition", req.disposition.as_bytes()),
            ("ContentID", req.content_id.as_bytes()),
        ];
        let blobs = [
            ("KeyPackets", key_packets.as_slice()),
            ("DataPacket", encrypted.data_packet.as_slice()),
            ("Signature", signature.as_slice()),
        ];
        let (content_type, body) = multipart_form(&fields, &blobs);
        let res: Res = self
            .json(&Request::post("/mail/v4/attachments").body(content_type, body))
            .await?;
        Ok((res.attachment, encrypted.key))
    }

    /// Downloads an attachment's encrypted data packets (`GetAttachment`).
    /// Prefix them with the attachment's key packets to decrypt them.
    pub async fn get_attachment(&self, attachment_id: &str) -> Result<Vec<u8>, Error> {
        let response = self
            .send(&Request::get(format!(
                "/mail/v4/attachments/{attachment_id}"
            )))
            .await?;
        Ok(response.body)
    }
}

/// Writes a `multipart/form-data` body: text fields, then binary parts
/// named `blob` as resty sends them. Returns the content type (with the
/// boundary) and the body.
fn multipart_form(fields: &[(&str, &[u8])], blobs: &[(&str, &[u8])]) -> (String, Vec<u8>) {
    let boundary = loop {
        let candidate: String = (0..30)
            .map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16u8)))
            .collect();
        let used = fields
            .iter()
            .chain(blobs)
            .any(|(_, value)| contains(value, candidate.as_bytes()));
        if !used {
            break candidate;
        }
    };
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(value);
        body.extend_from_slice(b"\r\n");
    }
    for (name, value) in blobs {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"blob\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(value);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_multipart_form() {
        let (content_type, body) = multipart_form(
            &[("MessageID", b"draft-1"), ("ContentID", b"")],
            &[("DataPacket", b"\x00\x01")],
        );
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .expect("boundary");
        let expected = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"MessageID\"\r\n\r\ndraft-1\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"ContentID\"\r\n\r\n\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"DataPacket\"; filename=\"blob\"\r\nContent-Type: application/octet-stream\r\n\r\n\x00\x01\r\n\
             --{boundary}--\r\n"
        );
        assert_eq!(body, expected.as_bytes());
    }
}
