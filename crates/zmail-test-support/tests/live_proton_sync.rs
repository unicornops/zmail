//! Live tests for Proton accounts in the sync runtime (`zmail-sync` with
//! its `proton` feature) against the Proton harness.
//!
//! An account is signed in the way onboarding will do it (SRP login, salted
//! key password, the three Proton secret slots) and then synced through
//! `SyncService`, which resumes the session from the keychain.

use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use zmail_core::{AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind};
use zmail_proton::{ClientConfig, store_new_session};
use zmail_proton_api::{AppVersion, Manager};
use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{ProtonHarness, live_harness_lock};

fn harness_config(harness: &ProtonHarness) -> Result<ClientConfig> {
    Ok(
        ClientConfig::for_test_harness(harness.base_url(), &harness.ca_pem()?)?
            .with_app_version(AppVersion::zmail(env!("CARGO_PKG_VERSION"))?),
    )
}

/// Signs in to the harness and stores the session as onboarding will.
fn sign_in(harness: &ProtonHarness, secrets: &dyn SecretStore, account_id: i64) -> Result<()> {
    let manager = Manager::new(harness_config(harness)?)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let (client, auth) = manager
                .login(harness.username(), harness.password())
                .await
                .with_context(|| format!("login failed; server log:\n{}", harness.logs()))?;
            let user = client.get_user().await?;
            let key_pass = client.salted_key_pass(&user, harness.password()).await?;
            store_new_session(secrets, account_id, &auth, &key_pass)
        })
}

fn proton_account(username: &str) -> NewAccount {
    NewAccount {
        display_name: "Proton".to_owned(),
        protocol: ProtocolKind::Proton,
        auth: AuthKind::ProtonSession,
        endpoint: zmail_proton_api::API_HOST_URL.to_owned(),
        username: username.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: None,
        identities: vec![NewIdentity {
            display_name: username.to_owned(),
            email_address: format!("{username}@placeholder.invalid"),
            signature: String::new(),
            is_primary: true,
        }],
    }
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn syncs_identities_and_keeps_the_rotated_refresh_token() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let state = harness.state().clone();

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let token = |secrets: &Arc<dyn SecretStore>| {
        secrets.get_account_secret_from_slot(account_id, SecretSlot::ProtonRefreshToken)
    };
    let signed_in_token = token(&secrets)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
    sync_service.configure_proton(harness_config(&harness)?)?;

    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("first sync failed; server log:\n{}", harness.logs()))?;
    let emails: Vec<String> = store
        .list_identities(account_id)?
        .into_iter()
        .map(|identity| identity.email_address)
        .collect();
    let expected: Vec<String> = state.addresses.iter().map(|a| a.email.clone()).collect();
    ensure!(emails == expected, "identities {emails:?} != {expected:?}");

    // Resuming the session rotated the refresh token, and the new one was
    // written back before the sync carried on.
    let resumed_token = token(&secrets)?;
    ensure!(
        resumed_token != signed_in_token,
        "the rotated refresh token was not stored"
    );

    // A second sync reuses the live session: no new refresh.
    sync_service.sync_account_and_wait(account_id)?;
    ensure!(
        token(&secrets)? == resumed_token,
        "the second sync refreshed again"
    );
    ensure!(
        store.list_accounts()?[0]
            .sync_status
            .checkpoint
            .as_deref()
            .is_some_and(|checkpoint| !checkpoint.is_empty()),
        "the latest event ID was not checkpointed"
    );
    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn a_revoked_session_asks_to_sign_in_again() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    secrets.set_account_secret_in_slot(account_id, SecretSlot::ProtonRefreshToken, "revoked")?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;

    let error = sync_service
        .sync_account_and_wait(account_id)
        .err()
        .context("a revoked session synced")?;
    ensure!(
        format!("{error:#}").contains("sign in again"),
        "unexpected error: {error:#}"
    );
    Ok(())
}
