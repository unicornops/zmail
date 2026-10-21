// Ported from go-proton-api `mail_settings.go` (`GetMailSettings`) and
// `mail_settings_types.go` (`MailSettings`, `SignExternalMessages`) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust; the setters are
// not ported.

//! The account's mail settings that shape sending.

use serde::Deserialize;

use crate::{
    boolean::api_bool, client::Client, manager::Request, package::EncryptionScheme, response::Error,
};

/// Mail settings (`MailSettings`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MailSettings {
    /// Display name.
    #[serde(rename = "DisplayName", default)]
    pub display_name: String,
    /// The composer's format: `text/html` or `text/plain`.
    #[serde(rename = "DraftMIMEType", default)]
    pub draft_mime_type: String,
    /// Whether to attach the sender's public key to outgoing mail.
    #[serde(rename = "AttachPublicKey", default, deserialize_with = "api_bool")]
    pub attach_public_key: bool,
    /// "Sign external messages" (`SignExternalMessages`): above zero is on.
    #[serde(rename = "Sign", default)]
    pub sign: i64,
    /// The default PGP scheme for external recipients with keys.
    #[serde(rename = "PGPScheme", default)]
    pub pgp_scheme: EncryptionScheme,
}

impl Client {
    /// The account's mail settings (`GetMailSettings`).
    pub async fn get_mail_settings(&self) -> Result<MailSettings, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "MailSettings")]
            mail_settings: MailSettings,
        }

        let res: Res = self.json(&Request::get("/mail/v4/settings")).await?;
        Ok(res.mail_settings)
    }
}
