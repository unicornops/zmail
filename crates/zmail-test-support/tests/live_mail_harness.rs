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
use zmail_sync::{
    BackgroundRuntime, SyncService,
    power::{PowerConditions, PowerMonitor},
};
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

    /// Whether the logged-in session advertises `capability`.
    fn has_capability(&mut self, capability: &str) -> Result<bool> {
        let transcript = self.command("CAPABILITY")?;
        Ok(transcript
            .lines()
            .filter(|line| line.starts_with("* CAPABILITY"))
            .flat_map(str::split_whitespace)
            .any(|token| token.eq_ignore_ascii_case(capability)))
    }

    /// The UID of the selected mailbox's message with this exact subject.
    fn uid_for_subject(&mut self, subject: &str) -> Result<u32> {
        let transcript = self.command(&format!("UID SEARCH SUBJECT \"{subject}\""))?;
        transcript
            .lines()
            .find(|line| line.starts_with("* SEARCH"))
            .and_then(|line| {
                line.split_whitespace()
                    .find_map(|token| token.parse::<u32>().ok())
            })
            .with_context(|| format!("no message with subject {subject:?}"))
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
    // Dovecot also offers QRESYNC, which takes precedence over the plain
    // CONDSTORE skip; the unchanged-sync detail names whichever ran.
    let skip_detail = if other_client.has_capability("QRESYNC")? {
        "via QRESYNC"
    } else {
        "via CONDSTORE"
    };
    other_client.command("LOGOUT")?;

    let account_id = store.insert_account(&dovecot_account())?;

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
        detail.contains(skip_detail),
        "an unchanged sync should report the mod-sequence skip: {detail}"
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
        "a flag change by another client should arrive through CHANGEDSINCE or QRESYNC"
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
        detail.contains(skip_detail),
        "the flag change should be absorbed into the checkpoint: {detail}"
    );

    harness.down()?;
    Ok(())
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn resyncs_expunges_and_flags_through_qresync() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);

    let mut other_client = RawImap::connect(
        3243,
        MAIL_HARNESS_DOVECOT_LOGIN,
        MAIL_HARNESS_DOVECOT_PASSWORD,
    )?;
    if !other_client.has_capability("QRESYNC")? {
        // Gate on the advertised capability instead of faking it; the
        // CONDSTORE test still covers servers without QRESYNC.
        println!("skipping: the Dovecot harness does not advertise QRESYNC");
        harness.down()?;
        return Ok(());
    }
    for subject in ["QRESYNC expunged", "QRESYNC flagged", "QRESYNC untouched"] {
        other_client.append(&format!(
            "From: peer@example.test\r\nSubject: {subject}\r\nTo: mailuser@example.test\r\n\r\n{subject} body.\r\n"
        ))?;
    }
    other_client.command("LOGOUT")?;

    let account_id = store.insert_account(&dovecot_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_DOVECOT_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.sync_account_and_wait(account_id)?;
    sync_service.sync_account_and_wait(account_id)?;
    assert!(
        sync_detail(&store, account_id)?.contains("via QRESYNC"),
        "an unchanged mailbox should resync through QRESYNC"
    );

    // Another client expunges one message and flags another. Without
    // QRESYNC the expunge would force a full flag scan.
    let mut other_client = RawImap::connect(
        3243,
        MAIL_HARNESS_DOVECOT_LOGIN,
        MAIL_HARNESS_DOVECOT_PASSWORD,
    )?;
    other_client.command("SELECT INBOX")?;
    let expunged = other_client.uid_for_subject("QRESYNC expunged")?;
    let flagged = other_client.uid_for_subject("QRESYNC flagged")?;
    other_client.command(&format!("UID STORE {expunged} +FLAGS (\\Deleted)"))?;
    other_client.command("EXPUNGE")?;
    other_client.command(&format!("UID STORE {flagged} +FLAGS (\\Seen \\Flagged)"))?;
    other_client.command("LOGOUT")?;

    sync_service.sync_account_and_wait(account_id)?;
    let detail = sync_detail(&store, account_id)?;
    assert!(
        detail.contains("opened with QRESYNC"),
        "the changes should arrive through the QRESYNC open: {detail}"
    );
    let threads = store.list_threads(inbox_id(&store, account_id)?, 0..20)?;
    assert!(
        !threads
            .iter()
            .any(|thread| thread.subject == "QRESYNC expunged"),
        "VANISHED (EARLIER) should drop the expunged message"
    );
    let flagged_thread = threads
        .iter()
        .find(|thread| thread.subject == "QRESYNC flagged")
        .context("the flagged message should remain")?;
    assert!(flagged_thread.is_flagged);
    assert_eq!(flagged_thread.unread_count, 0);
    assert!(
        threads
            .iter()
            .any(|thread| thread.subject == "QRESYNC untouched")
    );

    // The patched flag map is the new baseline.
    sync_service.sync_account_and_wait(account_id)?;
    assert!(sync_detail(&store, account_id)?.contains("via QRESYNC"));

    harness.down()?;
    Ok(())
}

fn sync_detail(store: &LocalStore, account_id: i64) -> Result<String> {
    store
        .list_accounts()?
        .into_iter()
        .find(|account| account.id == account_id)
        .map(|account| account.sync_status.detail)
        .context("the account should load")
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn hydrates_html_only_mail_through_the_sanitizer() -> Result<()> {
    use zmail_core::MessageRenderMode;

    let _harness_lock = live_harness_lock();
    let harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);

    // An HTML-only newsletter with a remote tracking image (issue #112).
    let mut other_client = RawImap::connect(
        3243,
        MAIL_HARNESS_DOVECOT_LOGIN,
        MAIL_HARNESS_DOVECOT_PASSWORD,
    )?;
    other_client.append(concat!(
        "From: news@example.test\r\n",
        "Subject: HTML-only probe\r\n",
        "To: mailuser@example.test\r\n",
        "Message-ID: <html-only-probe@example.test>\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: text/html; charset=utf-8\r\n",
        "\r\n",
        "<html><body><h1>Your new dashboard</h1><p>Faster exports</p>",
        "<img src=\"https://tracker.example.test/open.gif\">",
        "<table><tr><td>Plan</td><td>Price</td></tr>",
        "<tr><td>Starter</td><td>EUR 9</td></tr></table></body></html>\r\n",
    ))?;
    other_client.command("LOGOUT")?;

    let account_id = store.insert_account(&dovecot_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_DOVECOT_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.sync_account_and_wait(account_id)?;

    let thread_id = store
        .list_threads(inbox_id(&store, account_id)?, 0..10)?
        .iter()
        .find(|thread| thread.subject == "HTML-only probe")
        .map(|thread| thread.id)
        .context("the HTML-only message should sync")?;
    let hydrated = runtime
        .handle()
        .block_on(sync_service.hydrate_thread_body_and_wait(account_id, thread_id))?;
    assert_eq!(hydrated, 1, "the HTML-only body should hydrate");

    let view = store
        .thread_view(thread_id)?
        .context("the HTML-only thread should load")?;
    let message = &view.messages[0];
    assert_eq!(message.render_mode, MessageRenderMode::SanitizedHtml);
    assert!(message.body_text.is_none());
    let sanitized_html = message
        .sanitized_html
        .as_deref()
        .context("the sanitized HTML should be stored")?;
    assert!(sanitized_html.contains("blocked://tracker.example.test/open.gif"));
    assert!(
        message.has_remote_content,
        "the remote image must be reported"
    );
    assert!(
        message
            .rendered_body
            .contains("Your new dashboard\nFaster exports\nPlan Price\nStarter EUR 9"),
        "block elements and table cells keep their separators: {:?}",
        message.rendered_body
    );

    harness.down()?;
    Ok(())
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn background_indexing_makes_unopened_bodies_searchable() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&imap_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_RECIPIENT_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    // Runners report no battery, but pin the conditions so the test never
    // depends on the host.
    sync_service.set_power_monitor(Arc::new(PluggedIn));

    // Three messages whose bodies carry words found nowhere in the headers.
    let words = ["cormorant", "kittiwake", "guillemot"];
    let smtp_runtime = Builder::new_current_thread().enable_all().build()?;
    for word in words {
        smtp_runtime.block_on(submit_text_message(
            &harness_submission(),
            &SubmittedMessage {
                envelope_from: MAIL_HARNESS_SENDER.to_owned(),
                from_name: Some("Harness Sender".to_owned()),
                recipients: vec![MAIL_HARNESS_RECIPIENT.to_owned()],
                subject: format!("Indexing check {word}"),
                text_body: format!("The body mentions the seabird {word}x only here."),
            },
        ))?;
    }
    sync_service.sync_account_and_wait(account_id)?;

    for word in words {
        assert!(
            store
                .search_threads(account_id, &format!("{word}x"), 10)?
                .is_empty(),
            "header-only sync cannot find body text yet"
        );
    }
    let before = store.index_progress(account_id)?;
    assert!(before.pending() >= words.len(), "{before:?}");

    let report = sync_service.index_account_and_wait(account_id)?;
    assert_eq!(report.paused, None);
    assert!(report.indexed >= words.len(), "{report:?}");
    let after = store.index_progress(account_id)?;
    assert_eq!(after.pending(), 0, "{after:?}");
    for word in words {
        let hits = store.search_threads(account_id, &format!("{word}x"), 10)?;
        assert_eq!(hits.len(), 1, "{word} should be searchable after indexing");
        assert_eq!(hits[0].subject, format!("Indexing check {word}"));
    }
    // Indexing peeks: nothing it fetched was marked read on the server.
    sync_service.sync_account_and_wait(account_id)?;
    let unread = store.search_threads(account_id, "is:unread subject:indexing", 10)?;
    assert_eq!(unread.len(), words.len());

    // A second pass has nothing left to do.
    assert_eq!(sync_service.index_account_and_wait(account_id)?.indexed, 0);
    Ok(())
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn server_search_finds_unindexed_and_unsynced_imap_mail() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&imap_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_RECIPIENT_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    let smtp_runtime = Builder::new_current_thread().enable_all().build()?;
    let send = |subject: &str, body: &str| {
        smtp_runtime.block_on(submit_text_message(
            &harness_submission(),
            &SubmittedMessage {
                envelope_from: MAIL_HARNESS_SENDER.to_owned(),
                from_name: Some("Harness Sender".to_owned()),
                recipients: vec![MAIL_HARNESS_RECIPIENT.to_owned()],
                subject: subject.to_owned(),
                text_body: body.to_owned(),
            },
        ))
    };
    let server_search = |query: &str| {
        runtime
            .handle()
            .block_on(sync_service.search_server_and_wait(account_id, query))
    };

    send(
        "Server search indexed",
        "Only the body names the shearwaterx.",
    )?;
    sync_service.sync_account_and_wait(account_id)?;
    // Headers only: the local index cannot see the body word.
    assert!(
        store
            .search_threads(account_id, "shearwaterx", 10)?
            .is_empty()
    );
    assert!(store.search_needs_server(account_id, "shearwaterx")?);

    let report = server_search("shearwaterx")?;
    assert_eq!((report.hits, report.fetched_by_sync), (1, 0), "{report:?}");
    let hits = store.search_threads(account_id, "shearwaterx", 10)?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].subject, "Server search indexed");
    assert!(hits[0].from_server);

    // State operators reach the server as SEARCH keys, and `in:` limits the
    // mailboxes searched.
    assert_eq!(server_search("shearwaterx is:read")?.hits, 0);
    assert_eq!(server_search("shearwaterx -is:read")?.hits, 1);
    assert_eq!(server_search("shearwaterx in:archive")?.hits, 0);
    assert!(
        store
            .search_threads(account_id, "shearwaterx in:archive", 10)?
            .is_empty()
    );
    // Header operators are matched locally too and need no server.
    assert!(!store.search_needs_server(account_id, "from:sender subject:indexed")?);

    // Mail delivered after the last sync: found on the server and brought
    // in by exactly one sync, still marked as a server hit (IMAP syncs
    // headers only).
    send("Server search late", "A late skuax arrives.")?;
    let report = server_search("skuax")?;
    assert_eq!((report.hits, report.fetched_by_sync), (1, 1), "{report:?}");
    let hits = store.search_threads(account_id, "skuax", 10)?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].subject, "Server search late");
    assert!(hits[0].from_server);

    // Once indexed, the same query is answered locally and needs no server.
    sync_service.index_account_and_wait(account_id)?;
    let hits = store.search_threads(account_id, "skuax", 10)?;
    assert!(!hits[0].from_server);
    assert!(!store.search_needs_server(account_id, "skuax")?);
    Ok(())
}

/// A power monitor for machines on mains power with an unmetered link.
struct PluggedIn;

impl PowerMonitor for PluggedIn {
    fn conditions(&self) -> PowerConditions {
        PowerConditions {
            on_battery: Some(false),
            metered: Some(false),
        }
    }
}

fn harness_submission() -> SubmissionConfig {
    SubmissionConfig {
        host: MAIL_HARNESS_HOST.to_owned(),
        port: MAIL_HARNESS_SMTP_PORT,
        username: Some(MAIL_HARNESS_SENDER_LOGIN.to_owned()),
        secret: Some(MAIL_HARNESS_SENDER_PASSWORD.to_owned()),
        auth: AuthKind::Password,
        security: SubmissionSecurity::Plain,
        hello_name: "localhost".to_owned(),
    }
}

fn dovecot_account() -> NewAccount {
    NewAccount {
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
    }
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
