// Ported from go-proton-api `manager_auth.go` and `manager_auth_types.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust. SRP runs on
// `proton-srp` with the modulus signature checked by `srp_modulus.rs`
// (go-srp's `NewAuth` and `GenerateProofs`). The server proof is always
// checked (no `WithSkipVerifyProofs`). The refresh `State` is base64 rather
// than raw random bytes, and tokens are redacted from `Debug` output.
// FIDO2 is not supported (ADR 0004) but the empty `FIDO2` object is sent
// as upstream does. `AuthModulus` is not ported.

//! Sessions: SRP login, the `Auth` response, new clients and token refresh.

use std::fmt;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
};
use proton_srp::{SRPAuth, SRPProof, SrpHashVersion};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::{
    client::Client,
    hv::HumanVerification,
    manager::{Manager, Request},
    response::Error,
    srp_modulus::ModulusVerifier,
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

/// The SRP challenge from `/auth/v4/info` (go-proton-api's `AuthInfo`).
#[derive(Debug, Clone, Deserialize)]
pub struct AuthInfo {
    /// SRP password hash version (4 for current accounts).
    #[serde(rename = "Version")]
    pub version: u8,
    /// The SRP modulus, clear-signed by Proton's modulus key.
    #[serde(rename = "Modulus")]
    pub modulus: String,
    /// The server's ephemeral value, base64.
    #[serde(rename = "ServerEphemeral")]
    pub server_ephemeral: String,
    /// The account's SRP salt, base64.
    #[serde(rename = "Salt")]
    pub salt: String,
    /// The SRP session to answer.
    #[serde(rename = "SRPSession")]
    pub srp_session: String,
    /// Two-factor information.
    #[serde(rename = "2FA", default)]
    pub two_fa: TwoFaInfo,
}

/// go-proton-api's `FIDO2Req`, always empty: Zmail sends it like Bridge
/// does but does not support FIDO2.
#[derive(Debug, Default, Serialize)]
struct Fido2Req {
    #[serde(rename = "AuthenticationOptions")]
    authentication_options: Option<()>,
    #[serde(rename = "ClientData")]
    client_data: &'static str,
    #[serde(rename = "AuthenticatorData")]
    authenticator_data: &'static str,
    #[serde(rename = "Signature")]
    signature: &'static str,
    #[serde(rename = "CredentialID")]
    credential_id: Option<()>,
}

/// go-proton-api's `Auth2FAReq`.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Auth2FaReq<'a> {
    #[serde(rename = "TwoFactorCode", skip_serializing_if = "str::is_empty")]
    pub(crate) two_factor_code: &'a str,
    #[serde(rename = "FIDO2")]
    fido2: Fido2Req,
}

impl<'a> Auth2FaReq<'a> {
    pub(crate) fn totp(code: &'a str) -> Self {
        Self {
            two_factor_code: code,
            fido2: Fido2Req::default(),
        }
    }
}

#[derive(Serialize)]
struct AuthReq<'a> {
    #[serde(flatten)]
    two_fa: Auth2FaReq<'a>,
    #[serde(rename = "Username")]
    username: &'a str,
    #[serde(rename = "ClientEphemeral")]
    client_ephemeral: String,
    #[serde(rename = "ClientProof")]
    client_proof: String,
    #[serde(rename = "SRPSession")]
    srp_session: &'a str,
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
    /// Signs in with SRP (`NewClientWithLogin`).
    ///
    /// Runs `/auth/v4/info`, checks the modulus signature, sends the SRP
    /// proofs to `/auth/v4` and checks the server's proof before trusting
    /// the session. If [`Auth::two_fa`] asks for TOTP, call
    /// [`Client::auth_2fa`] next. The password is only borrowed; keep it in
    /// a zeroizing buffer on the caller's side.
    pub async fn login(&self, username: &str, password: &str) -> Result<(Client, Auth), Error> {
        self.login_with_hv(username, password, None).await
    }

    /// Signs in after human verification (`NewClientWithLoginWithHVToken`):
    /// pass the [`HumanVerification`] from the 9001 error once the user has
    /// completed it.
    pub async fn login_with_hv(
        &self,
        username: &str,
        password: &str,
        hv: Option<&HumanVerification>,
    ) -> Result<(Client, Auth), Error> {
        let info = self.auth_info(username).await?;
        let proofs = srp_proofs(&info, username, password)?;
        let request = Request::post("/auth/v4")
            .human_verification(hv)
            .json(&AuthReq {
                two_fa: Auth2FaReq::default(),
                username,
                client_ephemeral: BASE64.encode(proofs.client_ephemeral),
                client_proof: BASE64.encode(proofs.client_proof),
                srp_session: &info.srp_session,
            })?;
        let auth: Auth = self.send(&request, None).await?.json()?;
        check_server_proof(&proofs, &auth.server_proof)?;
        let client = Client::new(
            self.clone(),
            &auth.uid,
            &auth.access_token,
            &auth.refresh_token,
        );
        Ok((client, auth))
    }

    /// `POST /auth/v4/info` (`AuthInfo`).
    pub async fn auth_info(&self, username: &str) -> Result<AuthInfo, Error> {
        #[derive(Serialize)]
        struct AuthInfoReq<'a> {
            #[serde(rename = "Username")]
            username: &'a str,
        }
        let request = Request::post("/auth/v4/info").json(&AuthInfoReq { username })?;
        self.send(&request, None).await?.json()
    }

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

/// Builds the SRP proofs for a login challenge (go-srp `NewAuth` and
/// `GenerateProofs`), checking the modulus signature first.
fn srp_proofs(info: &AuthInfo, username: &str, password: &str) -> Result<SRPProof, Error> {
    let version = SrpHashVersion::try_from(info.version).map_err(Error::Srp)?;
    let auth = SRPAuth::new(
        &ModulusVerifier,
        Some(username),
        password,
        version,
        &info.salt,
        &info.modulus,
        &info.server_ephemeral,
    )
    .map_err(Error::Srp)?;
    auth.generate_proofs().map_err(Error::Srp)
}

/// Compares the server's proof in constant time (`ErrInvalidProof`).
fn check_server_proof(proofs: &SRPProof, server_proof: &str) -> Result<(), Error> {
    let server_proof = BASE64
        .decode(server_proof)
        .map_err(|_| Error::InvalidServerProof)?;
    if proofs.compare_server_proof(&server_proof) {
        Ok(())
    } else {
        Err(Error::InvalidServerProof)
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
