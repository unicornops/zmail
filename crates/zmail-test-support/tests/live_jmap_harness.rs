//! Live JMAP sync test for the Docker mail harness.

use std::{sync::Arc, thread, time::Duration};

use anyhow::{Context, Result, bail};
use tokio::runtime::Builder;
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutgoingConfig, ProtocolKind,
};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_smtp::{SubmissionConfig, SubmissionSecurity, SubmittedMessage, submit_text_message};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{
    DockerMailHarness, MAIL_HARNESS_HOST, MAIL_HARNESS_JMAP_ACCOUNT, MAIL_HARNESS_JMAP_ENDPOINT,
    MAIL_HARNESS_JMAP_PASSWORD, MAIL_HARNESS_JMAP_SMTP_PORT,
};

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn syncs_mail_from_a_live_jmap_server() -> Result<()> {
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    submit_harness_message(
        "Phase 2b JMAP initial sync",
        "This message primes the live JMAP sync test.",
    )?;

    sync_until_subject(
        &sync_service,
        &store,
        account_id,
        "Phase 2b JMAP initial sync",
        12,
    )?;
    let inbox_mailbox_id = inbox_id(&store, account_id)?;
    let initial_message_count = store.stats()?.message_count;
    let initial_threads = store.list_threads(inbox_mailbox_id, 0..20)?;
    assert!(
        initial_threads
            .iter()
            .any(|thread| thread.subject == "Phase 2b JMAP initial sync"),
        "initial JMAP sync should surface the injected SMTP message"
    );

    submit_harness_message(
        "Phase 2b JMAP incremental sync",
        "This message validates the next incremental JMAP sync.",
    )?;

    let roundtrip_thread_id = sync_until_subject(
        &sync_service,
        &store,
        account_id,
        "Phase 2b JMAP incremental sync",
        12,
    )?;
    assert!(
        store.stats()?.message_count > initial_message_count,
        "message count should increase after the second SMTP injection"
    );

    let preview = store
        .message_preview(roundtrip_thread_id)?
        .context("JMAP roundtrip thread should have a preview")?;
    assert!(preview.body_preview.contains("incremental JMAP sync"));

    // Local mailbox operations queue server write-back and the next sync
    // applies them through JMAP Email/set.
    store.set_thread_read(roundtrip_thread_id, true)?;
    store.set_thread_flagged(roundtrip_thread_id, true)?;
    assert_eq!(
        store.list_pending_ops(account_id)?.len(),
        2,
        "read and flag should queue JMAP write-back operations"
    );
    sync_service.sync_account_and_wait(account_id)?;
    assert!(
        store.list_pending_ops(account_id)?.is_empty(),
        "the JMAP write-back operations should be applied and drained"
    );

    // Move the thread into another mailbox the server already exposes.
    let mailboxes = store.list_mailboxes(account_id)?;
    let target = mailboxes
        .iter()
        .find(|mailbox| !mailbox.name.eq_ignore_ascii_case("Inbox"))
        .context("the JMAP server should expose a second mailbox")?;
    let target_id = target.id;
    let target_name = target.name.clone();
    store.move_thread(roundtrip_thread_id, target_id)?;
    sync_service.sync_account_and_wait(account_id)?;
    assert!(
        store.list_pending_ops(account_id)?.is_empty(),
        "the JMAP move should be applied and drained"
    );
    let moved_threads = store.list_threads(target_id, 0..50)?;
    assert!(
        moved_threads
            .iter()
            .any(|thread| thread.subject == "Phase 2b JMAP incremental sync"),
        "the moved thread should appear in the {target_name} mailbox"
    );

    Ok(())
}

fn submit_harness_message(subject: &str, body: &str) -> Result<()> {
    let smtp_runtime = Builder::new_current_thread().enable_all().build()?;
    smtp_runtime.block_on(async {
        submit_text_message(
            &SubmissionConfig {
                host: MAIL_HARNESS_HOST.to_owned(),
                port: MAIL_HARNESS_JMAP_SMTP_PORT,
                username: None,
                secret: None,
                security: SubmissionSecurity::Plain,
                hello_name: "localhost".to_owned(),
            },
            &SubmittedMessage {
                envelope_from: "external-sender@phase2b.test".to_owned(),
                from_name: Some("External Sender".to_owned()),
                recipients: vec![MAIL_HARNESS_JMAP_ACCOUNT.to_owned()],
                subject: subject.to_owned(),
                text_body: body.to_owned(),
            },
        )
        .await
    })
}

fn sync_until_subject(
    sync_service: &SyncService,
    store: &LocalStore,
    account_id: i64,
    subject: &str,
    attempts: usize,
) -> Result<i64> {
    for _ in 0..attempts {
        sync_service.sync_account_and_wait(account_id)?;
        let inbox_mailbox_id = inbox_id(store, account_id)?;
        let threads = store.list_threads(inbox_mailbox_id, 0..50)?;
        if let Some(thread) = threads.iter().find(|thread| thread.subject == subject) {
            return Ok(thread.id);
        }
        thread::sleep(Duration::from_millis(500));
    }

    bail!("subject {subject:?} was not visible after repeated JMAP sync attempts")
}

fn jmap_account() -> NewAccount {
    NewAccount {
        display_name: "Phase 2b JMAP Harness".to_owned(),
        protocol: ProtocolKind::Jmap,
        auth: AuthKind::Password,
        endpoint: MAIL_HARNESS_JMAP_ENDPOINT.to_owned(),
        username: MAIL_HARNESS_JMAP_ACCOUNT.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: Some(OutgoingConfig::jmap_default()),
        identities: vec![NewIdentity {
            display_name: "Phase 2b JMAP Recipient".to_owned(),
            email_address: MAIL_HARNESS_JMAP_ACCOUNT.to_owned(),
            signature: "Regards,\nPhase 2b JMAP Recipient".to_owned(),
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
        .context("synced JMAP account should contain an INBOX mailbox")
}
