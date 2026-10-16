//! Live JMAP sync test for the Docker mail harness.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use tokio::runtime::Builder;
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutgoingAttachment, OutgoingConfig,
    OutgoingMessage, ProtocolKind,
};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_smtp::{SubmissionConfig, SubmissionSecurity, SubmittedMessage, submit_text_message};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{
    DockerMailHarness, MAIL_HARNESS_HOST, MAIL_HARNESS_JMAP_ACCOUNT, MAIL_HARNESS_JMAP_ENDPOINT,
    MAIL_HARNESS_JMAP_PASSWORD, MAIL_HARNESS_JMAP_SMTP_PORT, live_harness_lock,
};

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn syncs_mail_from_a_live_jmap_server() -> Result<()> {
    let _harness_lock = live_harness_lock();
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

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn downloads_jmap_attachment_blobs_into_the_store() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    // Prime the account once so the attachment sync is incremental.
    sync_service.sync_account_and_wait(account_id)?;

    let blob = b"JMAP attachment round trip payload.\n".to_vec();
    submit_harness_attachment_message(
        "Phase 2b JMAP attachment",
        "This message carries an attachment blob.",
        "notes.txt",
        blob.clone(),
    )?;

    let thread_id = sync_until_subject(
        &sync_service,
        &store,
        account_id,
        "Phase 2b JMAP attachment",
        12,
    )?;
    let view = store
        .thread_view(thread_id)?
        .context("the attachment thread should load")?;
    assert!(
        view.has_attachments,
        "the JMAP thread should carry the attachment flag"
    );
    let message = view
        .messages
        .iter()
        .find(|message| !message.attachments.is_empty())
        .context("the JMAP message should list its downloaded attachment")?;
    let attachment = &message.attachments[0];
    assert_eq!(attachment.filename, "notes.txt");
    assert_eq!(attachment.size_bytes, blob.len() as u64);

    let destination =
        std::env::temp_dir().join(format!("zmail-jmap-attachment-save-{}", std::process::id()));
    let saved = store.save_attachment(attachment.id, &destination)?;
    let written = std::fs::read(&saved).context("the saved blob should read back")?;
    assert_eq!(written, blob, "save-as writes the original bytes");
    let _ = std::fs::remove_dir_all(&destination);

    Ok(())
}

fn submit_harness_attachment_message(
    subject: &str,
    body: &str,
    filename: &str,
    bytes: Vec<u8>,
) -> Result<()> {
    let smtp_runtime = Builder::new_current_thread().enable_all().build()?;
    smtp_runtime.block_on(async {
        zmail_smtp::submit_outgoing_message(
            &SubmissionConfig {
                host: MAIL_HARNESS_HOST.to_owned(),
                port: MAIL_HARNESS_JMAP_SMTP_PORT,
                username: None,
                secret: None,
                auth: AuthKind::Password,
                security: SubmissionSecurity::Plain,
                hello_name: "localhost".to_owned(),
            },
            &OutgoingMessage {
                from_email: "external-sender@phase2b.test".to_owned(),
                from_name: Some("External Sender".to_owned()),
                to: vec![MAIL_HARNESS_JMAP_ACCOUNT.to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject.to_owned(),
                text_body: body.to_owned(),
                attachments: vec![OutgoingAttachment {
                    filename: filename.to_owned(),
                    content_type: "text/plain".to_owned(),
                    bytes,
                }],
            },
        )
        .await
    })
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn event_source_pushes_new_mail_before_the_timeout() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
    sync_service.sync_account_and_wait(account_id)?;

    let target = store
        .sync_target(account_id)?
        .with_context(|| "the JMAP target should load")?;
    let secret = secrets.get_account_secret(account_id)?;

    // Submit a message while the event source is already listening, then
    // assert the push arrives well before the wait's 45 s budget expires.
    // The ping interval equals the budget, so an early return can only be
    // a real state change, not a keepalive.
    let submitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let submitted_writer = submitted.clone();
    // A plain OS thread, not a runtime task: submit_harness_message builds its
    // own current-thread runtime, which would panic on a tokio worker thread.
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        submit_harness_message(
            "Phase 2b JMAP push",
            "This message should wake the event source.",
        )
        .expect("harness submission should succeed");
        submitted_writer.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let wait_secret = secret.clone();
    let started = Instant::now();
    let push = runtime.handle().block_on(async {
        zmail_jmap::wait_for_mail_changes(&target, &wait_secret, Duration::from_secs(45)).await
    });
    let elapsed = started.elapsed();

    assert!(
        submitted.load(std::sync::atomic::Ordering::SeqCst),
        "the harness message was submitted"
    );
    push.with_context(|| "the JMAP event source should connect against James")?;
    assert!(
        elapsed < Duration::from_secs(35),
        "a server state change should arrive before the wait budget, took {elapsed:?}"
    );

    Ok(())
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn syncs_html_only_mail_through_the_sanitizer() -> Result<()> {
    use zmail_core::MessageRenderMode;

    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    // An HTML-only newsletter with a remote tracking image (issue #130).
    // RFC 8621 lists its text/html part in textBody, so the adapter must
    // decide by part type rather than by list.
    submit_raw_harness_message(concat!(
        "From: News <news@phase2b.test>\r\n",
        "To: jmap@localhost\r\n",
        "Subject: Phase 2b JMAP HTML-only\r\n",
        "Message-ID: <jmap-html-only-probe@phase2b.test>\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: text/html; charset=utf-8\r\n",
        "\r\n",
        "<html><body><h1>Your new dashboard</h1><p>Faster exports</p>",
        "<img src=\"https://tracker.example.test/open.gif\">",
        "<table><tr><td>Plan</td><td>Price</td></tr>",
        "<tr><td>Starter</td><td>EUR 9</td></tr></table></body></html>\r\n",
    ))?;

    let thread_id = sync_until_subject(
        &sync_service,
        &store,
        account_id,
        "Phase 2b JMAP HTML-only",
        12,
    )?;
    let view = store
        .thread_view(thread_id)?
        .context("the HTML-only thread should load")?;
    let message = &view.messages[0];
    assert_eq!(message.render_mode, MessageRenderMode::SanitizedHtml);
    assert!(
        message.body_text.is_none(),
        "HTML must never be stored as plain text and shown raw"
    );
    let sanitized_html = message
        .sanitized_html
        .as_deref()
        .context("the sanitized HTML should be stored")?;
    assert!(sanitized_html.contains("blocked://tracker.example.test/open.gif"));
    assert!(!sanitized_html.contains("https://"));
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
    assert!(
        !message.rendered_body.contains('<'),
        "no markup reaches the reader: {:?}",
        message.rendered_body
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
                auth: AuthKind::Password,
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

/// Submits a prebuilt RFC 5322 message over plain SMTP.
///
/// `zmail-smtp` only renders text/plain mail, so HTML-only fixtures are
/// sent through a minimal SMTP dialogue instead. Lines in `message` must
/// already end in CRLF and must not start with a dot.
fn submit_raw_harness_message(message: &str) -> Result<()> {
    let stream = TcpStream::connect((MAIL_HARNESS_HOST, MAIL_HARNESS_JMAP_SMTP_PORT))
        .context("the JMAP harness SMTP port should accept connections")?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    expect_smtp_reply(&mut reader, "220")?;
    for (command, code) in [
        ("EHLO localhost".to_owned(), "250"),
        ("MAIL FROM:<news@phase2b.test>".to_owned(), "250"),
        (format!("RCPT TO:<{MAIL_HARNESS_JMAP_ACCOUNT}>"), "250"),
        ("DATA".to_owned(), "354"),
    ] {
        write!(writer, "{command}\r\n")?;
        expect_smtp_reply(&mut reader, code)?;
    }
    writer.write_all(message.as_bytes())?;
    writer.write_all(b".\r\n")?;
    expect_smtp_reply(&mut reader, "250")?;
    writer.write_all(b"QUIT\r\n")?;
    Ok(())
}

/// Reads one (possibly multi-line) SMTP reply and checks its status code.
fn expect_smtp_reply(reader: &mut impl BufRead, code: &str) -> Result<()> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("the SMTP server closed the connection while {code} was expected");
        }
        if !line.starts_with(code) {
            bail!("expected SMTP {code}, got {:?}", line.trim_end());
        }
        // "250-" continues a multi-line reply; "250 " ends it.
        if line.as_bytes().get(3) != Some(&b'-') {
            return Ok(());
        }
    }
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
