// Ported from go-proton-api `user.go` and `user_types.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust. Only `GetUser`
// is ported; `DeleteUser` is not.

//! The signed-in user.

use serde::Deserialize;

use crate::{client::Client, keyring::Keys, manager::Request, response::Error};

/// Storage used per Proton product, in bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ProductUsedSpace {
    /// Calendar.
    pub calendar: u64,
    /// Contacts.
    pub contact: u64,
    /// Drive.
    pub drive: u64,
    /// Mail.
    pub mail: u64,
    /// Pass.
    pub pass: u64,
}

/// The account behind a session (`GET /core/v4/users`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct User {
    /// User ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// Login name.
    pub name: String,
    /// Display name.
    pub display_name: String,
    /// Primary email address.
    pub email: String,
    /// The user keys.
    pub keys: Keys,
    /// Storage used, in bytes.
    pub used_space: u64,
    /// Storage quota, in bytes.
    pub max_space: u64,
    /// Largest upload, in bytes.
    pub max_upload: u64,
    /// Account credit, in the currency's smallest unit.
    pub credit: i64,
    /// Account currency.
    pub currency: String,
    /// Storage used per product.
    pub product_used_space: ProductUsedSpace,
}

impl Client {
    /// Returns the signed-in user (`GetUser`).
    pub async fn get_user(&self) -> Result<User, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "User")]
            user: User,
        }
        let res: Res = self.json(&Request::get("/core/v4/users")).await?;
        Ok(res.user)
    }
}
