//! Live IMAP and SMTP round-trip tests for the Docker mail harness.

use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
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
    DockerMailHarness, MAIL_HARNESS_DOVECOT_ENDPOINT, MAIL_HARNESS_DOVECOT_LOGIN,
    MAIL_HARNESS_DOVECOT_PASSWORD, MAIL_HARNESS_HOST, MAIL_HARNESS_IMAP_ENDPOINT,
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
                auth: AuthKind::Password,
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

    // Lazy sync stores headers only; the round-trip body arrives on demand.
    let hydrated = runtime
        .handle()
        .block_on(sync_service.hydrate_thread_body_and_wait(account_id, roundtrip_thread.id))?;
    assert_eq!(hydrated, 1, "the round-trip message body should hydrate");

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
                auth: AuthKind::Password,
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

    let hydrated = runtime
        .handle()
        .block_on(sync_service.hydrate_thread_body_and_wait(account_id, thread.id))?;
    assert_eq!(hydrated, 1, "the attachment message body should hydrate");

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

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn hydrates_bodies_lazily_and_survives_offline_opens() -> Result<()> {
    use zmail_core::MessageRenderMode;

    let _harness_lock = live_harness_lock();
    let harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&imap_account())?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_RECIPIENT_PASSWORD)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.sync_account_and_wait(account_id)?;

    let inbox_mailbox_id = inbox_id(&store, account_id)?;
    let threads = store.list_threads(inbox_mailbox_id, 0..50)?;
    let preloaded = threads
        .iter()
        .find(|thread| thread.subject == MAIL_HARNESS_PRELOADED_SUBJECT)
        .context("the preloaded message should sync")?;

    // Headers-only sync: the body is absent and the reader falls back to
    // the preview.
    let view = store
        .thread_view(preloaded.id)?
        .context("the preloaded thread should load")?;
    assert_eq!(view.messages[0].render_mode, MessageRenderMode::PreviewOnly);

    // Opening hydrates the body and records its preview.
    let missing = store
        .messages_missing_bodies(preloaded.id)?
        .context("the thread should exist")?;
    assert_eq!(missing.len(), 1, "one message awaits hydration");
    let started = std::time::Instant::now();
    let hydrated = runtime
        .handle()
        .block_on(sync_service.hydrate_thread_body_and_wait(account_id, preloaded.id))?;
    assert_eq!(hydrated, 1);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "hydration should complete within a second on the harness"
    );
    let view = store
        .thread_view(preloaded.id)?
        .context("the preloaded thread should load")?;
    assert_ne!(view.messages[0].render_mode, MessageRenderMode::PreviewOnly);
    assert!(
        store
            .messages_missing_bodies(preloaded.id)?
            .expect("the thread should exist")
            .is_empty(),
        "hydration drains the missing list"
    );

    // A later sync must not wipe the hydrated body: the merge keeps local
    // bodies the header-only snapshot cannot re-supply.
    sync_service.sync_account_and_wait(account_id)?;
    let view = store
        .thread_view(preloaded.id)?
        .context("the preloaded thread should load")?;
    assert_ne!(
        view.messages[0].render_mode,
        MessageRenderMode::PreviewOnly,
        "syncs must preserve hydrated bodies"
    );

    // A second message arrives and stays un-hydrated after its sync.
    let smtp_runtime = Builder::new_current_thread().enable_all().build()?;
    smtp_runtime.block_on(async {
        submit_text_message(
            &SubmissionConfig {
                host: MAIL_HARNESS_HOST.to_owned(),
                port: MAIL_HARNESS_SMTP_PORT,
                username: Some(MAIL_HARNESS_SENDER_LOGIN.to_owned()),
                secret: Some(MAIL_HARNESS_SENDER_PASSWORD.to_owned()),
                auth: AuthKind::Password,
                security: SubmissionSecurity::Plain,
                hello_name: "localhost".to_owned(),
            },
            &SubmittedMessage {
                envelope_from: MAIL_HARNESS_SENDER.to_owned(),
                from_name: Some("Harness Sender".to_owned()),
                recipients: vec![MAIL_HARNESS_RECIPIENT.to_owned()],
                subject: "Phase 2b offline probe".to_owned(),
                text_body: "This body is never fetched while the server is down.".to_owned(),
            },
        )
        .await
    })?;
    sync_service.sync_account_and_wait(account_id)?;
    let threads = store.list_threads(inbox_mailbox_id, 0..50)?;
    let offline_thread = threads
        .iter()
        .find(|thread| thread.subject == "Phase 2b offline probe")
        .context("the offline probe message should sync")?;
    let view = store
        .thread_view(offline_thread.id)?
        .context("the offline probe thread should load")?;
    assert_eq!(
        view.messages[0].render_mode,
        MessageRenderMode::PreviewOnly,
        "the fresh message starts un-hydrated"
    );

    // Offline opens keep the preview instead of surfacing an error.
    harness.down()?;
    let hydrate_result = runtime
        .handle()
        .block_on(sync_service.hydrate_thread_body_and_wait(account_id, offline_thread.id));
    assert!(
        hydrate_result.is_err(),
        "hydration against a stopped server should fail"
    );
    let view = store
        .thread_view(offline_thread.id)?
        .context("the offline probe thread should load")?;
    assert_eq!(
        view.messages[0].render_mode,
        MessageRenderMode::PreviewOnly,
        "an offline open keeps the preview, not an error"
    );

    Ok(())
}

/// A minimal blocking IMAP client used for harness-side setup against the
/// Dovecot service: appending a message and storing flags as "another
/// client" would, without pulling a full client into the test crate.
struct RawImap {
    stream: TcpStream,
    tag: usize,
}

impl RawImap {
    fn connect(port: u16, login: &str, password: &str) -> Result<Self> {
        let mut client = Self {
            stream: TcpStream::connect((MAIL_HARNESS_HOST, port))?,
            tag: 0,
        };
        client
            .stream
            .set_read_timeout(Some(Duration::from_secs(10)))?;
        let greeting = client.read_line()?;
        if !greeting.contains("OK") {
            bail!("unexpected IMAP greeting: {greeting}");
        }
        client.command(&format!("LOGIN {login} {password}"))?;
        Ok(client)
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let read = self.stream.read(&mut byte)?;
            if read == 0 {
                bail!("the IMAP connection closed mid-response");
            }
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    fn command(&mut self, command: &str) -> Result<String> {
        self.tag += 1;
        let tag = format!("T{}", self.tag);
        let request = format!("{tag} {command}\r\n");
        self.stream.write_all(request.as_bytes())?;

        // APPEND servers acknowledge the literal with a `+` continuation.
        let first = self.read_line()?;
        if first.starts_with('+') {
            return Ok(first);
        }

        // Keep the untagged lines too: SEARCH results and friends arrive
        // as `* ...` responses before the tagged completion.
        let mut response = first;
        let mut transcript = String::new();
        while !response.starts_with(&tag) {
            transcript.push_str(&response);
            response = self.read_line()?;
        }
        transcript.push_str(&response);
        if !response.contains("OK") {
            bail!("IMAP command failed: {command} -> {response}");
        }
        Ok(transcript)
    }

    fn append(&mut self, message: &str) -> Result<()> {
        self.command(&format!("APPEND INBOX {{{}}}", message.len()))?;
        self.stream.write_all(format!("{message}\r\n").as_bytes())?;
        let tag = format!("T{}", self.tag);
        let mut response = self.read_line()?;
        while !response.starts_with(&tag) {
            response = self.read_line()?;
        }
        if !response.contains("OK") {
            bail!("IMAP APPEND failed: {response}");
        }
        Ok(())
    }
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn syncs_flags_incrementally_through_condstore() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);

    // Another client appends a message before Zmail ever connects.
    let mut other_client = RawImap::connect(
        3243,
        MAIL_HARNESS_DOVECOT_LOGIN,
        MAIL_HARNESS_DOVECOT_PASSWORD,
    )?;
    other_client.append(concat!(
        "From: peer@example.test\r\n",
        "Subject: CONDSTORE probe\r\n",
        "To: mailuser@example.test\r\n",
        "Message-ID: <condstore-probe@example.test>\r\n",
        "\r\n",
        "Peer flag change target.\r\n",
    ))?;
    other_client.command("LOGOUT")?;

    let account_id = store.insert_account(&NewAccount {
        display_name: "Dovecot Harness".to_owned(),
        protocol: ProtocolKind::Imap,
        auth: AuthKind::Password,
        endpoint: MAIL_HARNESS_DOVECOT_ENDPOINT.to_owned(),
        username: MAIL_HARNESS_DOVECOT_LOGIN.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: None,
        identities: vec![NewIdentity {
            display_name: "Dovecot Recipient".to_owned(),
            email_address: "mailuser@example.test".to_owned(),
            signature: String::new(),
            is_primary: true,
        }],
    })?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_DOVECOT_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    // First sync: a full refresh that records the highest mod-sequence.
    sync_service.sync_account_and_wait(account_id)?;
    let checkpoint = store
        .sync_target(account_id)?
        .context("the sync target should load")?
        .checkpoint
        .context("the checkpoint should be recorded")?;
    assert!(
        checkpoint.contains("highest_modseq"),
        "the CONDSTORE checkpoint should record the highest mod-sequence: {checkpoint}"
    );

    let mailbox_id = store
        .list_mailboxes(account_id)?
        .into_iter()
        .find(|mailbox| mailbox.name.eq_ignore_ascii_case("INBOX"))
        .map(|mailbox| mailbox.id)
        .context("the Dovecot account should have an INBOX")?;
    let thread_id = store
        .list_threads(mailbox_id, 0..10)?
        .iter()
        .find(|thread| thread.subject == "CONDSTORE probe")
        .map(|thread| thread.id)
        .context("the appended message should sync")?;
    let view = store
        .thread_view(thread_id)?
        .context("the probe thread should load")?;
    assert!(!view.messages[0].is_flagged, "the probe starts unflagged");

    // Second sync: nothing changed, so the CONDSTORE path fetches no flags.
    sync_service.sync_account_and_wait(account_id)?;
    let detail = store
        .list_accounts()?
        .into_iter()
        .find(|account| account.id == account_id)
        .map(|account| account.sync_status.detail)
        .context("the account should load")?;
    assert!(
        detail.contains("via CONDSTORE"),
        "an unchanged sync should report the CONDSTORE skip: {detail}"
    );

    // Another client flags the message; the next sync must see it.
    let mut other_client = RawImap::connect(
        3243,
        MAIL_HARNESS_DOVECOT_LOGIN,
        MAIL_HARNESS_DOVECOT_PASSWORD,
    )?;
    other_client.command("SELECT INBOX")?;
    let search = other_client.command("SEARCH ALL")?;
    let sequence = search
        .split_whitespace()
        .find_map(|token| token.parse::<u32>().ok())
        .context("the search should find the appended message")?;
    other_client.command(&format!("STORE {sequence} +FLAGS (\\Flagged)"))?;
    other_client.command("LOGOUT")?;

    sync_service.sync_account_and_wait(account_id)?;
    let view = store
        .thread_view(thread_id)?
        .context("the probe thread should load")?;
    assert!(
        view.messages[0].is_flagged,
        "a flag change by another client should arrive through CHANGEDSINCE"
    );

    // Steady state again: the merged flag map keeps the skip path working.
    sync_service.sync_account_and_wait(account_id)?;
    let detail = store
        .list_accounts()?
        .into_iter()
        .find(|account| account.id == account_id)
        .map(|account| account.sync_status.detail)
        .context("the account should load")?;
    assert!(
        detail.contains("via CONDSTORE"),
        "the flag change should be absorbed into the checkpoint: {detail}"
    );

    harness.down()?;
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
