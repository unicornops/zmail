//! Live tests for Proton accounts in the sync runtime (`zmail-sync` with
//! its `proton` feature) against the Proton harness.
//!
//! An account is signed in the way onboarding will do it (SRP login, salted
//! key password, the three Proton secret slots) and then synced through
//! `SyncService`, which resumes the session from the keychain.

use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewDraft, NewIdentity, OutboxJobState, ProtocolKind,
};
use zmail_proton::{ClientConfig, store_new_session};
use zmail_proton_api::{AppVersion, CardType, EncryptionScheme, Manager};
use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{
    PROTON_HARNESS_ALIAS_ADDRESS, PROTON_HARNESS_PRIMARY_ADDRESS, ProtonHarness, live_harness_lock,
};

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

/// Subjects of the threads in one mailbox.
fn subjects(store: &LocalStore, account_id: i64, mailbox: &str) -> Result<Vec<String>> {
    let id = store
        .list_mailboxes(account_id)?
        .into_iter()
        .find(|summary| summary.name == mailbox)
        .map(|summary| summary.id)
        .with_context(|| format!("no {mailbox} mailbox"))?;
    let mut subjects: Vec<String> = store
        .list_threads(id, 0..100)?
        .into_iter()
        .map(|thread| thread.subject)
        .collect();
    subjects.sort();
    Ok(subjects)
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn mirrors_labels_as_mailboxes_and_messages_by_metadata() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let state = harness.state().clone();
    ensure!(
        state.messages.len() == 6,
        "unexpected fixtures: {:?}",
        state.messages
    );

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;

    // Mailboxes as Bridge names them, with roles for the system labels.
    let mailboxes = store.list_mailboxes(account_id)?;
    let role_of = |name: &str| {
        mailboxes
            .iter()
            .find(|mailbox| mailbox.name == name)
            .map(|mailbox| mailbox.role.map(|role| role.as_str()))
    };
    for (name, role) in [
        ("INBOX", Some("inbox")),
        ("Drafts", Some("drafts")),
        ("Sent", Some("sent")),
        ("Archive", Some("archive")),
        ("Spam", Some("junk")),
        ("Trash", Some("trash")),
        ("All Mail", None),
        ("Folders/Work", None),
        ("Folders/Receipts", None),
        ("Labels/Family", None),
        ("Labels/Follow up", None),
    ] {
        ensure!(role_of(name) == Some(role), "{name}: {:?}", role_of(name));
    }
    ensure!(
        role_of("Starred").is_none(),
        "Starred is the flagged state, not a mailbox"
    );
    let work = mailboxes
        .iter()
        .find(|mailbox| mailbox.name == "Folders/Work")
        .context("no Folders/Work")?;
    ensure!(work.parent.as_deref() == Some("Folders"));
    ensure!(work.delimiter.as_deref() == Some("/"));

    // Each message sits in every mailbox of its labels; All Mail has all.
    ensure!(
        subjects(&store, account_id, "INBOX")?
            == [
                "Dinner on Sunday",
                "Re: Welcome to the harness",
                "To the alias",
                "Welcome to the harness"
            ],
        "INBOX: {:?}",
        subjects(&store, account_id, "INBOX")?
    );
    ensure!(subjects(&store, account_id, "Folders/Work")? == ["Quarterly report"]);
    ensure!(subjects(&store, account_id, "Labels/Family")? == ["Dinner on Sunday"]);
    ensure!(subjects(&store, account_id, "Archive")? == ["Old receipt"]);
    ensure!(subjects(&store, account_id, "Folders/Receipts")?.is_empty());
    ensure!(subjects(&store, account_id, "All Mail")?.len() == 6);

    // Read, unread and starred come from the metadata.
    let inbox_id = mailboxes
        .iter()
        .find(|mailbox| mailbox.name == "INBOX")
        .map(|mailbox| mailbox.id)
        .context("no INBOX")?;
    let threads = store.list_threads(inbox_id, 0..100)?;
    let thread = |subject: &str| {
        threads
            .iter()
            .find(|thread| thread.subject == subject)
            .with_context(|| format!("no thread {subject}"))
    };
    let welcome = thread("Welcome to the harness")?;
    ensure!(
        welcome.unread_count == 1 && welcome.is_flagged,
        "{welcome:?}"
    );
    let reply = thread("Re: Welcome to the harness")?;
    ensure!(reply.unread_count == 0 && !reply.is_flagged, "{reply:?}");
    ensure!(thread("Dinner on Sunday")?.unread_count == 1);
    ensure!(
        thread("Welcome to the harness")?
            .latest_sender
            .contains("alice@example.com"),
        "sender: {}",
        welcome.latest_sender
    );
    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn hydrates_decrypted_bodies_and_attachments_on_open() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;

    let mailbox_id = |name: &str| -> Result<i64> {
        store
            .list_mailboxes(account_id)?
            .into_iter()
            .find(|mailbox| mailbox.name == name)
            .map(|mailbox| mailbox.id)
            .with_context(|| format!("no {name} mailbox"))
    };
    let thread_id = |mailbox: &str, subject: &str| -> Result<i64> {
        store
            .list_threads(mailbox_id(mailbox)?, 0..100)?
            .into_iter()
            .find(|thread| thread.subject == subject)
            .map(|thread| thread.id)
            .with_context(|| format!("no {subject} in {mailbox}"))
    };
    let hydrate = |thread: i64| {
        runtime
            .handle()
            .block_on(sync_service.hydrate_thread_body_and_wait(account_id, thread))
    };

    // Metadata sync stores no body; opening the thread decrypts it.
    let welcome = thread_id("INBOX", "Welcome to the harness")?;
    ensure!(
        store.thread_view(welcome)?.context("thread")?.messages[0]
            .body_text
            .is_none()
    );
    ensure!(hydrate(welcome)? == 1, "the welcome body should hydrate");
    let view = store.thread_view(welcome)?.context("thread")?;
    let body = view.messages[0].body_text.clone().unwrap_or_default();
    ensure!(
        body.contains("Hello from the Proton harness."),
        "body: {body:?}"
    );
    // Hydrated once, the body is not fetched again.
    ensure!(hydrate(welcome)? == 0, "a hydrated body was fetched again");

    // An attachment is decrypted with its key packets and stored.
    let report = thread_id("Folders/Work", "Quarterly report")?;
    ensure!(hydrate(report)? == 1);
    let view = store.thread_view(report)?.context("thread")?;
    let message = &view.messages[0];
    ensure!(
        message
            .body_text
            .as_deref()
            .is_some_and(|text| text.contains("The report is attached.")),
        "body: {:?}",
        message.body_text
    );
    ensure!(message.attachments.len() == 1, "{:?}", message.attachments);
    ensure!(message.attachments[0].filename == "report.csv");
    let destination =
        std::env::temp_dir().join(format!("zmail-proton-attachment-{}", std::process::id()));
    let saved = store.save_attachment(message.attachments[0].id, &destination)?;
    let bytes = std::fs::read(&saved)?;
    let _ = std::fs::remove_dir_all(&destination);
    ensure!(
        bytes == b"quarter,total\n1,10\n",
        "attachment bytes: {bytes:?}"
    );

    // HTML goes through the sanitizer, with remote content blocked.
    let dinner = thread_id("Labels/Family", "Dinner on Sunday")?;
    ensure!(hydrate(dinner)? == 1);
    let view = store.thread_view(dinner)?.context("thread")?;
    let message = &view.messages[0];
    ensure!(
        message.has_remote_content,
        "the tracking pixel is remote content"
    );
    let html = message.sanitized_html.clone().unwrap_or_default();
    ensure!(
        html.contains("six") && !html.contains("https://tracker"),
        "html: {html:?}"
    );
    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn applies_server_changes_from_the_event_stream() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let state = harness.state().clone();
    let message_id = |subject: &str| -> Result<String> {
        state
            .messages
            .iter()
            .find(|message| message.subject == subject)
            .map(|message| message.id.clone())
            .with_context(|| format!("no fixture {subject}"))
    };
    let family_id = state
        .labels
        .iter()
        .find(|label| label.name == "Family")
        .map(|label| label.id.clone())
        .context("no Family label")?;

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;
    let first_checkpoint = store.list_accounts()?[0].sync_status.checkpoint.clone();

    // Change things on the server, as another Proton client would.
    let manager = Manager::new(harness_config(&harness)?)?;
    runtime.handle().block_on(async {
        let (client, _) = manager
            .login(harness.username(), harness.password())
            .await?;
        let dinner = vec![message_id("Dinner on Sunday")?];
        client.mark_messages_read(&dinner).await?;
        client.unlabel_messages(&dinner, &family_id).await?;
        client
            .label_messages(&[message_id("Re: Welcome to the harness")?], "10")
            .await?;
        anyhow::Ok(())
    })?;

    sync_service.sync_account_and_wait(account_id)?;
    let status = store.list_accounts()?[0].sync_status.clone();
    ensure!(
        status.detail.contains("applied"),
        "expected an incremental sync, got {:?}",
        status.detail
    );
    ensure!(
        status.checkpoint != first_checkpoint,
        "the checkpoint did not move"
    );
    ensure!(subjects(&store, account_id, "Labels/Family")?.is_empty());
    let inbox_id = store
        .list_mailboxes(account_id)?
        .into_iter()
        .find(|mailbox| mailbox.name == "INBOX")
        .map(|mailbox| mailbox.id)
        .context("no INBOX")?;
    let threads = store.list_threads(inbox_id, 0..100)?;
    let thread = |subject: &str| {
        threads
            .iter()
            .find(|thread| thread.subject == subject)
            .with_context(|| format!("no thread {subject}"))
    };
    ensure!(thread("Dinner on Sunday")?.unread_count == 0);
    ensure!(thread("Re: Welcome to the harness")?.is_flagged);
    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn writes_local_mailbox_changes_back_with_bridges_rules() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let state = harness.state().clone();
    let message_id = |subject: &str| -> Result<String> {
        state
            .messages
            .iter()
            .find(|message| message.subject == subject)
            .map(|message| message.id.clone())
            .with_context(|| format!("no fixture {subject}"))
    };
    let label_id = |name: &str| -> Result<String> {
        state
            .labels
            .iter()
            .find(|label| label.name == name)
            .map(|label| label.id.clone())
            .with_context(|| format!("no label {name}"))
    };

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;

    let mailbox_id = |name: &str| -> Result<i64> {
        store
            .list_mailboxes(account_id)?
            .into_iter()
            .find(|mailbox| mailbox.name == name)
            .map(|mailbox| mailbox.id)
            .with_context(|| format!("no {name} mailbox"))
    };
    let thread_id = |mailbox: &str, subject: &str| -> Result<i64> {
        store
            .list_threads(mailbox_id(mailbox)?, 0..100)?
            .into_iter()
            .find(|thread| thread.subject == subject)
            .map(|thread| thread.id)
            .with_context(|| format!("no {subject} in {mailbox}"))
    };

    // Local changes queue write-back operations.
    store.set_thread_read(thread_id("INBOX", "Welcome to the harness")?, true)?;
    store.set_thread_flagged(thread_id("INBOX", "To the alias")?, true)?;
    store.move_thread(
        thread_id("Folders/Work", "Quarterly report")?,
        mailbox_id("Folders/Receipts")?,
    )?;
    // Deleting moves to Trash; deleting from Trash destroys.
    store.delete_thread(thread_id("Archive", "Old receipt")?)?;
    store.delete_thread(thread_id("Trash", "Old receipt")?)?;
    ensure!(!store.list_pending_ops(account_id)?.is_empty());

    sync_service.sync_account_and_wait(account_id)?;
    ensure!(
        store.list_pending_ops(account_id)?.is_empty(),
        "operations left: {:?}",
        store.list_pending_ops(account_id)?
    );

    // The server now has every change.
    let manager = Manager::new(harness_config(&harness)?)?;
    let server = runtime.handle().block_on(async {
        let (client, _) = manager
            .login(harness.username(), harness.password())
            .await?;
        let ids = state.messages.iter().map(|m| m.id.clone()).collect();
        let filter = zmail_proton_api::MessageFilter {
            id: ids,
            ..zmail_proton_api::MessageFilter::default()
        };
        anyhow::Ok(client.get_message_metadata_page(0, 50, &filter).await?)
    })?;
    let meta = |subject: &str| -> Result<&zmail_proton_api::MessageMetadata> {
        let id = message_id(subject)?;
        server
            .iter()
            .find(|meta| meta.id == id)
            .with_context(|| format!("{subject} is gone from the server"))
    };
    ensure!(meta("Welcome to the harness")?.seen());
    ensure!(meta("To the alias")?.starred());
    let report = meta("Quarterly report")?;
    ensure!(
        report.label_ids.contains(&label_id("Receipts")?)
            && !report.label_ids.contains(&label_id("Work")?),
        "labels: {:?}",
        report.label_ids
    );
    ensure!(
        meta("Old receipt").is_err(),
        "a message destroyed from Trash should be deleted"
    );
    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn sends_through_the_outbox_with_internal_and_clear_packages() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;

    // The sync made the Proton addresses the account's identities.
    let identity = store
        .list_identities(account_id)?
        .into_iter()
        .find(|identity| identity.email_address == PROTON_HARNESS_PRIMARY_ADDRESS)
        .context("no identity for the primary address")?;
    let draft = |subject: &str| NewDraft {
        account_id,
        identity_id: identity.id,
        // The alias is a Proton address (an internal package); the Bcc
        // is outside Proton (a clear package).
        to: vec![PROTON_HARNESS_ALIAS_ADDRESS.to_owned()],
        cc: Vec::new(),
        bcc: vec!["someone@example.com".to_owned()],
        subject: subject.to_owned(),
        body: "Hello from Zmail over Proton.\nSecond line.".to_owned(),
    };

    // Undo send: a job inside its undo window is not sent, and cancelling
    // takes it back.
    let undone = store.create_draft(&draft("Never sent"))?;
    let job = store.queue_draft_after(undone, std::time::Duration::from_secs(3600))?;
    ensure!(sync_service.process_outbox_and_wait(Some(account_id))? == 0);
    ensure!(
        store.cancel_queued_outbox_job(job)?,
        "the job was not cancelled"
    );

    // A message with an attachment goes out.
    let draft_id = store.create_draft(&draft("Sent from Zmail"))?;
    let scratch = std::env::temp_dir().join(format!("zmail-proton-send-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let notes = scratch.join("notes.txt");
    std::fs::write(&notes, b"line one\nline two\n")?;
    store.add_draft_attachment(draft_id, &notes)?;
    store.queue_draft(draft_id)?;
    let processed = sync_service.process_outbox_and_wait(Some(account_id))?;
    let jobs = store.list_outbox_jobs(Some(account_id))?;
    let sent = jobs
        .iter()
        .find(|job| job.draft_id == draft_id)
        .context("no outbox job")?;
    ensure!(
        processed == 1 && sent.state == OutboxJobState::Sent,
        "job {sent:?}; server log:\n{}",
        harness.logs()
    );

    // Proton files it in Sent, and delivers the alias its copy, which
    // decrypts with the alias's key.
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;
    ensure!(subjects(&store, account_id, "Sent")?.contains(&"Sent from Zmail".to_owned()));
    ensure!(
        !subjects(&store, account_id, "Sent")?.contains(&"Never sent".to_owned()),
        "the cancelled message was sent"
    );
    let inbox = store
        .list_mailboxes(account_id)?
        .into_iter()
        .find(|mailbox| mailbox.name == "INBOX")
        .context("no INBOX")?;
    let thread = store
        .list_threads(inbox.id, 0..100)?
        .into_iter()
        .find(|thread| thread.subject == "Sent from Zmail")
        .context("the alias did not receive the message")?;
    let hydrated = runtime
        .handle()
        .block_on(sync_service.hydrate_thread_body_and_wait(account_id, thread.id))?;
    ensure!(hydrated == 1);
    let view = store.thread_view(thread.id)?.context("thread")?;
    let message = view
        .messages
        .iter()
        .find(|message| message.attachments.len() == 1)
        .context("no message with the attachment")?;
    let body = message.body_text.clone().unwrap_or_default();
    ensure!(
        body.contains("Hello from Zmail over Proton.") && body.contains("Second line."),
        "body: {body:?}"
    );
    ensure!(message.attachments[0].filename == "notes.txt");
    let saved = store.save_attachment(message.attachments[0].id, scratch.join("saved"))?;
    let bytes = std::fs::read(&saved)?;
    let _ = std::fs::remove_dir_all(&scratch);
    ensure!(
        bytes == b"line one\nline two\n",
        "attachment bytes: {bytes:?}"
    );
    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn sends_to_outside_recipients_by_their_contact_settings() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;

    // The seeded contacts' signed cards read back with the user keys.
    let manager = Manager::new(harness_config(&harness)?)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let (client, _) = manager
                .login(harness.username(), harness.password())
                .await?;
            let user = client.get_user().await?;
            let addresses = client.get_addresses().await?;
            let key_pass = client.salted_key_pass(&user, harness.password()).await?;
            let keys = zmail_proton_api::unlock(&user, &addresses, &key_pass)?;
            let settings = |email: &'static str| {
                let client = &client;
                let keys = &keys;
                async move {
                    let emails = client.get_all_contact_emails(email).await?;
                    let contact = emails
                        .iter()
                        .find(|contact| contact.email == email)
                        .with_context(|| format!("no contact {email}"))?;
                    let contact = client.get_contact(&contact.contact_id).await?;
                    anyhow::Ok(contact.get_settings(&keys.user, email, CardType::SIGNED)?)
                }
            };
            let pgp_mime = settings("pgp-mime@example.com").await?;
            ensure!(
                pgp_mime.encrypt == Some(true)
                    && pgp_mime.scheme == Some(EncryptionScheme::PGP_MIME)
                    && pgp_mime.keys.len() == 1,
                "{pgp_mime:?}"
            );
            let signed = settings("signed@example.com").await?;
            ensure!(
                signed.sign == Some(true) && signed.encrypt.is_none(),
                "{signed:?}"
            );
            anyhow::Ok(())
        })
        .with_context(|| format!("contacts; server log:\n{}", harness.logs()))?;

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;
    let identity = store
        .list_identities(account_id)?
        .into_iter()
        .find(|identity| identity.email_address == PROTON_HARNESS_PRIMARY_ADDRESS)
        .context("no identity for the primary address")?;

    // One message to a Proton address (internal), a PGP/MIME contact and
    // a signed one (one MIME package), and a PGP/Inline contact (a text
    // package encrypted to the contact's key).
    let draft_id = store.create_draft(&NewDraft {
        account_id,
        identity_id: identity.id,
        to: vec![
            PROTON_HARNESS_ALIAS_ADDRESS.to_owned(),
            "pgp-mime@example.com".to_owned(),
        ],
        cc: vec!["signed@example.com".to_owned()],
        bcc: vec!["pgp-inline@example.com".to_owned()],
        subject: "To the outside".to_owned(),
        body: "Encrypted where the contact asks for it.".to_owned(),
    })?;
    store.queue_draft(draft_id)?;
    sync_service.process_outbox_and_wait(Some(account_id))?;
    let job = store
        .list_outbox_jobs(Some(account_id))?
        .into_iter()
        .find(|job| job.draft_id == draft_id)
        .context("no outbox job")?;
    ensure!(
        job.state == OutboxJobState::Sent,
        "job {job:?}; server log:\n{}",
        harness.logs()
    );

    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;
    ensure!(subjects(&store, account_id, "Sent")?.contains(&"To the outside".to_owned()));
    ensure!(subjects(&store, account_id, "INBOX")?.contains(&"To the outside".to_owned()));
    Ok(())
}
