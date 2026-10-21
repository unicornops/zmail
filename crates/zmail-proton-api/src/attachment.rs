// Ported from go-proton-api `attachment.go` (`GetAttachment`) and
// `attachment_types.go` (`Attachment`, `Disposition`) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. Uploading
// attachments follows with sending (#237).

//! Message attachments.

use serde::Deserialize;

use crate::{client::Client, header_types::Headers, manager::Request, response::Error};

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
