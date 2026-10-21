// Ported from go-proton-api `address.go` (`GetAddresses`) and
// `address_types.go` at 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust. Only listing
// addresses is ported; ordering, creating and deleting addresses are not.

//! The account's addresses and their keys.

use serde::Deserialize;

use crate::{boolean::api_bool, client::Client, keyring::Keys, manager::Request, response::Error};

/// Address status (go-proton-api's `AddressStatus`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct AddressStatus(pub i64);

impl AddressStatus {
    /// Disabled.
    pub const DISABLED: Self = Self(0);
    /// Enabled.
    pub const ENABLED: Self = Self(1);
    /// Being deleted.
    pub const DELETING: Self = Self(2);
}

/// Address type (go-proton-api's `AddressType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct AddressType(pub i64);

impl AddressType {
    /// The account's original address.
    pub const ORIGINAL: Self = Self(1);
    /// An alias.
    pub const ALIAS: Self = Self(2);
    /// An address on a custom domain.
    pub const CUSTOM: Self = Self(3);
    /// A premium (short) address.
    pub const PREMIUM: Self = Self(4);
    /// An external address (bring your own email).
    pub const EXTERNAL: Self = Self(5);
}

/// One of the account's addresses (go-proton-api's `Address`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Address {
    /// Address ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// The email address.
    #[serde(rename = "Email")]
    pub email: String,
    /// Whether mail can be sent from it.
    #[serde(rename = "Send", default, deserialize_with = "api_bool")]
    pub send: bool,
    /// Whether mail is received at it.
    #[serde(rename = "Receive", default, deserialize_with = "api_bool")]
    pub receive: bool,
    /// Status.
    #[serde(rename = "Status", default)]
    pub status: AddressStatus,
    /// Type.
    #[serde(rename = "Type", default)]
    pub kind: AddressType,
    /// Sort order (lower first).
    #[serde(rename = "Order", default)]
    pub order: i64,
    /// Display name.
    #[serde(rename = "DisplayName", default)]
    pub display_name: String,
    /// The address keys.
    #[serde(rename = "Keys", default)]
    pub keys: Keys,
}

impl Address {
    /// Whether this is a bring-your-own-email address (`IsBYOEAddress`).
    pub fn is_byoe(&self) -> bool {
        self.send && self.kind == AddressType::EXTERNAL
    }
}

impl Client {
    /// Returns the account's addresses in their display order
    /// (`GetAddresses`).
    pub async fn get_addresses(&self) -> Result<Vec<Address>, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "Addresses")]
            addresses: Vec<Address>,
        }
        let mut res: Res = self.json(&Request::get("/core/v4/addresses")).await?;
        res.addresses.sort_by_key(|address| address.order);
        Ok(res.addresses)
    }
}
