//! Live checks for the Proton API test double (`scripts/proton-harness.sh`).
//!
//! These prove the harness itself: that it serves the Proton API over TLS
//! signed by its throwaway CA and that the seeded account exists. The
//! protocol tests for `zmail-proton-api` build on the same harness.

use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use zmail_test_support::{
    PROTON_HARNESS_ALIAS_ADDRESS, PROTON_HARNESS_PRIMARY_ADDRESS, PROTON_HARNESS_URL,
    PROTON_HARNESS_USERNAME, ProtonHarness, ProtonHarnessLabelKind, live_harness_lock,
};

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn serves_the_api_over_tls_signed_by_the_harness_ca() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    assert_eq!(harness.base_url(), PROTON_HARNESS_URL);

    let response = harness
        .http_client()?
        .get(format!("{}/tests/ping", harness.base_url()))
        .send()
        .with_context(|| format!("ping failed; server log:\n{}", harness.logs()))?;
    assert!(
        response.status().is_success(),
        "ping answered {}",
        response.status()
    );

    // Without the harness CA the certificate must not verify: the test
    // trusts that CA, not a relaxed verifier.
    let untrusted = Client::builder()
        .no_proxy()
        .build()?
        .get(format!("{}/tests/ping", harness.base_url()))
        .header("x-pm-appversion", &harness.state().app_version)
        .send();
    assert!(
        untrusted.is_err(),
        "the harness certificate verified without its CA"
    );

    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn seeds_an_account_with_two_addresses_and_labels() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let state = harness.state();

    assert_eq!(harness.username(), PROTON_HARNESS_USERNAME);
    let emails: Vec<&str> = state.addresses.iter().map(|a| a.email.as_str()).collect();
    assert_eq!(
        emails,
        [PROTON_HARNESS_PRIMARY_ADDRESS, PROTON_HARNESS_ALIAS_ADDRESS]
    );
    assert!(
        state
            .labels
            .iter()
            .any(|label| label.kind == ProtonHarnessLabelKind::Folder)
            && state
                .labels
                .iter()
                .any(|label| label.kind == ProtonHarnessLabelKind::Label),
        "the harness should seed both folders and labels: {:?}",
        state.labels
    );

    // The first step of an SRP login: the server knows the seeded user and
    // hands back its SRP parameters.
    let info: Value = harness
        .http_client()?
        .post(format!("{}/auth/v4/info", harness.base_url()))
        .json(&json!({ "Username": harness.username() }))
        .send()
        .with_context(|| format!("auth info failed; server log:\n{}", harness.logs()))?
        .error_for_status()?
        .json()?;
    ensure!(info["Version"] == 4, "unexpected auth info version: {info}");
    for field in ["Modulus", "ServerEphemeral", "Salt", "SRPSession"] {
        ensure!(
            info[field].as_str().is_some_and(|value| !value.is_empty()),
            "auth info has no {field}: {info}"
        );
    }

    Ok(())
}
