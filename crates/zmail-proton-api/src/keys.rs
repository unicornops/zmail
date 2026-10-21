// Ported from go-proton-api `keys.go` (`GetPublicKeys`), `keys_types.go`
// (`PublicKey`, `PublicKeys`) and `contact_types.go` (`RecipientType`) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust.

//! Recipients' public keys.

use serde::Deserialize;

use crate::{client::Client, keyring::KeyFlags, manager::Request, response::Error};

/// A recipient's public key as the API serves it (`PublicKey`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PublicKey {
    /// Key flags.
    #[serde(rename = "Flags", default)]
    pub flags: KeyFlags,
    /// The armored public key.
    #[serde(rename = "PublicKey")]
    pub public_key: String,
}

/// Whether a recipient is on Proton (`RecipientType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "i64")]
pub enum RecipientType {
    /// A Proton address (`RecipientTypeInternal`).
    Internal,
    /// Anyone else (`RecipientTypeExternal`).
    External,
}

impl From<i64> for RecipientType {
    fn from(value: i64) -> Self {
        if value == 1 {
            Self::Internal
        } else {
            Self::External
        }
    }
}

impl Client {
    /// Looks up an address's public keys and whether it is on Proton
    /// (`GetPublicKeys`). External addresses may come with keys found
    /// through WKD.
    pub async fn get_public_keys(
        &self,
        address: &str,
    ) -> Result<(Vec<PublicKey>, RecipientType), Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "Keys", default)]
            keys: Option<Vec<PublicKey>>,
            #[serde(rename = "RecipientType", default = "external")]
            recipient_type: RecipientType,
        }

        fn external() -> RecipientType {
            RecipientType::External
        }

        let res: Res = self
            .json(&Request::get("/core/v4/keys").query("Email", address))
            .await?;
        Ok((res.keys.unwrap_or_default(), res.recipient_type))
    }
}
