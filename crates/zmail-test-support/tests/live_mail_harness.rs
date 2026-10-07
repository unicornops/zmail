//! Live IMAP and SMTP round-trip test for the Docker mail harness.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::runtime::Builder;
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutgoingAttachment, OutgoingConfig,
    OutgoingMessage, ProtocolKind,
};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_smtp::{
    SubmissionConfig, SubmissionSecurity, SubmittedMessage, submit_outgoing_message,
    submit_text_message,
};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{
    DockerMailHarness, MAIL_HARNESS_HOST, MAIL_HARNESS_IMAP_ENDPOINT,
    MAIL_HARNESS_PRELOADED_SUBJECT, MAIL_HARNESS_RECIPIENT, MAIL_HARNESS_RECIPIENT_LOGIN,
    MAIL_HARNESS_RECIPIENT_PASSWORD, MAIL_HARNESS_SENDER, MAIL_HARNESS_SENDER_LOGIN,
    MAIL_HARNESS_SENDER_PASSWORD, MAIL_HARNESS_SMTP_PORT, live_harness_lock,
};

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn syncs_preloaded_mail_and_round_trips_smtp_submission() -> Result<()> {
    let _harness_lock = live_harness_lock();
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

    // Local mailbox operations queue server write-back and the next sync
    // applies them to the IMAP server.
    let preloaded_thread = initial_threads
        .iter()
        .find(|thread| thread.subject == MAIL_HARNESS_PRELOADED_SUBJECT)
        .context("the preloaded thread should still exist")?;
    store.set_thread_read(preloaded_thread.id, true)?;
    store.set_thread_flagged(preloaded_thread.id, true)?;
    assert_eq!(
        store.list_pending_ops(account_id)?.len(),
        2,
        "read and flag should queue IMAP write-back operations"
    );

    sync_service.sync_account_and_wait(account_id)?;
    assert!(
        store.list_pending_ops(account_id)?.is_empty(),
        "the IMAP write-back operations should be applied and drained"
    );

    let threads = store.list_threads(inbox_mailbox_id, 0..50)?;
    let preloaded = threads
        .iter()
        .find(|thread| thread.subject == MAIL_HARNESS_PRELOADED_SUBJECT)
        .context("the preloaded thread should survive the write-back sync")?;
    assert_eq!(preloaded.unread_count, 0, "the thread should stay read");
    assert!(preloaded.is_flagged, "the thread should stay flagged");

    // A fresh local mirror of the same account proves the server state
    // itself changed, not just the local mirror.
    let fresh_store = Arc::new(LocalStore::open_ephemeral()?);
    let fresh_account_id = fresh_store.insert_account(&imap_account())?;
    let fresh_secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    fresh_secrets.set_account_secret(fresh_account_id, MAIL_HARNESS_RECIPIENT_PASSWORD)?;
    let fresh_sync = SyncService::new(runtime.handle().clone(), fresh_store.clone(), fresh_secrets);
    fresh_sync.sync_account_and_wait(fresh_account_id)?;
    let fresh_inbox = inbox_id(&fresh_store, fresh_account_id)?;
    let fresh_threads = fresh_store.list_threads(fresh_inbox, 0..50)?;
    let fresh_preloaded = fresh_threads
        .iter()
        .find(|thread| thread.subject == MAIL_HARNESS_PRELOADED_SUBJECT)
        .context("the fresh mirror should contain the preloaded thread")?;
    assert_eq!(
        fresh_preloaded.unread_count, 0,
        "the server should report the message as read"
    );
    assert!(
        fresh_preloaded.is_flagged,
        "the server should report the message as flagged"
    );

    Ok(())
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn syncs_attachments_and_saves_the_original_bytes() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&imap_account())?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_RECIPIENT_PASSWORD)?;

    let attachment_bytes = b"received attachment payload".to_vec();
    let smtp_runtime = Builder::new_current_thread().enable_all().build()?;
    smtp_runtime.block_on(async {
        submit_outgoing_message(
            &SubmissionConfig {
                host: MAIL_HARNESS_HOST.to_owned(),
                port: MAIL_HARNESS_SMTP_PORT,
                username: Some(MAIL_HARNESS_SENDER_LOGIN.to_owned()),
                secret: Some(MAIL_HARNESS_SENDER_PASSWORD.to_owned()),
                security: SubmissionSecurity::Plain,
                hello_name: "localhost".to_owned(),
            },
            &OutgoingMessage {
                from_email: MAIL_HARNESS_SENDER.to_owned(),
                from_name: Some("Harness Sender".to_owned()),
                to: vec![MAIL_HARNESS_RECIPIENT.to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Phase 2b attachment roundtrip".to_owned(),
                text_body: "This message carries an attachment.".to_owned(),
                attachments: vec![OutgoingAttachment {
                    filename: "notes.txt".to_owned(),
                    content_type: "text/plain".to_owned(),
                    bytes: attachment_bytes.clone(),
                }],
            },
        )
        .await
    })?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.sync_account_and_wait(account_id)?;

    let inbox_mailbox_id = inbox_id(&store, account_id)?;
    let threads = store.list_threads(inbox_mailbox_id, 0..50)?;
    let thread = threads
        .iter()
        .find(|thread| thread.subject == "Phase 2b attachment roundtrip")
        .context("the attachment message should sync into the inbox")?;

    let view = store
        .thread_view(thread.id)?
        .context("the attachment thread should load")?;
    assert_eq!(view.messages.len(), 1);
    assert_eq!(view.messages[0].attachments.len(), 1);
    let attachment = &view.messages[0].attachments[0];
    assert_eq!(attachment.filename, "notes.txt");
    assert_eq!(attachment.content_type, "text/plain");
    assert_eq!(attachment.size_bytes, attachment_bytes.len() as u64);

    let destination =
        std::env::temp_dir().join(format!("zmail-live-attachment-{}", std::process::id()));
    let saved = store.save_attachment(attachment.id, &destination)?;
    assert_eq!(saved, destination.join("notes.txt"));
    let written = std::fs::read(&saved)?;
    assert_eq!(
        written, attachment_bytes,
        "save-as restores the original bytes"
    );

    let _ = std::fs::remove_dir_all(&destination);
    Ok(())
}

fn imap_account() -> NewAccount {
    NewAccount {
        display_name: "Phase 2b Harness".to_owned(),
        protocol: ProtocolKind::Imap,
        auth: AuthKind::Password,
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
