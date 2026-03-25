//! Live IMAP and SMTP round-trip test for the Docker mail harness.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::runtime::Builder;
use zmail_core::{CachePolicySummary, NewAccount, NewIdentity, OutgoingConfig, ProtocolKind};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_smtp::{SubmissionConfig, SubmissionSecurity, SubmittedMessage, submit_text_message};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{
    DockerMailHarness, MAIL_HARNESS_HOST, MAIL_HARNESS_IMAP_ENDPOINT,
    MAIL_HARNESS_PRELOADED_SUBJECT, MAIL_HARNESS_RECIPIENT, MAIL_HARNESS_RECIPIENT_LOGIN,
    MAIL_HARNESS_RECIPIENT_PASSWORD, MAIL_HARNESS_SENDER, MAIL_HARNESS_SENDER_LOGIN,
    MAIL_HARNESS_SENDER_PASSWORD, MAIL_HARNESS_SMTP_PORT,
};

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn syncs_preloaded_mail_and_round_trips_smtp_submission() -> Result<()> {
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&imap_account())?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_RECIPIENT_PASSWORD)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    sync_service.sync_account_and_wait(account_id)?;
    let inbox_mailbox_id = inbox_id(&store, account_id)?;
    let initial_threads = store.list_threads(inbox_mailbox_id, 0..20)?;
    assert!(
        initial_threads
            .iter()
            .any(|thread| thread.subject == MAIL_HARNESS_PRELOADED_SUBJECT),
        "initial sync should surface the preloaded IMAP message"
    );
    let initial_message_count = store.stats()?.message_count;

    let smtp_runtime = Builder::new_current_thread().enable_all().build()?;
    smtp_runtime.block_on(async {
        submit_text_message(
            &SubmissionConfig {
                host: MAIL_HARNESS_HOST.to_owned(),
                port: MAIL_HARNESS_SMTP_PORT,
                username: Some(MAIL_HARNESS_SENDER_LOGIN.to_owned()),
                secret: Some(MAIL_HARNESS_SENDER_PASSWORD.to_owned()),
                security: SubmissionSecurity::Plain,
                hello_name: "localhost".to_owned(),
            },
            &SubmittedMessage {
                envelope_from: MAIL_HARNESS_SENDER.to_owned(),
                from_name: Some("Harness Sender".to_owned()),
                recipients: vec![MAIL_HARNESS_RECIPIENT.to_owned()],
                subject: "Phase 2b SMTP roundtrip".to_owned(),
                text_body: "This message was submitted through the local SMTP harness.".to_owned(),
            },
        )
        .await
    })?;

    sync_service.sync_account_and_wait(account_id)?;

    let inbox_mailbox_id = inbox_id(&store, account_id)?;
    let threads = store.list_threads(inbox_mailbox_id, 0..50)?;
    let roundtrip_thread = threads
        .iter()
        .find(|thread| thread.subject == "Phase 2b SMTP roundtrip")
        .context("resynced inbox should contain the SMTP round-trip message")?;
    assert!(
        store.stats()?.message_count > initial_message_count,
        "message count should increase after SMTP submission"
    );

    let preview = store
        .message_preview(roundtrip_thread.id)?
        .context("roundtrip thread should have a preview")?;
    assert!(preview.sender.contains("sender@localhost"));
    assert!(
        preview
            .body_preview
            .contains("submitted through the local SMTP harness")
    );

    Ok(())
}

fn imap_account() -> NewAccount {
    NewAccount {
        display_name: "Phase 2b Harness".to_owned(),
        protocol: ProtocolKind::Imap,
        endpoint: MAIL_HARNESS_IMAP_ENDPOINT.to_owned(),
        username: MAIL_HARNESS_RECIPIENT_LOGIN.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: Some(OutgoingConfig::smtp_default(MAIL_HARNESS_HOST)),
        identities: vec![NewIdentity {
            display_name: "Phase 2b Recipient".to_owned(),
            email_address: MAIL_HARNESS_RECIPIENT.to_owned(),
            signature: "Regards,\nPhase 2b Recipient".to_owned(),
            is_primary: true,
        }],
    }
}

fn inbox_id(store: &LocalStore, account_id: i64) -> Result<i64> {
    store
        .list_mailboxes(account_id)?
        .into_iter()
        .find(|mailbox| mailbox.name.eq_ignore_ascii_case("INBOX"))
        .map(|mailbox| mailbox.id)
        .context("synced account should contain an INBOX mailbox")
}
