//! Live JMAP sync test for the Docker mail harness.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tokio::runtime::Builder;
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewDraft, NewIdentity, OutboxJobState,
    OutgoingAttachment, OutgoingConfig, OutgoingMessage, PendingOp, PendingOpKind, ProtocolKind,
};
use zmail_jmap::{PUSH_PING_INTERVAL, PushEvent, PushPreference, PushTransport};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_smtp::{
    SubmissionConfig, SubmissionSecurity, SubmittedMessage, submit_outgoing_message,
    submit_text_message,
};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{
    DockerMailHarness, MAIL_HARNESS_HOST, MAIL_HARNESS_JMAP_ACCOUNT,
    MAIL_HARNESS_JMAP_ADMIN_ENDPOINT, MAIL_HARNESS_JMAP_ENDPOINT, MAIL_HARNESS_JMAP_PASSWORD,
    MAIL_HARNESS_JMAP_SMTP_PORT, MAIL_HARNESS_SENDER, MAIL_HARNESS_SENDER_PASSWORD,
    live_harness_lock,
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

    // The sync reads the headers a reply needs, and the header-only fetch
    // for mail stored before Zmail kept them reads the same ones.
    let synced = store
        .thread_view(roundtrip_thread_id)?
        .context("the JMAP round-trip thread should load")?;
    let message = &synced.messages[0];
    assert!(message.headers.captured, "the sync reads the reply headers");
    assert!(
        message.headers.message_id.is_some(),
        "the JMAP message's Message-ID is stored: {:?}",
        message.headers
    );
    let target = store
        .sync_target(account_id)?
        .context("the JMAP account should have a sync target")?;
    let fetched = runtime
        .handle()
        .block_on(zmail_jmap::fetch_message_headers(
            &target,
            MAIL_HARNESS_JMAP_PASSWORD,
            std::slice::from_ref(&message.remote_id),
        ))?;
    assert_eq!(
        fetched,
        [(message.remote_id.clone(), message.headers.clone())]
    );

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
                message_id: None,
                in_reply_to: None,
                references: Vec::new(),
                reply_action: None,
            },
        )
        .await
    })
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn event_source_pushes_new_mail() -> Result<()> {
    assert_push_reports_new_mail(PushPreference::EventSource, PushTransport::EventSource)
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn websocket_pushes_new_mail() -> Result<()> {
    assert_push_reports_new_mail(PushPreference::WebSocket, PushTransport::WebSocket)
}

/// Opens a push session over one transport, delivers a message, and
/// asserts the session reports the change long before the keepalive
/// interval could have woken it.
fn assert_push_reports_new_mail(
    preference: PushPreference,
    transport: PushTransport,
) -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = DockerMailHarness::start()?;
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
    let (events_tx, events) = mpsc::channel();
    let watch = runtime.handle().spawn(async move {
        zmail_jmap::watch_mail_changes(&target, &secret, preference, move |event| {
            let _ = events_tx.send(event);
        })
        .await
    });

    let first = events
        .recv_timeout(Duration::from_secs(20))
        .context("the push session should connect")?;
    assert_eq!(first, PushEvent::Connected(transport));
    // Drop anything the server sends straight after connecting, so only a
    // push caused by the delivery below can pass.
    while events.recv_timeout(Duration::from_secs(2)).is_ok() {}

    submit_harness_message(
        "Phase 2b JMAP push",
        "This message should wake the push session.",
    )?;
    let started = Instant::now();
    let pushed = events.recv_timeout(Duration::from_secs(20));
    match pushed {
        Ok(PushEvent::MailChanged) => watch.abort(),
        Ok(other) => bail!("unexpected push event {other:?}"),
        Err(_) => {
            // A late push (for example one released by the 30 s heartbeat)
            // points at buffering between the server and the client.
            let late = events
                .recv_timeout(Duration::from_secs(25))
                .map(|event| format!("{event:?} after {:?}", started.elapsed()));
            println!("late push: {late:?}");
            println!("james logs:\n{}", harness.logs("james", 80));
            println!("nginx logs:\n{}", harness.logs("jmap-proxy", 40));
            // Tell a push session that ended apart from a delivery that
            // never happened, so a failure says which side broke.
            let session = if watch.is_finished() {
                format!("ended with {:?}", runtime.handle().block_on(watch))
            } else {
                watch.abort();
                "was still open".to_owned()
            };
            let delivered =
                sync_until_subject(&sync_service, &store, account_id, "Phase 2b JMAP push", 4)
                    .is_ok();
            bail!(
                "no {transport:?} push arrived within 20s of delivery; the session {session}, \
                 and the message was {}delivered",
                if delivered { "" } else { "not " }
            );
        }
    }
    assert!(
        started.elapsed() < PUSH_PING_INTERVAL,
        "the push is a real state change, not a keepalive"
    );
    Ok(())
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn mail_watcher_applies_pushed_changes_incrementally() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;

    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
    submit_harness_message("Phase 2b JMAP before push", "Synced by the full sync.")?;
    sync_until_subject(
        &sync_service,
        &store,
        account_id,
        "Phase 2b JMAP before push",
        12,
    )?;

    // The poll interval is far beyond the test, so only push can bring the
    // next message in; nothing below calls sync directly.
    sync_service.start_mail_watcher_with_interval(Duration::from_secs(3600))?;
    // Let the connect-time resync settle before delivering.
    thread::sleep(Duration::from_secs(3));
    submit_harness_message(
        "Phase 2b JMAP pushed",
        "Delivered while the watcher listens.",
    )?;
    let thread_id = wait_for_subject(&store, account_id, "Phase 2b JMAP pushed", 30)?;
    // A full sync records the query state; Email/changes clears it. A
    // fallback to the full sync after the push would show up here.
    let checkpoint = sync_checkpoint(&store, account_id)?;
    assert!(
        checkpoint["email_state"].is_string() && checkpoint["email_query_state"].is_null(),
        "pushed mail should arrive through Email/changes, checkpoint {checkpoint}"
    );
    let preview = store
        .message_preview(thread_id)?
        .context("the pushed thread should have a preview")?;
    assert!(
        preview
            .body_preview
            .contains("Delivered while the watcher listens")
    );

    // A wake (sleep or network change) drops the push connection; the
    // watcher must reconnect at once for the next change to arrive.
    sync_service.signal_wake();
    thread::sleep(Duration::from_secs(3));

    // Another client marks the message read; the keyword change arrives as
    // an `updated` id and keeps the stored body.
    let remote_id = store
        .account_mailbox_snapshots(account_id)?
        .into_iter()
        .flat_map(|mailbox| mailbox.threads)
        .flat_map(|thread| thread.messages)
        .find(|message| message.subject == "Phase 2b JMAP pushed")
        .map(|message| message.remote_id)
        .context("the pushed message should be stored")?;
    let target = store
        .sync_target(account_id)?
        .context("the JMAP target should load")?;
    let secret = secrets.get_account_secret(account_id)?;
    let report = runtime.handle().block_on(zmail_jmap::apply_pending_ops(
        &target,
        &secret,
        &[PendingOp {
            id: 1,
            account_id,
            kind: PendingOpKind::MarkRead,
            source_mailbox: "INBOX".to_owned(),
            remote_id,
            target_mailbox: None,
            attempts: 0,
            last_error: None,
        }],
    ))?;
    assert_eq!(report.applied, vec![1], "the other client marks it read");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let thread = store
            .thread_view(thread_id)?
            .context("the pushed thread should still exist")?;
        if thread.messages.iter().all(|message| message.is_read) {
            assert!(
                thread.messages[0].body_text.is_some()
                    || thread.messages[0].sanitized_html.is_some(),
                "a keyword change keeps the stored body"
            );
            break;
        }
        if Instant::now() > deadline {
            bail!("the remote read state was not pushed within 30s");
        }
        thread::sleep(Duration::from_millis(250));
    }

    Ok(())
}

/// Polls the store, without syncing, until the subject appears in INBOX.
fn wait_for_subject(
    store: &LocalStore,
    account_id: i64,
    subject: &str,
    timeout_secs: u64,
) -> Result<i64> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if let Ok(inbox_mailbox_id) = inbox_id(store, account_id) {
            let threads = store.list_threads(inbox_mailbox_id, 0..50)?;
            if let Some(thread) = threads.iter().find(|thread| thread.subject == subject) {
                return Ok(thread.id);
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    bail!("subject {subject:?} was not pushed into the store within {timeout_secs}s")
}

fn sync_checkpoint(store: &LocalStore, account_id: i64) -> Result<serde_json::Value> {
    let checkpoint = store
        .list_accounts()?
        .into_iter()
        .find(|account| account.id == account_id)
        .and_then(|account| account.sync_status.checkpoint)
        .context("the JMAP account should have a checkpoint")?;
    serde_json::from_str(&checkpoint).context("the JMAP checkpoint should be JSON")
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
    assert!(sanitized_html.contains("<img src=\"zmail-blocked:0\">"));
    assert!(!sanitized_html.contains("tracker.example.test"));
    assert!(!sanitized_html.contains("https://"));
    assert!(
        message.has_remote_content,
        "the remote image must be reported"
    );
    assert!(
        message
            .rendered_body
            .contains("Your new dashboard\n\nFaster exports\n\nPlan  Price\nStarter  EUR 9"),
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

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn server_search_finds_jmap_mail_the_local_index_lacks() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    // The word sits past the stored preview excerpt, so once the body is
    // gone only the server can still find it.
    let body = format!(
        "{} Only the end of the body names the storm petrelx.",
        "Filler sentence that keeps the preview busy. ".repeat(20)
    );
    submit_harness_message("Server search JMAP indexed", &body)?;
    sync_until_subject(
        &sync_service,
        &store,
        account_id,
        "Server search JMAP indexed",
        12,
    )?;
    // JMAP syncs bodies, so drop them the way a full disk budget does.
    store.evict_account_blobs(account_id, 0)?;
    assert!(store.search_threads(account_id, "petrelx", 10)?.is_empty());
    assert!(store.search_needs_server(account_id, "petrelx")?);

    // Email/query finds the body text; the hit is marked as a server hit.
    let hits = server_search_until_found(&runtime, &sync_service, &store, account_id, "petrelx")?;
    assert_eq!(hits[0].subject, "Server search JMAP indexed");
    assert!(hits[0].from_server);
    // Non-text clauses are honoured: the message is unread, not read.
    let read = runtime
        .handle()
        .block_on(sync_service.search_server_and_wait(account_id, "petrelx is:read"))?;
    assert_eq!(read.hits, 0);
    assert!(
        store
            .search_threads(account_id, "petrelx is:read", 10)?
            .is_empty()
    );

    // Mail delivered after the last sync: the server finds it and one sync
    // brings it into the store.
    submit_harness_message("Server search JMAP late", "A late fulmarx arrives.")?;
    let hits = server_search_until_found(&runtime, &sync_service, &store, account_id, "fulmarx")?;
    assert_eq!(hits[0].subject, "Server search JMAP late");
    Ok(())
}

/// Runs the server search until the store lists a hit (James indexes new
/// mail asynchronously) and returns the hits.
fn server_search_until_found(
    runtime: &BackgroundRuntime,
    sync_service: &SyncService,
    store: &LocalStore,
    account_id: i64,
    query: &str,
) -> Result<Vec<zmail_core::SearchHit>> {
    for _ in 0..15 {
        let report = runtime
            .handle()
            .block_on(sync_service.search_server_and_wait(account_id, query))?;
        assert!(report.searched, "{query}: {report:?}");
        let hits = store.search_threads(account_id, query, 10)?;
        if !hits.is_empty() {
            return Ok(hits);
        }
        thread::sleep(Duration::from_secs(1));
    }
    bail!("the server search never found {query:?}")
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn a_reply_from_zmail_threads_over_jmap() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    // A message to the account and the harness sender, itself a reply.
    let subject = "Threading over JMAP";
    Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(submit_outgoing_message(
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
                to: vec![
                    MAIL_HARNESS_JMAP_ACCOUNT.to_owned(),
                    MAIL_HARNESS_SENDER.to_owned(),
                ],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject.to_owned(),
                text_body: "Launch on Monday?".to_owned(),
                attachments: Vec::new(),
                message_id: Some("launch-2@phase2b.test".to_owned()),
                in_reply_to: Some("launch-1@phase2b.test".to_owned()),
                references: vec!["launch-1@phase2b.test".to_owned()],
                reply_action: None,
            },
        ))?;
    let thread_id = sync_until_subject(&sync_service, &store, account_id, subject, 12)?;
    let view = store
        .thread_view(thread_id)?
        .context("the thread should load")?;
    let original = &view.messages[0];
    assert_eq!(
        original.headers.message_id.as_deref(),
        Some("launch-2@phase2b.test")
    );

    // Reply to all, as both frontends start it: the harness sender, a
    // local James user, gets a copy.
    let identities = store.list_identities(account_id)?;
    let start = zmail_actions::reply::reply(
        &view.subject,
        &view.mailbox_name,
        original,
        &identities,
        true,
        "today",
    )?;
    assert!(
        start
            .to
            .iter()
            .chain(&start.cc)
            .any(|address| address.contains(MAIL_HARNESS_SENDER)),
        "the harness sender is on the reply: {start:?}"
    );
    let draft_id = store.create_draft(&NewDraft {
        account_id,
        identity_id: identities[0].id,
        to: start.to,
        cc: start.cc,
        bcc: Vec::new(),
        subject: start.subject.clone(),
        body: format!("Monday it is.{}", start.body),
        threading: start.threading,
    })?;
    sync_service.queue_draft_and_send(draft_id)?;
    wait_until_sent(&store, &sync_service, account_id)?;
    let sent_id = store
        .draft_detail(draft_id)?
        .context("the draft should still exist")?
        .message_id
        .context("the sent draft should keep its Message-ID")?;

    // The delivered copy, read from the harness sender's account over JMAP.
    let (sender_store, sender_account, sender_sync) = harness_sender_account(&runtime)?;
    let delivered = sync_until_subject(
        &sender_sync,
        &sender_store,
        sender_account,
        &start.subject,
        60,
    )?;
    let delivered = sender_store
        .thread_view(delivered)?
        .context("the delivered reply should load")?;
    let headers = &delivered
        .messages
        .iter()
        .find(|message| message.subject == start.subject)
        .context("the reply is in the thread")?
        .headers;
    assert_eq!(
        headers.in_reply_to,
        ["launch-2@phase2b.test"],
        "In-Reply-To names the message answered: {headers:?}"
    );
    assert_eq!(
        headers.references,
        ["launch-1@phase2b.test", "launch-2@phase2b.test"],
        "References extends the conversation: {headers:?}"
    );
    assert_eq!(headers.message_id.as_deref(), Some(sent_id.as_str()));
    Ok(())
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn a_forward_over_jmap_arrives_with_the_attachment() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    let subject = "Forwarding over JMAP";
    // No line break: a text part's line breaks travel as CRLF (RFC 2046),
    // which is not what this checks.
    let bytes = b"minutes of the meeting".to_vec();
    submit_harness_attachment_message(subject, "Minutes attached.", "minutes.txt", bytes.clone())?;
    let thread_id = sync_until_subject(&sync_service, &store, account_id, subject, 12)?;
    let view = store
        .thread_view(thread_id)?
        .context("the thread should load")?;

    let mut start = zmail_actions::reply::forward(
        &view.subject,
        &view.mailbox_name,
        &view.messages[0],
        &store.list_identities(account_id)?,
        "today",
    );
    start.to = vec![MAIL_HARNESS_SENDER.to_owned()];
    let started = zmail_actions::MailActions::new(store.clone()).create_started_draft(
        account_id,
        &start,
        &mut |_| {},
    )?;
    assert!(started.missing_attachments.is_empty());
    sync_service.queue_draft_and_send(started.draft_id)?;
    wait_until_sent(&store, &sync_service, account_id)?;

    let (sender_store, sender_account, sender_sync) = harness_sender_account(&runtime)?;
    let delivered = sync_until_subject(
        &sender_sync,
        &sender_store,
        sender_account,
        &start.subject,
        60,
    )?;
    let delivered = sender_store
        .thread_view(delivered)?
        .context("the forward should load")?;
    let message = delivered
        .messages
        .iter()
        .find(|message| message.subject == start.subject)
        .context("the forward is in the thread")?;
    assert_eq!(message.attachments.len(), 1, "{:?}", message.attachments);
    assert_eq!(message.attachments[0].filename, "minutes.txt");
    assert_eq!(
        sender_store
            .attachment_blob(message.attachments[0].id)?
            .read()?,
        bytes,
        "the attachment arrives byte for byte"
    );
    Ok(())
}

/// The harness sender's JMAP account, in a store of its own.
fn harness_sender_account(
    runtime: &BackgroundRuntime,
) -> Result<(Arc<LocalStore>, i64, SyncService)> {
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&NewAccount {
        username: MAIL_HARNESS_SENDER.to_owned(),
        identities: vec![NewIdentity {
            display_name: "Harness Sender".to_owned(),
            email_address: MAIL_HARNESS_SENDER.to_owned(),
            signature: String::new(),
            is_primary: true,
        }],
        ..jmap_account()
    })?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_SENDER_PASSWORD)?;
    let sync = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    Ok((store, account_id, sync))
}

/// Waits until the account's outbox has sent everything, running passes.
fn wait_until_sent(store: &LocalStore, sync_service: &SyncService, account_id: i64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let _ = sync_service.process_outbox_and_wait(Some(account_id));
        let jobs = store.list_outbox_jobs(Some(account_id))?;
        if let Some(failed) = jobs.iter().find(|job| job.state == OutboxJobState::Failed) {
            bail!("delivery failed: {:?}", failed.last_error_message);
        }
        if !jobs.is_empty() && jobs.iter().all(|job| job.state == OutboxJobState::Sent) {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("not sent in time: {jobs:?}");
        }
        thread::sleep(Duration::from_millis(250));
    }
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

/// One JMAP method call against the harness's James as the JMAP account,
/// with the submission capability identities need; returns the method's
/// answer (`[name, arguments, call id]`).
fn jmap_call(method: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
    let client = reqwest::blocking::Client::new();
    let session: serde_json::Value = client
        .get(format!("{MAIL_HARNESS_JMAP_ENDPOINT}/.well-known/jmap"))
        .basic_auth(MAIL_HARNESS_JMAP_ACCOUNT, Some(MAIL_HARNESS_JMAP_PASSWORD))
        .send()?
        .error_for_status()?
        .json()?;
    let api_url = session["apiUrl"]
        .as_str()
        .context("the session names its API URL")?;
    let account_id = session["primaryAccounts"]["urn:ietf:params:jmap:mail"]
        .as_str()
        .context("the session names the mail account")?;
    let mut arguments = arguments;
    arguments["accountId"] = serde_json::json!(account_id);
    let answer: serde_json::Value = client
        .post(api_url)
        .basic_auth(MAIL_HARNESS_JMAP_ACCOUNT, Some(MAIL_HARNESS_JMAP_PASSWORD))
        .json(&serde_json::json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:submission"],
            "methodCalls": [[method, arguments, "c1"]],
        }))
        .send()?
        .error_for_status()?
        .json()?;
    Ok(answer["methodResponses"][0].clone())
}

/// The identity with `email` in the store, and how it stands.
fn synced_identity(
    store: &LocalStore,
    account_id: i64,
    email: &str,
) -> Result<zmail_core::identity_sync::SyncedIdentity> {
    store
        .list_synced_identities(account_id)?
        .into_iter()
        .find(|identity| identity.identity.email_address == email)
        .with_context(|| format!("the store has no identity for {email}"))
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn jmap_identities_import_keep_local_edits_and_push_back() -> Result<()> {
    use zmail_core::identity_sync::{IdentitySyncStatus, IdentityValues};

    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    // An alias the account may send from, and a server identity for it,
    // made on the server as another client would.
    let stamp = chrono::Utc::now().timestamp_millis();
    let alias = format!("identity-{stamp}@localhost");
    add_james_alias(&alias)?;
    let created = jmap_call(
        "Identity/set",
        serde_json::json!({"create": {"n1": {
            "email": alias, "name": "Help desk", "textSignature": "-- Help desk"
        }}}),
    )?;
    let server_id = created[1]["created"]["n1"]["id"]
        .as_str()
        .with_context(|| format!("James creates the identity: {created}"))?
        .to_owned();

    // The sync imports it.
    sync_service.sync_account_and_wait(account_id)?;
    let imported = synced_identity(&store, account_id, &alias)?;
    assert_eq!(imported.local.name, "Help desk");
    assert_eq!(imported.local.signature, "-- Help desk");
    assert_eq!(imported.status, IdentitySyncStatus::InSync);
    eprintln!(
        "the server-set identity of {MAIL_HARNESS_JMAP_ACCOUNT}: {:?}",
        synced_identity(&store, account_id, MAIL_HARNESS_JMAP_ACCOUNT)?.status
    );

    // A local edit survives the next sync, marked as differing.
    let edited = IdentityValues {
        name: "Support".to_owned(),
        reply_to: "Tickets <tickets@localhost>".to_owned(),
        signature: "-- Support, from Zmail".to_owned(),
    };
    store.update_identity(imported.identity.id, &edited)?;
    sync_service.sync_account_and_wait(account_id)?;
    let kept = synced_identity(&store, account_id, &alias)?;
    assert_eq!(kept.local, edited, "the sync keeps the local edit");
    assert_eq!(kept.status, IdentitySyncStatus::ChangedHere);

    // "Update the server", then read it back from James and sync again.
    runtime
        .handle()
        .block_on(sync_service.push_identity_and_wait(imported.identity.id))?;
    assert_eq!(
        synced_identity(&store, account_id, &alias)?.status,
        IdentitySyncStatus::InSync
    );
    let listed = jmap_call("Identity/get", serde_json::json!({"ids": [server_id]}))?;
    let on_server = &listed[1]["list"][0];
    assert_eq!(on_server["name"], "Support", "{listed}");
    assert_eq!(
        on_server["textSignature"], "-- Support, from Zmail",
        "{listed}"
    );
    assert_eq!(
        on_server["replyTo"][0]["email"], "tickets@localhost",
        "{listed}"
    );
    sync_service.sync_account_and_wait(account_id)?;
    let after = synced_identity(&store, account_id, &alias)?;
    assert_eq!(after.local, edited);
    assert_eq!(after.status, IdentitySyncStatus::InSync);

    // Deleted on the server. James then lists the identity it derives
    // from the alias (another id, no name): Zmail relinks to it by address
    // and keeps the local values, differing. A server with nothing else
    // for the address leaves it marked as gone, to create again.
    let destroyed = jmap_call("Identity/set", serde_json::json!({"destroy": [server_id]}))?;
    ensure_destroyed(&destroyed, &server_id)?;
    sync_service.sync_account_and_wait(account_id)?;
    let after_destroy = synced_identity(&store, account_id, &alias)?;
    let link = store
        .identity_link(imported.identity.id)?
        .context("the identity is kept")?;
    assert_eq!(after_destroy.local, edited, "the local values survive");
    if after_destroy.status == IdentitySyncStatus::NotOnServer {
        eprintln!("the server lists nothing else for {alias}: creating it again");
        runtime
            .handle()
            .block_on(sync_service.recreate_identity_and_wait(imported.identity.id))?;
        sync_service.sync_account_and_wait(account_id)?;
        let recreated = synced_identity(&store, account_id, &alias)?;
        assert_eq!(recreated.status, IdentitySyncStatus::InSync);
        assert_eq!(recreated.local, edited);
    } else {
        let listed = jmap_call("Identity/get", serde_json::json!({}))?;
        eprintln!(
            "relinked {alias} to {:?} ({:?}); the server lists {listed}",
            link.server_id, after_destroy.status
        );
        assert_ne!(link.server_id.as_deref(), Some(server_id.as_str()));
        assert!(
            listed[1]["list"]
                .as_array()
                .is_some_and(|list| list.iter().any(|identity| identity["id"].as_str()
                    == link.server_id.as_deref()
                    && identity["email"] == alias.as_str())),
            "relinked to a server identity for {alias}: {listed}"
        );
    }
    Ok(())
}

/// Lets the JMAP account send from `alias` (James's admin API).
fn add_james_alias(alias: &str) -> Result<()> {
    reqwest::blocking::Client::new()
        .put(format!(
            "{MAIL_HARNESS_JMAP_ADMIN_ENDPOINT}/address/aliases/{MAIL_HARNESS_JMAP_ACCOUNT}/sources/{alias}"
        ))
        .send()?
        .error_for_status()
        .context("James adds the alias")?;
    Ok(())
}

/// The server identities James lists for the JMAP account.
fn james_identities() -> Result<Vec<serde_json::Value>> {
    let listed = jmap_call("Identity/get", serde_json::json!({}))?;
    listed[1]["list"]
        .as_array()
        .cloned()
        .with_context(|| format!("James lists the identities: {listed}"))
}

fn tui_key(app: &mut zmail_tui::App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
}

fn tui_type(app: &mut zmail_tui::App, text: &str) {
    for ch in text.chars() {
        tui_key(app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
}

/// Ticks the terminal UI until `done`, so its background requests report
/// back, or fails after `seconds`.
fn tick_until(
    app: &mut zmail_tui::App,
    what: &str,
    seconds: u64,
    mut done: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !done()? {
        if Instant::now() > deadline {
            bail!("timed out waiting for {what}");
        }
        thread::sleep(Duration::from_millis(100));
        app.tick();
    }
    Ok(())
}

/// Lets the terminal UI take in what its background request reported
/// (the store changes before the report arrives), so the next request
/// isn't refused as still busy.
fn settle(app: &mut zmail_tui::App) {
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(100));
        app.tick();
    }
}

/// The identity editor of #366, driven through the terminal UI against
/// James: an edit to a server identity, sent up with "Update the server
/// too?", read back from James; an identity added and created on the
/// server; and that identity removed from Zmail and the server.
#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn the_identity_editor_edits_adds_and_removes_identities_on_james() -> Result<()> {
    use zmail_core::identity_sync::IdentitySyncStatus;
    use zmail_tui::{App, AppOptions, Environment};

    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&jmap_account())?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_JMAP_PASSWORD)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());

    // A server identity for an alias, made as another client would, and
    // imported by a sync.
    let stamp = chrono::Utc::now().timestamp_millis();
    let alias = format!("editor-{stamp}@localhost");
    add_james_alias(&alias)?;
    let created = jmap_call(
        "Identity/set",
        serde_json::json!({"create": {"n1": {
            "email": alias, "name": "Help desk", "textSignature": "-- Help desk"
        }}}),
    )?;
    let server_id = created[1]["created"]["n1"]["id"]
        .as_str()
        .with_context(|| format!("James creates the identity: {created}"))?
        .to_owned();
    sync_service.sync_account_and_wait(account_id)?;
    let identities = store.list_synced_identities(account_id)?;
    let index = identities
        .iter()
        .position(|identity| identity.identity.email_address == alias)
        .context("the sync imports the alias's identity")?;
    let identity_id = identities[index].identity.id;

    let mut app = App::new(AppOptions {
        store: store.clone(),
        sync: Some(sync_service.clone()),
        runtime: Some(runtime.handle().clone()),
        env: Environment {
            color: false,
            unicode: true,
            over_ssh: false,
            macos: false,
        },
        clock: Arc::new(|| chrono::Local::now().fixed_offset()),
        hour_cycle: zmail_core::time::HourCycle::H24,
        version: "0.0.0-test".to_owned(),
        arrivals: None,
        secrets: Some(secrets.clone()),
        network: None,
    });

    // Setup → Accounts → Change, down past the sign-in and the cache
    // budget to the alias's identity, and edit it.
    tui_type(&mut app, "sa");
    tui_type(&mut app, "c");
    for _ in 0..2 + index {
        tui_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    tui_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    tui_key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    tui_type(&mut app, "Support");
    // The address is the server's: Tab goes from Name to Reply-To.
    tui_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    tui_type(&mut app, "Tickets <tickets@localhost>");
    tui_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    tui_key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    tui_type(&mut app, "-- Support, from the terminal");
    tui_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    let saved = synced_identity(&store, account_id, &alias)?;
    assert_eq!(saved.local.name, "Support");
    assert_eq!(saved.status, IdentitySyncStatus::ChangedHere);
    // "Update the server too?" Yes.
    tui_type(&mut app, "y");
    tick_until(&mut app, "the update on James", 60, || {
        Ok(synced_identity(&store, account_id, &alias)?.status == IdentitySyncStatus::InSync)
    })?;
    settle(&mut app);
    let listed = jmap_call("Identity/get", serde_json::json!({"ids": [server_id]}))?;
    let on_server = &listed[1]["list"][0];
    assert_eq!(on_server["name"], "Support", "{listed}");
    assert_eq!(
        on_server["textSignature"], "-- Support, from the terminal",
        "{listed}"
    );
    assert_eq!(
        on_server["replyTo"][0]["email"], "tickets@localhost",
        "{listed}"
    );
    // The next sync agrees.
    sync_service.sync_account_and_wait(account_id)?;
    assert_eq!(
        synced_identity(&store, account_id, &alias)?.status,
        IdentitySyncStatus::InSync
    );
    assert_eq!(
        store
            .identity_link(identity_id)?
            .context("kept")?
            .server_id
            .as_deref(),
        Some(server_id.as_str())
    );

    // Add an identity for a second alias, and create it on James.
    let added = format!("added-{stamp}@localhost");
    add_james_alias(&added)?;
    tui_type(&mut app, "a");
    tui_type(&mut app, "Added in Zmail");
    tui_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    tui_type(&mut app, &added);
    tui_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    tui_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    tui_type(&mut app, "-- Added");
    tui_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    synced_identity(&store, account_id, &added).context("added in Zmail")?;
    // "Create it on the server too?" Yes.
    tui_type(&mut app, "y");
    tick_until(&mut app, "the identity created on James", 60, || {
        Ok(synced_identity(&store, account_id, &added)?.status == IdentitySyncStatus::InSync)
    })?;
    settle(&mut app);
    let added_server_id = store
        .identity_link(synced_identity(&store, account_id, &added)?.identity.id)?
        .and_then(|link| link.server_id)
        .context("linked to the created identity")?;
    let made = james_identities()?
        .into_iter()
        .find(|identity| identity["id"].as_str() == Some(added_server_id.as_str()))
        .context("James lists the created identity")?;
    assert_eq!(made["email"], added.as_str());
    assert_eq!(made["name"], "Added in Zmail");
    assert_eq!(made["textSignature"], "-- Added");

    // Remove it from Zmail and the server: the cursor is still on it.
    tui_type(&mut app, "d");
    tui_type(&mut app, "s");
    tick_until(&mut app, "the identity removed from James", 60, || {
        Ok(synced_identity(&store, account_id, &added).is_err())
    })?;
    assert!(
        james_identities()?
            .iter()
            .all(|identity| identity["id"].as_str() != Some(added_server_id.as_str())),
        "James no longer lists {added_server_id}"
    );
    Ok(())
}

fn ensure_destroyed(answer: &serde_json::Value, server_id: &str) -> Result<()> {
    let destroyed = answer[1]["destroyed"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|id| id == server_id));
    if !destroyed {
        bail!("James did not destroy the identity: {answer}");
    }
    Ok(())
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
