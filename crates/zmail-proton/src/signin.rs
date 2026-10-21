// Ported from proton-bridge `internal/bridge/user.go` (`LoginAuth`,
// `LoginUser`, `LoginFull`, `loginUser`) at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust as a step-by-step
// sign-in the onboarding UI drives: each step returns what to ask for next
// instead of calling back for the TOTP code and mailbox password. Human
// verification returns the verify.proton.me URL and the same sign-in
// retries with the token once the user has finished in the browser. A
// security key (FIDO2) without TOTP is refused (Zmail v1 has no FIDO2).
// The session's rotated tokens are tracked until the account is stored.
// Adding the Bridge user (`addUser`) is Zmail's account insert followed
// by `store`.

//! Signing in to a Proton account, one step at a time.

use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Result, anyhow, bail};
use tokio::sync::Mutex;
use zeroize::Zeroizing;
use zmail_proton_api::{
    AddressStatus, Auth, Client, Error, HumanVerification, Manager, PasswordMode, SaltedKeyPass,
};
use zmail_security::SecretStore;

use crate::session::store_new_session;

/// What the sign-in needs next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInStep {
    /// Proton wants proof a person is signing in: open `url` in the
    /// browser, then call [`ProtonSignIn::login`] again.
    HumanVerification {
        /// The verify.proton.me page.
        url: String,
    },
    /// The account has two-factor authentication: ask for the code.
    Totp,
    /// The account has a separate mailbox password: ask for it.
    MailboxPassword,
    /// Signed in, with the account's primary address.
    SignedIn(SignedInAccount),
}

/// The account a sign-in reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedInAccount {
    /// The primary (first enabled) address.
    pub email: String,
    /// Its display name, or the account's.
    pub display_name: String,
}

struct State {
    manager: Manager,
    username: String,
    password: Zeroizing<String>,
    hv: Option<HumanVerification>,
    client: Option<Client>,
    /// The latest session tokens; refreshes during sign-in update them.
    auth: Arc<StdMutex<Option<Auth>>>,
    password_mode: PasswordMode,
    needs_totp: bool,
    key_pass: Option<SaltedKeyPass>,
}

/// A sign-in in progress. Cheap to clone; the clones share the state, and
/// steps run one at a time.
#[derive(Clone)]
pub struct ProtonSignIn {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for ProtonSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtonSignIn")
    }
}

impl ProtonSignIn {
    /// Starts a sign-in on `manager` (no network yet).
    pub fn new(manager: Manager, username: &str, password: &str) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                manager,
                username: username.trim().to_owned(),
                password: Zeroizing::new(password.to_owned()),
                hv: None,
                client: None,
                auth: Arc::new(StdMutex::new(None)),
                password_mode: PasswordMode::One,
                needs_totp: false,
                key_pass: None,
            })),
        }
    }

    /// Signs in with the username and password (`LoginAuth`), with the
    /// human verification token when an earlier attempt asked for one.
    pub async fn login(&self) -> Result<SignInStep> {
        let mut state = self.state.lock().await;
        if state.client.is_some() {
            return next_step(&mut state).await;
        }
        let result = state
            .manager
            .login_with_hv(&state.username, &state.password, state.hv.as_ref())
            .await;
        let (client, auth) = match result {
            Ok(session) => session,
            Err(error) => {
                if let Some(hv) = error.human_verification() {
                    let hv = hv?;
                    let url = hv.verify_url();
                    state.hv = Some(hv);
                    return Ok(SignInStep::HumanVerification { url });
                }
                return Err(sign_in_error(error));
            }
        };
        if auth.two_fa.enabled.has_fido2() && !auth.two_fa.enabled.has_totp() {
            let _ = client.auth_delete().await;
            bail!(
                "this Proton account signs in with a security key, which Zmail does not support yet; add an authenticator app (TOTP) to the account to use it here"
            );
        }
        state.needs_totp = auth.two_fa.enabled.has_totp();
        state.password_mode = auth.password_mode;
        let tokens = state.auth.clone();
        client.add_auth_handler(move |auth| {
            *tokens
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(auth.clone());
        });
        *state
            .auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(auth);
        state.client = Some(client);
        next_step(&mut state).await
    }

    /// Completes two-factor authentication with a TOTP code (`Auth2FA`).
    /// A wrong code can be tried again.
    pub async fn submit_totp(&self, code: &str) -> Result<SignInStep> {
        let mut state = self.state.lock().await;
        let client = state
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("sign in with the password first"))?;
        client
            .auth_2fa(code.trim())
            .await
            .map_err(|error| match error {
                Error::Api(_) => anyhow!("Proton did not accept that code: {error}"),
                other => sign_in_error(other),
            })?;
        state.needs_totp = false;
        next_step(&mut state).await
    }

    /// Unlocks the keys with the mailbox password (two-password mode). A
    /// wrong password can be tried again.
    pub async fn submit_mailbox_password(&self, password: &str) -> Result<SignInStep> {
        let mut state = self.state.lock().await;
        if state.needs_totp {
            bail!("enter the two-factor code first");
        }
        let password = Zeroizing::new(password.to_owned());
        login_user(&mut state, &password).await
    }

    /// Stores the session for `account_id` in the keychain: the UID, the
    /// current refresh token and the salted key password (Bridge's
    /// `addUser` writing its vault).
    pub async fn store(&self, secrets: &dyn SecretStore, account_id: i64) -> Result<()> {
        let state = self.state.lock().await;
        let key_pass = state
            .key_pass
            .as_ref()
            .ok_or_else(|| anyhow!("the sign-in has not finished"))?;
        let auth = state
            .auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| anyhow!("the sign-in has not finished"))?;
        store_new_session(secrets, account_id, &auth, key_pass)
    }

    /// Gives up: signs the session out on Proton's side (`AuthDelete`).
    pub async fn abort(&self) {
        let mut state = self.state.lock().await;
        if let Some(client) = state.client.take() {
            let _ = client.auth_delete().await;
            client.close();
        }
    }
}

/// What comes after the password or the TOTP code (`LoginFull`).
async fn next_step(state: &mut State) -> Result<SignInStep> {
    if state.needs_totp {
        return Ok(SignInStep::Totp);
    }
    match state.password_mode {
        PasswordMode::Two => Ok(SignInStep::MailboxPassword),
        PasswordMode::One => {
            let password = state.password.clone();
            login_user(state, &password).await
        }
    }
}

/// Checks the key password against the user keys and reads the account's
/// primary address (`loginUser`). A wrong key password can be retried;
/// any other failure signs the session out, as Bridge's `LoginUser` does.
async fn login_user(state: &mut State, key_pass: &str) -> Result<SignInStep> {
    let client = state
        .client
        .as_ref()
        .ok_or_else(|| anyhow!("sign in with the password first"))?;
    let result = async {
        let user = client.get_user().await?;
        let salted = client.salted_key_pass(&user, key_pass).await?;
        let addresses = client.get_addresses().await?;
        Ok::<_, Error>((user, salted, addresses))
    }
    .await;
    let (user, salted, mut addresses) = match result {
        Ok(found) => found,
        Err(Error::KeyPasswordWrong) => bail!("that mailbox password is wrong"),
        Err(error) => {
            if let Some(client) = state.client.take() {
                let _ = client.auth_delete().await;
                client.close();
            }
            return Err(sign_in_error(error));
        }
    };
    addresses.sort_by_key(|address| address.order);
    let primary = addresses
        .iter()
        .find(|address| address.status == AddressStatus::ENABLED)
        .ok_or_else(|| anyhow!("the Proton account has no enabled address"))?;
    let display_name = if primary.display_name.is_empty() {
        user.display_name.clone()
    } else {
        primary.display_name.clone()
    };
    let account = SignedInAccount {
        email: primary.email.clone(),
        display_name,
    };
    state.key_pass = Some(salted);
    Ok(SignInStep::SignedIn(account))
}

/// A user-facing message for a sign-in failure.
fn sign_in_error(error: Error) -> anyhow::Error {
    match error {
        Error::PasswordWrong(_) => anyhow!("the Proton username or password is wrong"),
        Error::PaidPlanRequired(_) => {
            anyhow!("Proton only allows third-party mail clients on paid plans")
        }
        Error::AppVersion(_) => anyhow!(
            "Proton did not accept this version of Zmail; Proton may be blocking third-party clients"
        ),
        other => anyhow!(other),
    }
}
