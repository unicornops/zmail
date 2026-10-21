// Ported from go-proton-api `boolean.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust as a serde
// helper for `bool` fields.

//! The API's booleans: `0` or `1` (go-proton-api's `Bool`).

use serde::{Deserialize, Deserializer};

/// Deserializes `1` as `true` and anything else as `false`, as upstream.
pub(crate) fn api_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(i64::deserialize(deserializer)? == 1)
}
