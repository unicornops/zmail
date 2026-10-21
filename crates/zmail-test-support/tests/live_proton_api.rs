//! Live tests for `zmail-proton-api` against the Proton harness
//! (go-proton-api's fake server, `scripts/proton-harness.sh`).
//!
//! They run the real client end to end: TLS to the harness CA, SRP login
//! with the modulus signature check and server proof, token refresh,
//! logout, key salts and unlocking the seeded account's RSA user and
//! address keys. The fake server differs from Proton in a few places (a
//! wrong password is a bare 401, not code 8002), which the assertions
//! allow for.

use anyhow::{Context, Result, ensure};
use zmail_proton_api::{
    AppVersion, ClientConfig, Error, Manager, PasswordMode, key_password, unlock,
};
use zmail_test_support::{ProtonHarness, live_harness_lock};

fn manager(harness: &ProtonHarness) -> Result<Manager> {
    let config = ClientConfig::for_test_harness(harness.base_url(), &harness.ca_pem()?)?
        .with_app_version(AppVersion::zmail(env!("CARGO_PKG_VERSION"))?);
    Ok(Manager::new(config)?)
}

fn block_on<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn logs_in_refreshes_and_logs_out() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let state = harness.state().clone();
    let manager = manager(&harness)?;

    block_on(async {
        let (client, auth) = manager
            .login(harness.username(), harness.password())
            .await
            .with_context(|| format!("login failed; server log:\n{}", harness.logs()))?;
        ensure!(auth.user_id == state.user_id, "unexpected user: {auth:?}");
        ensure!(
            !auth.two_fa.enabled.has_totp(),
            "the seeded user has no 2FA"
        );
        ensure!(auth.password_mode == PasswordMode::One);
        let user = client.get_user().await?;
        ensure!(user.id == state.user_id && user.name == harness.username());

        // Resume the session from its UID and refresh token, as Zmail does
        // after a restart.
        let (resumed, refreshed) = manager
            .new_client_with_refresh(&auth.uid, &auth.refresh_token)
            .await
            .context("refresh failed")?;
        ensure!(refreshed.uid == auth.uid);
        ensure!(
            refreshed.refresh_token != auth.refresh_token,
            "refresh token not rotated"
        );
        resumed.get_user().await.context("user after refresh")?;

        // After logout the session is gone on the server too.
        resumed.auth_delete().await.context("logout failed")?;
        ensure!(
            resumed.get_user().await.is_err(),
            "the session still works after logout"
        );
        Ok(())
    })
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn rejects_a_wrong_password() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let manager = manager(&harness)?;

    block_on(async {
        let error = manager
            .login(harness.username(), "not the password")
            .await
            .err()
            .context("a wrong password logged in")?;
        // Proton answers 8002; the fake server answers a bare 401.
        ensure!(
            matches!(error, Error::PasswordWrong(_)) || error.status() == Some(401),
            "unexpected error: {error:?}"
        );
        Ok(())
    })
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn unlocks_user_and_address_keys() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let state = harness.state().clone();
    let manager = manager(&harness)?;

    block_on(async {
        let (client, auth) = manager
            .login(harness.username(), harness.password())
            .await
            .with_context(|| format!("login failed; server log:\n{}", harness.logs()))?;
        let key_pass = key_password(auth.password_mode, harness.password(), None)?;
        let user = client.get_user().await?;
        ensure!(user.keys.primary().is_some(), "the user has no primary key");

        let salted = client
            .salted_key_pass(&user, key_pass)
            .await
            .context("salting the key password")?;
        ensure!(salted.as_bytes().len() == 31);
        ensure!(
            matches!(
                client.salted_key_pass(&user, "not the password").await,
                Err(Error::KeyPasswordWrong)
            ),
            "a wrong key password unlocked the user keys"
        );

        let addresses = client.get_addresses().await?;
        let emails: Vec<&str> = addresses.iter().map(|a| a.email.as_str()).collect();
        let expected: Vec<&str> = state.addresses.iter().map(|a| a.email.as_str()).collect();
        ensure!(emails == expected, "addresses {emails:?} != {expected:?}");

        let account = unlock(&user, &addresses, &salted).context("unlocking the account")?;
        ensure!(!account.user.is_empty());
        for address in &state.addresses {
            let keyring = account
                .addresses
                .get(&address.id)
                .with_context(|| format!("no keys unlocked for {}", address.email))?;
            ensure!(!keyring.is_empty());
        }
        Ok(())
    })
}
