// Ported from go-proton-api `auth.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust. `Auth2FA` takes
// a TOTP code only (no FIDO2). The session listing and revocation calls are
// not ported.

//! Second factor and sign-out for a session.

use crate::{client::Client, manager::Request, manager_auth::Auth2FaReq, response::Error};

impl Client {
    /// Completes sign-in with a TOTP code (`POST /auth/v4/2fa`).
    pub async fn auth_2fa(&self, totp_code: &str) -> Result<(), Error> {
        let request = Request::post("/auth/v4/2fa").json(&Auth2FaReq::totp(totp_code))?;
        self.send(&request).await.map(drop)
    }

    /// Signs the session out on Proton's side (`DELETE /auth/v4`). The
    /// caller then deletes the stored UID, refresh token and key password.
    pub async fn auth_delete(&self) -> Result<(), Error> {
        self.send(&Request::delete("/auth/v4")).await.map(drop)
    }
}
