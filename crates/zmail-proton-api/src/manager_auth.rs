// Ported from go-proton-api `manager_auth.go` and `manager_auth_types.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust. This file holds
// the session types and the token refresh; the SRP login lands with the
// auth work (issue #237). The refresh `State` is base64 rather than raw
// random bytes, and tokens are redacted from `Debug` output.

//! Sessions: the `Auth` response, new clients and token refresh.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::{
    client::Client,
    manager::{Manager, Request},
    response::Error,
};

/// Two-factor methods enabled on the account (go-proton-api's
/// `TwoFAStatus`, a bit set: 1 TOTP, 2 FIDO2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TwoFaStatus(pub u8);

impl TwoFaStatus {
    /// Whether a TOTP code is required.
    pub fn has_totp(self) -> bool {
        self.0 & 1 != 0
    }

    /// Whether a FIDO2 key is registered (not supported by Zmail yet).
    pub fn has_fido2(self) -> bool {
        self.0 & 2 != 0
    }
}

/// Two-factor information returned with an `Auth` (FIDO2 options are
/// ignored; Zmail supports TOTP only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TwoFaInfo {
    /// Enabled methods.
    #[serde(rename = "Enabled", default)]
    pub enabled: TwoFaStatus,
}

/// Whether the account uses one password or a separate mailbox password
/// (go-proton-api's `PasswordMode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "u8")]
pub enum PasswordMode {
    /// One password for login and keys.
    #[default]
    One,
    /// A second password unlocks the keys.
    Two,
}

impl From<u8> for PasswordMode {
    fn from(value: u8) -> Self {
        if value == 2 { Self::Two } else { Self::One }
    }
}

/// A session returned by `/auth/v4` and `/auth/v4/refresh`.
#[derive(Clone, Default, Deserialize)]
pub struct Auth {
    /// The account's user ID.
    #[serde(rename = "UserID", default)]
    pub user_id: String,
    /// The session UID (`x-pm-uid`).
    #[serde(rename = "UID")]
    pub uid: String,
    /// The bearer access token.
    #[serde(rename = "AccessToken")]
    pub access_token: String,
    /// The refresh token.
    #[serde(rename = "RefreshToken")]
    pub refresh_token: String,
    /// The SRP server proof (login only).
    #[serde(rename = "ServerProof", default)]
    pub server_proof: String,
    /// Granted scopes, space separated.
    #[serde(rename = "Scope", default)]
    pub scope: String,
    /// Two-factor information.
    #[serde(rename = "2FA", default)]
    pub two_fa: TwoFaInfo,
    /// One- or two-password mode.
    #[serde(rename = "PasswordMode", default)]
    pub password_mode: PasswordMode,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("user_id", &self.user_id)
            .field("uid", &self.uid)
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("scope", &self.scope)
            .field("two_fa", &self.two_fa)
            .field("password_mode", &self.password_mode)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct AuthRefreshReq<'a> {
    #[serde(rename = "UID")]
    uid: &'a str,
    #[serde(rename = "RefreshToken")]
    refresh_token: &'a str,
    #[serde(rename = "ResponseType")]
    response_type: &'static str,
    #[serde(rename = "GrantType")]
    grant_type: &'static str,
    #[serde(rename = "RedirectURI")]
    redirect_uri: &'static str,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "AccessToken", skip_serializing_if = "str::is_empty")]
    access_token: &'a str,
}

impl Manager {
    /// Creates a client for an existing session (`NewClient`).
    pub fn new_client(&self, uid: &str, access_token: &str, refresh_token: &str) -> Client {
        Client::new(self.clone(), uid, access_token, refresh_token)
    }

    /// Resumes a session from its UID and refresh token
    /// (`NewClientWithRefresh`), returning the client and the new tokens.
    pub async fn new_client_with_refresh(
        &self,
        uid: &str,
        refresh_token: &str,
    ) -> Result<(Client, Auth), Error> {
        let auth = self.auth_refresh(uid, refresh_token, "").await?;
        let client = Client::new(self.clone(), uid, &auth.access_token, &auth.refresh_token);
        Ok((client, auth))
    }

    /// `POST /auth/v4/refresh` (`authRefresh`).
    pub(crate) async fn auth_refresh(
        &self,
        uid: &str,
        refresh_token: &str,
        access_token: &str,
    ) -> Result<Auth, Error> {
        let mut state = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut state);
        let request = Request::post("/auth/v4/refresh").json(&AuthRefreshReq {
            uid,
            refresh_token,
            response_type: "token",
            grant_type: "refresh_token",
            redirect_uri: "https://protonmail.ch",
            state: URL_SAFE_NO_PAD.encode(state),
            access_token,
        })?;
        self.send(&request, None).await?.json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_decodes_and_redacts() {
        let auth: Auth = serde_json::from_str(
            r#"{"Code":1000,"UserID":"user","UID":"uid","AccessToken":"secret-access","RefreshToken":"secret-refresh","ServerProof":"proof","Scope":"full mail","2FA":{"Enabled":3,"FIDO2":{"RegisteredKeys":[]}},"PasswordMode":2}"#,
        )
        .expect("auth");
        assert_eq!(auth.uid, "uid");
        assert!(auth.two_fa.enabled.has_totp());
        assert!(auth.two_fa.enabled.has_fido2());
        assert_eq!(auth.password_mode, PasswordMode::Two);
        let debug = format!("{auth:?}");
        assert!(!debug.contains("secret"));

        let minimal: Auth =
            serde_json::from_str(r#"{"UID":"u","AccessToken":"a","RefreshToken":"r"}"#)
                .expect("minimal auth");
        assert_eq!(minimal.password_mode, PasswordMode::One);
        assert!(!minimal.two_fa.enabled.has_totp());
    }
}
