//! Live tests for Proton accounts in the sync runtime (`zmail-sync` with
//! its `proton` feature) against the Proton harness.
//!
//! An account is signed in the way onboarding will do it (SRP login, salted
//! key password, the three Proton secret slots) and then synced through
//! `SyncService`, which resumes the session from the keychain.

use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewDraft, NewIdentity, OutboxJobState, ProtocolKind,
};
use zmail_proton::{ClientConfig, SignInStep, store_new_session};
use zmail_proton_api::{AppVersion, CardType, EncryptionScheme, Manager, MessageFilter};
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
    // Decrypted Proton mail never reaches the disk in plaintext (ADR 0005):
    // the body and the attachment are in the encrypted store and its
    // encrypted files, under opaque names.
    let data_dir = store.path().parent().context("the store's directory")?;
    for (what, needle) in [
        ("the body", &b"The report is attached."[..]),
        ("the attachment", &b"quarter,total"[..]),
        ("the attachment's name", &b"report.csv"[..]),
    ] {
        if let Some(file) = file_containing(data_dir, needle)? {
            bail!("{} holds {what} in plaintext", file.display());
        }
    }

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
        threading: Default::default(),
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
    ensure!(
        message.headers.captured && message.headers.message_id.is_some(),
        "hydration reads the reply headers: {:?}",
        message.headers
    );
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
        threading: Default::default(),
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

/// The draft a request sends, if it is a send
/// (`POST /mail/v4/messages/{draft ID}`).
fn sent_draft_id(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/mail/v4/messages/")?;
    (!id.contains('/')
        && ![
            "import",
            "read",
            "unread",
            "delete",
            "label",
            "unlabel",
            "undoactions",
        ]
        .contains(&id))
    .then_some(id)
}

/// How many messages the harness was asked to send
/// (`POST /mail/v4/messages/{draft ID}`) in its log.
fn send_requests(log: &str) -> Vec<String> {
    log.lines()
        .filter(|line| line.contains("| POST"))
        .filter_map(|line| sent_draft_id(line.split('"').nth(1)?).map(str::to_owned))
        .collect()
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn interrupted_sends_are_settled_from_what_proton_shows() -> Result<()> {
    use std::time::Duration;

    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    // The process that holds the sync lock, as the desktop app does.
    let sync_service =
        SyncService::for_command(runtime.handle().clone(), store.clone(), secrets.clone())?;
    ensure!(sync_service.runs_sync());
    sync_service.configure_proton(harness_config(&harness)?)?;
    // Nothing counts as interrupted while each case is set up.
    sync_service.set_interrupted_send_after(Duration::from_secs(3600));
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;
    let identity_id = store
        .list_identities(account_id)?
        .into_iter()
        .find(|identity| identity.email_address == PROTON_HARNESS_PRIMARY_ADDRESS)
        .context("no identity for the primary address")?
        .id;

    // The stopped process's view of the account: a sign-in of its own,
    // so the two never refresh one session's tokens from under each other.
    let stopped_secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, stopped_secrets.as_ref(), account_id)?;
    let sessions = zmail_proton::ProtonSessions::new(harness_config(&harness)?, stopped_secrets)?;
    let session = runtime.handle().block_on(sessions.session(account_id))?;

    // A message to the alias, claimed for sending as a pass claims it;
    // the process then stops without recording anything.
    let scratch = std::env::temp_dir().join(format!("zmail-proton-stop-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let claim = |subject: &str, attach: bool| -> Result<zmail_core::PendingOutboxJob> {
        let draft_id = store.create_draft(&NewDraft {
            account_id,
            identity_id,
            to: vec![PROTON_HARNESS_ALIAS_ADDRESS.to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_owned(),
            body: format!("{subject}, sent once."),
            threading: Default::default(),
        })?;
        if attach {
            let notes = scratch.join("notes.txt");
            std::fs::write(&notes, b"line one\n")?;
            store.add_draft_attachment(draft_id, &notes)?;
        }
        store.queue_draft(draft_id)?;
        store
            .claim_due_outbox_job(Some(account_id))?
            .context("the job is due")
    };
    // Zmail starts again: the holder settles what was interrupted.
    let settle = || -> Result<()> {
        sync_service.set_interrupted_send_after(Duration::ZERO);
        sync_service
            .process_outbox_and_wait(Some(account_id))
            .with_context(|| format!("server log:\n{}", harness.logs()))?;
        sync_service.set_interrupted_send_after(Duration::from_secs(3600));
        Ok(())
    };
    let state = |job_id: i64| -> Result<zmail_core::OutboxJobSummary> {
        store
            .list_outbox_jobs(Some(account_id))?
            .into_iter()
            .find(|job| job.id == job_id)
            .context("the job is kept")
    };
    // What the account holds with the message's Message-ID: sent copies,
    // received copies (the alias's) and drafts.
    let held = |message_id: &str| -> Result<(usize, usize, usize, i64)> {
        let metadata = runtime
            .handle()
            .block_on(session.client().get_message_metadata(&MessageFilter {
                external_id: message_id.to_owned(),
                ..MessageFilter::default()
            }))?;
        let sent = metadata
            .iter()
            .filter(|message| message.flags.has(zmail_proton_api::MessageFlags::SENT))
            .collect::<Vec<_>>();
        let received = metadata
            .iter()
            .filter(|message| message.flags.has(zmail_proton_api::MessageFlags::RECEIVED))
            .count();
        let drafts = metadata.iter().filter(|message| message.is_draft()).count();
        Ok((
            sent.len(),
            received,
            drafts,
            sent.first().map_or(-1, |message| message.num_attachments),
        ))
    };

    // 1. It had sent the message when it stopped: the job is Sent, and
    // nothing is sent again.
    let job = claim("Stopped after sending", false)?;
    runtime
        .handle()
        .block_on(zmail_proton::submit_outgoing_message(
            &session,
            &job.message,
            &[],
        ))
        .with_context(|| format!("the first send; server log:\n{}", harness.logs()))?;
    let message_id = job.message.message_id.clone().context("a Message-ID")?;
    let before = send_requests(&harness.logs()).len();
    ensure!(state(job.job_id)?.state == OutboxJobState::Sending);
    settle()?;
    let after = state(job.job_id)?;
    ensure!(after.state == OutboxJobState::Sent, "{after:?}");
    ensure!(after.attempt_count == 1, "{after:?}");
    ensure!(
        send_requests(&harness.logs()).len() == before,
        "sent again; server log:\n{}",
        harness.logs()
    );
    let (sent, received, drafts, _) = held(&message_id)?;
    ensure!(
        (sent, received, drafts) == (1, 1, 0),
        "{sent} sent, {received} received, {drafts} drafts"
    );

    // 2. It had created the draft, and stopped before sending it: that
    // draft is sent, once, with the attachment the stopped process never
    // uploaded.
    let job = claim("Stopped with a draft", true)?;
    let message_id = job.message.message_id.clone().context("a Message-ID")?;
    let draft_id = runtime.handle().block_on(async {
        let client = session.client();
        let user = client.get_user().await?;
        let addresses = client.get_addresses().await?;
        let key_pass = client.salted_key_pass(&user, harness.password()).await?;
        let keys = zmail_proton_api::unlock(&user, &addresses, &key_pass)?;
        let address = addresses
            .iter()
            .find(|address| address.email == PROTON_HARNESS_PRIMARY_ADDRESS)
            .context("the primary address")?;
        let key = keys
            .addresses
            .get(&address.id)
            .and_then(|ring| ring.first())
            .context("the address key")?;
        let draft = client
            .create_draft(
                key,
                zmail_proton_api::CreateDraftReq {
                    message: zmail_proton_api::DraftTemplate {
                        subject: job.message.subject.clone(),
                        body: job.message.text_body.clone(),
                        mime_type: "text/plain".to_owned(),
                        sender: zmail_proton_api::EmailAddress {
                            name: String::new(),
                            address: PROTON_HARNESS_PRIMARY_ADDRESS.to_owned(),
                        },
                        to_list: vec![zmail_proton_api::EmailAddress {
                            name: String::new(),
                            address: PROTON_HARNESS_ALIAS_ADDRESS.to_owned(),
                        }],
                        external_id: message_id.clone(),
                        ..zmail_proton_api::DraftTemplate::default()
                    },
                    ..zmail_proton_api::CreateDraftReq::default()
                },
            )
            .await?;
        anyhow::Ok(draft.metadata.id)
    })?;
    let before = send_requests(&harness.logs()).len();
    settle()?;
    let after = state(job.job_id)?;
    ensure!(
        after.state == OutboxJobState::Sent,
        "{after:?}; server log:\n{}",
        harness.logs()
    );
    let sends = send_requests(&harness.logs());
    ensure!(
        sends[before..] == [draft_id.clone()],
        "sends {:?}, the draft {draft_id}",
        &sends[before..]
    );
    let (sent, received, drafts, attachments) = held(&message_id)?;
    ensure!(
        (sent, received, drafts, attachments) == (1, 1, 0, 1),
        "{sent} sent, {received} received, {drafts} drafts, {attachments} attachments"
    );

    // 3. It stopped before the draft: no trace, so it goes back in the
    // queue and is sent once.
    let job = claim("Stopped before the draft", false)?;
    let message_id = job.message.message_id.clone().context("a Message-ID")?;
    let before = send_requests(&harness.logs()).len();
    settle()?;
    let after = state(job.job_id)?;
    ensure!(
        after.state == OutboxJobState::Sent,
        "{after:?}; server log:\n{}",
        harness.logs()
    );
    ensure!(
        after.attempt_count == 2,
        "the interrupted attempt counts: {after:?}"
    );
    ensure!(send_requests(&harness.logs()).len() == before + 1);
    let (sent, received, drafts, _) = held(&message_id)?;
    ensure!(
        (sent, received, drafts) == (1, 1, 0),
        "{sent} sent, {received} received, {drafts} drafts"
    );
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

/// A TLS proxy in front of the Proton harness, for sends that go wrong on
/// the way. It serves the harness's own certificate, so the client runs its
/// real TLS path, forwards every request to the harness, and does to the
/// requests a test picks what a dropped connection or a struggling server
/// would.
mod proxy {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use anyhow::{Context, Result, anyhow};
    use tokio::{
        io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
        net::TcpListener,
        task::JoinHandle,
    };
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            crypto::aws_lc_rs,
            pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
        },
    };
    use zmail_test_support::ProtonHarness;

    /// The requests a fault is for.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Route {
        /// Sending a draft (`POST /mail/v4/messages/{draft ID}`).
        Send,
        /// Looking messages up by ExternalID (the metadata listing and its
        /// count).
        Lookup,
    }

    /// What the proxy does to the next request on a route.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Fault {
        /// Forwards it, waits for the harness's answer (the harness has
        /// done what was asked), then closes the connection without passing
        /// the answer on: the client sees the connection drop.
        DropAnswer,
        /// Answers it with this status and Proton code without forwarding
        /// it: the harness never sees it.
        Refuse(u16, i64),
    }

    /// A request as the proxy received it.
    #[derive(Debug, Clone)]
    pub struct Seen {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Seen {
        /// The draft this request sends, if it is a send.
        pub fn sent_draft(&self) -> Option<&str> {
            if self.method != "POST" {
                return None;
            }
            super::sent_draft_id(&self.path)
        }

        /// The ExternalID this request looks messages up by, if it does.
        pub fn lookup(&self) -> Option<String> {
            let listing = self.method == "POST"
                && self.path == "/mail/v4/messages"
                && self.headers.iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("x-http-method-override") && value == "GET"
                });
            if !listing {
                return None;
            }
            let body: serde_json::Value = serde_json::from_slice(&self.body).ok()?;
            body.get("ExternalID")?.as_str().map(str::to_owned)
        }

        /// The ExternalID of the draft this request creates, if it does.
        pub fn created_draft(&self) -> Option<String> {
            let creates = self.method == "POST"
                && self.path == "/mail/v4/messages"
                && !self
                    .headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("x-http-method-override"));
            if !creates {
                return None;
            }
            let body: serde_json::Value = serde_json::from_slice(&self.body).ok()?;
            body.pointer("/Message/ExternalID")?
                .as_str()
                .map(str::to_owned)
        }

        fn route(&self) -> Option<Route> {
            if self.sent_draft().is_some() {
                Some(Route::Send)
            } else if self.lookup().is_some() {
                Some(Route::Lookup)
            } else {
                None
            }
        }
    }

    type Faults = Arc<Mutex<VecDeque<(Route, Fault)>>>;
    type Log = Arc<Mutex<Vec<Seen>>>;

    pub struct Proxy {
        url: String,
        faults: Faults,
        seen: Log,
        task: JoinHandle<()>,
    }

    impl Proxy {
        /// Starts the proxy on a free loopback port, on the current tokio
        /// runtime.
        pub async fn start(harness: &ProtonHarness) -> Result<Self> {
            let (cert, key) = harness.server_certificate_paths();
            let chain = CertificateDer::pem_file_iter(&cert)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .map_err(|error| anyhow!("{}: {error:?}", cert.display()))?;
            let key = PrivateKeyDer::from_pem_file(&key)
                .map_err(|error| anyhow!("{}: {error:?}", key.display()))?;
            let mut config =
                ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                    .with_safe_default_protocol_versions()?
                    .with_no_client_auth()
                    .with_single_cert(chain, key)?;
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            let acceptor = TlsAcceptor::from(Arc::new(config));
            let upstream = reqwest::Client::builder()
                .tls_built_in_root_certs(false)
                .add_root_certificate(reqwest::Certificate::from_pem(&harness.ca_pem()?)?)
                .no_proxy()
                .http1_only()
                .build()
                .context("failed to build the proxy's client")?;
            let base = harness.base_url().trim_end_matches('/').to_owned();

            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let url = format!("https://{}", listener.local_addr()?);
            let faults = Faults::default();
            let seen = Log::default();
            let (task_faults, task_seen) = (faults.clone(), seen.clone());
            let task = tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let (acceptor, upstream, base) =
                        (acceptor.clone(), upstream.clone(), base.clone());
                    let (faults, seen) = (task_faults.clone(), task_seen.clone());
                    tokio::spawn(async move {
                        if let Ok(tls) = acceptor.accept(tcp).await {
                            serve(tls, &upstream, &base, &faults, &seen).await;
                        }
                    });
                }
            });
            Ok(Self {
                url,
                faults,
                seen,
                task,
            })
        }

        /// The base URL clients use instead of the harness's.
        pub fn url(&self) -> &str {
            &self.url
        }

        /// Does `fault` to the next request on `route`.
        pub fn arm(&self, route: Route, fault: Fault) {
            self.faults
                .lock()
                .expect("the faults")
                .push_back((route, fault));
        }

        /// Whether every armed fault has been used.
        pub fn disarmed(&self) -> bool {
            self.faults.lock().expect("the faults").is_empty()
        }

        /// Every request so far, in order.
        pub fn seen(&self) -> Vec<Seen> {
            self.seen.lock().expect("the requests").clone()
        }
    }

    impl Drop for Proxy {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// Serves one client connection, a request at a time.
    async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
        stream: S,
        upstream: &reqwest::Client,
        base: &str,
        faults: &Faults,
        seen: &Log,
    ) {
        let mut stream = BufReader::new(stream);
        while let Some(request) = read_request(&mut stream).await {
            seen.lock().expect("the requests").push(request.clone());
            let fault = request.route().and_then(|route| {
                let mut faults = faults.lock().expect("the faults");
                let index = faults.iter().position(|(armed, _)| *armed == route)?;
                faults.remove(index).map(|(_, fault)| fault)
            });
            let answer = match fault {
                Some(Fault::Refuse(status, code)) => (
                    status,
                    vec![("Content-Type".to_owned(), "application/json".to_owned())],
                    serde_json::json!({
                        "Code": code,
                        "Error": format!("refused by the test proxy ({status})"),
                    })
                    .to_string()
                    .into_bytes(),
                ),
                fault => {
                    let Ok(answer) = forward(upstream, base, &request).await else {
                        return;
                    };
                    if fault == Some(Fault::DropAnswer) {
                        // Closed without a TLS close_notify, as a reset
                        // connection is.
                        return;
                    }
                    answer
                }
            };
            if write_answer(stream.get_mut(), answer).await.is_err() {
                return;
            }
        }
    }

    /// Reads an HTTP/1.1 request with a `Content-Length` body (all the
    /// Proton client sends); `None` at the end of the connection.
    async fn read_request<S: AsyncRead + Unpin>(stream: &mut BufReader<S>) -> Option<Seen> {
        let mut line = String::new();
        if stream.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next()?.to_owned();
        let path = parts.next()?.to_owned();
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.ok()? == 0 {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            let (name, value) = line.split_once(':')?;
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
        let length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.ok()?;
        Some(Seen {
            method,
            path,
            headers,
            body,
        })
    }

    type Answer = (u16, Vec<(String, String)>, Vec<u8>);

    /// Sends the request on to the harness and reads its whole answer.
    async fn forward(upstream: &reqwest::Client, base: &str, request: &Seen) -> Result<Answer> {
        let hop = |name: &str| {
            ["host", "content-length", "connection", "transfer-encoding"]
                .iter()
                .any(|hop| name.eq_ignore_ascii_case(hop))
        };
        let method = reqwest::Method::from_bytes(request.method.as_bytes())?;
        let mut builder = upstream.request(method, format!("{base}{}", request.path));
        for (name, value) in request.headers.iter().filter(|(name, _)| !hop(name)) {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder.body(request.body.clone()).send().await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter(|(name, _)| !hop(name.as_str()))
            .map(|(name, value)| {
                (
                    name.to_string(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        Ok((status, headers, response.bytes().await?.to_vec()))
    }

    async fn write_answer<S: AsyncWrite + Unpin>(
        stream: &mut S,
        (status, headers, body): Answer,
    ) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {status} Proxied\r\n");
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(&body).await?;
        stream.flush().await
    }
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn a_send_that_may_have_gone_out_is_looked_up_before_any_retry() -> Result<()> {
    use proxy::{Fault, Proxy, Route};

    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let runtime = BackgroundRuntime::start(2)?;
    let proxy = runtime.handle().block_on(Proxy::start(&harness))?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let sync_service =
        SyncService::for_command(runtime.handle().clone(), store.clone(), secrets.clone())?;
    ensure!(sync_service.runs_sync());
    // Zmail reaches the harness only through the proxy.
    sync_service.configure_proton(
        ClientConfig::for_test_harness(proxy.url(), &harness.ca_pem()?)?
            .with_app_version(AppVersion::zmail(env!("CARGO_PKG_VERSION"))?),
    )?;
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;
    let identity_id = store
        .list_identities(account_id)?
        .into_iter()
        .find(|identity| identity.email_address == PROTON_HARNESS_PRIMARY_ADDRESS)
        .context("no identity for the primary address")?
        .id;

    // The test's own view of the account, straight to the harness.
    let own_secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, own_secrets.as_ref(), account_id)?;
    let sessions = zmail_proton::ProtonSessions::new(harness_config(&harness)?, own_secrets)?;
    let session = runtime.handle().block_on(sessions.session(account_id))?;
    // Sent copies, received copies (the alias's) and drafts with the
    // message's Message-ID.
    let held = |message_id: &str| -> Result<(usize, usize, usize)> {
        let metadata = runtime
            .handle()
            .block_on(session.client().get_message_metadata(&MessageFilter {
                external_id: message_id.to_owned(),
                ..MessageFilter::default()
            }))?;
        let flagged = |flag| {
            metadata
                .iter()
                .filter(|message| message.flags.has(flag))
                .count()
        };
        Ok((
            flagged(zmail_proton_api::MessageFlags::SENT),
            flagged(zmail_proton_api::MessageFlags::RECEIVED),
            metadata.iter().filter(|message| message.is_draft()).count(),
        ))
    };
    let state = |job_id: i64| -> Result<zmail_core::OutboxJobSummary> {
        store
            .list_outbox_jobs(Some(account_id))?
            .into_iter()
            .find(|job| job.id == job_id)
            .context("the job is kept")
    };
    let queue = |subject: &str| -> Result<i64> {
        let draft_id = store.create_draft(&NewDraft {
            account_id,
            identity_id,
            to: vec![PROTON_HARNESS_ALIAS_ADDRESS.to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_owned(),
            body: format!("{subject}, delivered once."),
            threading: Default::default(),
        })?;
        store.queue_draft(draft_id)
    };
    let pass = || -> Result<()> {
        sync_service
            .process_outbox_and_wait(Some(account_id))
            .with_context(|| format!("server log:\n{}", harness.logs()))?;
        Ok(())
    };
    // The attempt failed and the job waits to retry. Faults are armed
    // before a job is due, as the outbox poll may get to it first.
    let failed = |job_id: i64| -> Result<()> {
        let failed = state(job_id)?;
        ensure!(
            failed.state == OutboxJobState::Retrying,
            "{failed:?}; server log:\n{}",
            harness.logs()
        );
        ensure!(proxy.disarmed(), "the fault was not used");
        Ok(())
    };
    // The retry is made due now rather than after its back-off.
    let retry = |job_id: i64| -> Result<usize> {
        let from = proxy.seen().len();
        store.retry_outbox_job(job_id)?;
        pass()?;
        Ok(from)
    };
    // What the proxy saw from request `from` on: the drafts sent and the
    // lookups by ExternalID.
    let since = |from: usize| -> (Vec<String>, Vec<String>) {
        let seen = proxy.seen();
        let seen = &seen[from.min(seen.len())..];
        (
            seen.iter()
                .filter_map(|request| request.sent_draft().map(str::to_owned))
                .collect(),
            seen.iter().filter_map(proxy::Seen::lookup).collect(),
        )
    };
    let message_id = |from: usize| -> Result<String> {
        proxy.seen()[from..]
            .iter()
            .find_map(proxy::Seen::created_draft)
            .context("no draft was created")
    };

    // 1. Proton sent it, and the answer never arrived (the connection
    // dropped): the retry finds it sent and sends nothing.
    let start = proxy.seen().len();
    let harness_sends = send_requests(&harness.logs()).len();
    proxy.arm(Route::Send, Fault::DropAnswer);
    let job_id = queue("Sent, answer lost")?;
    pass()?;
    let id = message_id(start)?;
    ensure!(
        send_requests(&harness.logs()).len() == harness_sends + 1,
        "the harness did not get the send"
    );
    ensure!(held(&id)? == (1, 1, 0), "not sent before the drop");
    failed(job_id)?;
    let retried = retry(job_id)?;
    let after = state(job_id)?;
    ensure!(after.state == OutboxJobState::Sent, "{after:?}");
    ensure!(after.attempt_count == 2, "{after:?}");
    let (sends, lookups) = since(retried);
    ensure!(sends.is_empty(), "sent again: {sends:?}");
    ensure!(lookups.contains(&id), "no lookup by {id}: {lookups:?}");
    ensure!(
        send_requests(&harness.logs()).len() == harness_sends + 1,
        "exactly one send request; server log:\n{}",
        harness.logs()
    );
    let (sent, received, drafts) = held(&id)?;
    ensure!(
        (sent, received, drafts) == (1, 1, 0),
        "{sent} sent, {received} received, {drafts} drafts"
    );

    // 2. A server error before Proton sent it, then a lookup that fails
    // too: the job waits again without sending. The next retry finds the
    // draft unsent and sends that same draft, once.
    let start = proxy.seen().len();
    let harness_sends = send_requests(&harness.logs()).len();
    proxy.arm(Route::Send, Fault::Refuse(502, 0));
    let job_id = queue("Refused with 502")?;
    pass()?;
    let id = message_id(start)?;
    let (sends, _) = since(start);
    let [draft_id] = sends.as_slice() else {
        bail!("one send expected: {sends:?}");
    };
    let draft_id = draft_id.clone();
    ensure!(send_requests(&harness.logs()).len() == harness_sends);
    ensure!(held(&id)? == (0, 0, 1), "the draft is kept");
    failed(job_id)?;
    proxy.arm(Route::Lookup, Fault::Refuse(502, 0));
    let retried = retry(job_id)?;
    let (sends, lookups) = since(retried);
    ensure!(sends.is_empty(), "sent without a lookup: {sends:?}");
    ensure!(lookups.contains(&id), "no lookup by {id}: {lookups:?}");
    failed(job_id)?;
    let retried = retry(job_id)?;
    let after = state(job_id)?;
    ensure!(
        after.state == OutboxJobState::Sent,
        "{after:?}; server log:\n{}",
        harness.logs()
    );
    let (sends, _) = since(retried);
    ensure!(
        sends == [draft_id.clone()],
        "sends {sends:?}, the draft {draft_id}"
    );
    ensure!(send_requests(&harness.logs())[harness_sends..] == [draft_id]);
    let (sent, received, drafts) = held(&id)?;
    ensure!(
        (sent, received, drafts) == (1, 1, 0),
        "{sent} sent, {received} received, {drafts} drafts"
    );

    // 3. Proton refused it (validation failed): as before, the retry
    // sends from a new draft without looking anything up, and the refused
    // draft is left, as Bridge leaves it.
    let start = proxy.seen().len();
    let harness_sends = send_requests(&harness.logs()).len();
    proxy.arm(Route::Send, Fault::Refuse(422, 2001));
    let job_id = queue("Refused with 422")?;
    pass()?;
    let id = message_id(start)?;
    let (refused, _) = since(start);
    failed(job_id)?;
    let retried = retry(job_id)?;
    let after = state(job_id)?;
    ensure!(after.state == OutboxJobState::Sent, "{after:?}");
    let (sends, lookups) = since(retried);
    ensure!(
        sends.len() == 1 && refused.len() == 1 && sends != refused,
        "a new draft expected: refused {refused:?}, then sent {sends:?}"
    );
    ensure!(!lookups.contains(&id), "looked up: {lookups:?}");
    ensure!(send_requests(&harness.logs()).len() == harness_sends + 1);
    let (sent, received, drafts) = held(&id)?;
    ensure!(
        (sent, received, drafts) == (1, 1, 1),
        "{sent} sent, {received} received, {drafts} drafts"
    );
    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn a_reply_is_created_with_its_parent_and_threads() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    let sync = || {
        sync_service
            .sync_account_and_wait(account_id)
            .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))
    };
    sync()?;
    let identity_id = store
        .list_identities(account_id)?
        .into_iter()
        .find(|identity| identity.email_address == PROTON_HARNESS_PRIMARY_ADDRESS)
        .context("no identity for the primary address")?
        .id;

    // Opens (hydrates) the conversation called `subject` in `mailbox`, as
    // the reader does, and returns it with its message called `subject`.
    let open = |mailbox: &str, subject: &str| -> Result<(zmail_core::ThreadView, usize)> {
        let mailbox = store
            .list_mailboxes(account_id)?
            .into_iter()
            .find(|candidate| candidate.name == mailbox)
            .with_context(|| format!("no {mailbox}"))?;
        let thread = store
            .list_threads(mailbox.id, 0..100)?
            .into_iter()
            .find(|thread| thread.subject == subject)
            .with_context(|| format!("no {subject:?} in {}", mailbox.name))?;
        runtime
            .handle()
            .block_on(sync_service.hydrate_thread_body_and_wait(account_id, thread.id))?;
        let view = store.thread_view(thread.id)?.context("thread")?;
        let index = view
            .messages
            .iter()
            .position(|message| message.subject == subject)
            .context("no message with the subject")?;
        anyhow::Ok((view, index))
    };
    // Replies (to all, with `all`) to message `index` of `view` through
    // the outbox and returns the reply's subject and Message-ID.
    let reply =
        |view: &zmail_core::ThreadView, index: usize, all: bool| -> Result<(String, String)> {
            let start = zmail_actions::reply::reply(
                &view.subject,
                &view.mailbox_name,
                &view.messages[index],
                &store.list_identities(account_id)?,
                all,
                "today",
            )?;
            let draft_id = store.create_draft(&NewDraft {
                account_id,
                identity_id,
                to: start.to,
                cc: start.cc,
                bcc: Vec::new(),
                subject: start.subject.clone(),
                body: format!("Thanks.{}", start.body),
                threading: start.threading,
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
            let message_id = store
                .draft_detail(draft_id)?
                .context("the draft")?
                .message_id
                .context("the sent draft keeps its Message-ID")?;
            anyhow::Ok((start.subject, message_id))
        };
    let manager = Manager::new(harness_config(&harness)?)?;
    let metadata = |filter: MessageFilter| -> Result<Vec<zmail_proton_api::MessageMetadata>> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let (client, _) = manager
                    .login(harness.username(), harness.password())
                    .await?;
                anyhow::Ok(client.get_message_metadata(&filter).await?)
            })
    };

    // A message to the alias is answered from the alias.
    let (alias, index) = open("INBOX", "To the alias")?;
    let identities = store.list_identities(account_id)?;
    let start = zmail_actions::reply::reply(
        &alias.subject,
        &alias.mailbox_name,
        &alias.messages[index],
        &identities,
        false,
        "today",
    )?;
    let chosen = identities
        .iter()
        .find(|identity| Some(identity.id) == start.identity_id)
        .context("an identity is chosen")?;
    ensure!(
        chosen.email_address == PROTON_HARNESS_ALIAS_ADDRESS,
        "the reply comes from {}",
        chosen.email_address
    );

    // The seeded report has no external ID, so its Message-ID is Bridge's
    // internal one, and the parent is found by ID.
    let (report, index) = open("Folders/Work", "Quarterly report")?;
    let report_id = report.messages[index]
        .headers
        .message_id
        .clone()
        .context("the report's Message-ID")?;
    ensure!(report_id.ends_with("@protonmail.internalid"), "{report_id}");
    let (subject, first_id) = reply(&report, index, false)?;
    let parent = metadata(MessageFilter {
        id: vec![report.messages[index].remote_id.clone()],
        ..MessageFilter::default()
    })?;
    ensure!(
        parent.len() == 1 && parent[0].is_replied && !parent[0].is_replied_all,
        "the report is the reply's parent, answered with the reply action: {parent:?}"
    );

    // The sent reply carries Zmail's Message-ID as its external ID, so a
    // reply to it is found by that, and names it in In-Reply-To.
    sync()?;
    let (sent, index) = open("Sent", &subject)?;
    ensure!(
        sent.messages[index].headers.message_id.as_deref() == Some(first_id.as_str()),
        "{:?}",
        sent.messages[index].headers
    );
    // (A reply to one's own message goes to its recipients: reply all.)
    let (second_subject, second_id) = reply(&sent, index, true)?;
    let parent = metadata(MessageFilter {
        external_id: first_id.clone(),
        ..MessageFilter::default()
    })?;
    ensure!(
        parent.len() == 1 && parent[0].is_replied_all,
        "Zmail's own reply is the parent, answered with the reply-all action: {parent:?}"
    );
    sync()?;
    let (second, index) = open("Sent", &second_subject)?;
    let headers = &second.messages[index].headers;
    ensure!(
        headers.message_id.as_deref() == Some(second_id.as_str())
            && headers.in_reply_to == [first_id.clone()],
        "{headers:?}"
    );
    Ok(())
}

/// The first file under `directory` whose bytes or name contain `needle`.
fn file_containing(
    directory: &std::path::Path,
    needle: &[u8],
) -> Result<Option<std::path::PathBuf>> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            if let Some(found) = file_containing(&path, needle)? {
                return Ok(Some(found));
            }
            continue;
        }
        let named = path.file_name().is_some_and(|name| {
            name.as_encoded_bytes()
                .windows(needle.len())
                .any(|w| w == needle)
        });
        let held = std::fs::read(&path)?
            .windows(needle.len())
            .any(|window| window == needle);
        if named || held {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn onboarding_signs_in_step_by_step_and_the_session_syncs() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let harness = ProtonHarness::start()?;

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
    sync_service.configure_proton(harness_config(&harness)?)?;

    // A wrong password is an error, not a step. (The fake server answers
    // a bare 401 where Proton sends code 8002, which the sign-in words as
    // "the Proton username or password is wrong".)
    let wrong = sync_service.begin_proton_sign_in(harness.username(), "not the password")?;
    ensure!(
        runtime.handle().block_on(wrong.login()).is_err(),
        "a wrong password signed in"
    );

    // The harness account has one password and no second factor, so the
    // first step signs in.
    let sign_in = sync_service.begin_proton_sign_in(harness.username(), harness.password())?;
    let step = runtime.handle().block_on(sync_service.run_proton({
        let sign_in = sign_in.clone();
        async move { sign_in.login().await }
    }))??;
    let SignInStep::SignedIn(account) = step else {
        anyhow::bail!("expected to be signed in, got {step:?}");
    };
    ensure!(
        account.email == PROTON_HARNESS_PRIMARY_ADDRESS,
        "{account:?}"
    );

    // Onboarding stores the account, then the session (never the password).
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    runtime
        .handle()
        .block_on(sign_in.store(secrets.as_ref(), account_id))?;
    ensure!(
        !secrets
            .get_account_secret_from_slot(account_id, SecretSlot::ProtonUid)?
            .is_empty()
    );
    let key_pass = secrets.get_account_secret_from_slot(account_id, SecretSlot::ProtonKeyPass)?;
    ensure!(!key_pass.is_empty() && key_pass != harness.password());
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;
    ensure!(subjects(&store, account_id, "INBOX")?.contains(&"Welcome to the harness".to_owned()));
    Ok(())
}

/// Each mailbox's threads, as (conversation, messages, unread, flagged),
/// for comparing two mirrors of the same account.
type MirrorContents = Vec<(String, Vec<(String, Vec<String>, usize, bool)>)>;

fn mirror_contents(store: &LocalStore, account_id: i64) -> Result<MirrorContents> {
    let mut mailboxes: MirrorContents = store
        .account_mailbox_snapshots(account_id)?
        .into_iter()
        .map(|mailbox| {
            let mut threads: Vec<_> = mailbox
                .threads
                .into_iter()
                .map(|thread| {
                    let mut ids: Vec<String> = thread
                        .messages
                        .iter()
                        .map(|message| message.remote_id.clone())
                        .collect();
                    ids.sort();
                    (
                        thread.remote_thread_id,
                        ids,
                        thread.unread_count,
                        thread.is_flagged,
                    )
                })
                .collect();
            threads.sort();
            (mailbox.name, threads)
        })
        .collect();
    mailboxes.sort();
    Ok(mailboxes)
}

/// Waits for `ready` to hold, up to a minute.
fn wait_for(what: &str, mut ready: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !ready()? {
        ensure!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(())
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn shows_the_newest_mail_first_and_fetches_older_mail_in_the_background() -> Result<()> {
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
    let runtime = BackgroundRuntime::start(2)?;
    // Signed in up front, so the server changes below land while the older
    // mail is still arriving.
    let manager = Manager::new(harness_config(&harness)?)?;
    let (client, _) = runtime
        .handle()
        .block_on(manager.login(harness.username(), harness.password()))?;

    // Two messages a page, one page first and one more each sync: the six
    // seeded messages take several stretches.
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    sync_service.set_proton_paging(zmail_proton::SyncPaging {
        page_size: 2,
        first_sync_pages: 1,
        backfill_pages_per_run: 1,
    });
    let checkpoint = |store: &LocalStore| -> Result<zmail_proton::ProtonCheckpoint> {
        store.list_accounts()?[0]
            .sync_status
            .checkpoint
            .as_deref()
            .and_then(zmail_proton::ProtonCheckpoint::parse)
            .context("no Proton checkpoint")
    };

    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("first sync failed; server log:\n{}", harness.logs()))?;
    // The newest two, then one older message from the first stretch.
    let shown = subjects(&store, account_id, "All Mail")?;
    ensure!(shown.len() == 3, "after the first sync: {shown:?}");
    ensure!(checkpoint(&store)?.backfill_pending());
    let status = store.list_accounts()?[0].sync_status.clone();
    ensure!(
        status.detail == "Fetched 3 of 6 messages; older mail is still arriving",
        "detail: {:?}",
        status.detail
    );

    // Another client changes mail the backfill has not reached yet: the
    // events place it now and the backfill finds it already there.
    runtime.handle().block_on(async {
        client
            .mark_messages_read(&[message_id("Welcome to the harness")?])
            .await?;
        client
            .label_messages(&[message_id("Quarterly report")?], &family_id)
            .await?;
        anyhow::Ok(())
    })?;

    wait_for("the older mail", || {
        Ok(!checkpoint(&store)?.backfill_pending())
    })
    .with_context(|| format!("server log:\n{}", harness.logs()))?;
    // One more sync once the scheduled ones are done: nothing changes.
    wait_for("the last scheduled sync", || {
        Ok(sync_service.sync_account_and_wait(account_id).is_ok())
    })?;
    let status = store.list_accounts()?[0].sync_status.clone();
    ensure!(
        status.detail == "Proton account up to date",
        "detail: {:?}",
        status.detail
    );

    // The paged mirror ends up the same as a full sync of the account now.
    let reference = Arc::new(LocalStore::open_ephemeral()?);
    let reference_id = reference.insert_account(&proton_account(harness.username()))?;
    let reference_secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, reference_secrets.as_ref(), reference_id)?;
    let reference_service = SyncService::new(
        runtime.handle().clone(),
        reference.clone(),
        reference_secrets,
    );
    reference_service.configure_proton(harness_config(&harness)?)?;
    reference_service.sync_account_and_wait(reference_id)?;
    ensure!(!checkpoint(&reference)?.backfill_pending());

    let paged = mirror_contents(&store, account_id)?;
    ensure!(
        paged == mirror_contents(&reference, reference_id)?,
        "paged mirror {paged:#?}\n!= full sync {:#?}",
        mirror_contents(&reference, reference_id)?
    );
    ensure!(subjects(&store, account_id, "All Mail")?.len() == 6);
    ensure!(
        subjects(&store, account_id, "Labels/Family")?.contains(&"Quarterly report".to_owned())
    );
    Ok(())
}

/// Keeps every new-mail notification a sync raises.
#[derive(Default)]
struct RecordingNotifier(std::sync::Mutex<Vec<zmail_notify::NewMailArrival>>);

impl zmail_notify::Notifier for RecordingNotifier {
    fn notify_new_mail(&self, batch: &zmail_notify::NewMailBatch) {
        self.0
            .lock()
            .expect("notifications")
            .extend(batch.arrivals.iter().cloned());
    }
}

#[test]
#[ignore = "requires docker (or PROTON_HARNESS_MODE=native and Go) and the Proton harness"]
fn applies_events_to_just_the_changed_messages() -> Result<()> {
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

    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&proton_account(harness.username()))?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, secrets.as_ref(), account_id)?;
    let runtime = BackgroundRuntime::start(2)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync_service.configure_proton(harness_config(&harness)?)?;
    let notifier = Arc::new(RecordingNotifier::default());
    sync_service.set_notifier(notifier.clone());
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("sync failed; server log:\n{}", harness.logs()))?;
    ensure!(
        notifier.0.lock().expect("notifications").is_empty(),
        "a first sync is an import, not new mail"
    );

    // Opened before it moves: the body must move with it.
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
    let dinner = thread_id("INBOX", "Dinner on Sunday")?;
    ensure!(
        runtime
            .handle()
            .block_on(sync_service.hydrate_thread_body_and_wait(account_id, dinner))?
            == 1
    );
    let untouched = thread_id("Folders/Work", "Quarterly report")?;
    let untouched_rows: Vec<i64> = store
        .thread_view(untouched)?
        .context("thread")?
        .messages
        .iter()
        .map(|message| message.id)
        .collect();

    // Another client moves, flags, reads and deletes mail, and a new
    // message arrives from Zmail's own outbox.
    let manager = Manager::new(harness_config(&harness)?)?;
    runtime.handle().block_on(async {
        let (client, _) = manager
            .login(harness.username(), harness.password())
            .await?;
        let dinner = vec![message_id("Dinner on Sunday")?];
        client.label_messages(&dinner, "6").await?;
        client.unlabel_messages(&dinner, "0").await?;
        client
            .label_messages(&[message_id("Re: Welcome to the harness")?], "10")
            .await?;
        client
            .mark_messages_read(&[message_id("To the alias")?])
            .await?;
        client
            .delete_messages(&[message_id("Old receipt")?])
            .await?;
        anyhow::Ok(())
    })?;
    let identity = store
        .list_identities(account_id)?
        .into_iter()
        .find(|identity| identity.email_address == PROTON_HARNESS_PRIMARY_ADDRESS)
        .context("no identity for the primary address")?;
    let draft_id = store.create_draft(&NewDraft {
        account_id,
        identity_id: identity.id,
        to: vec![PROTON_HARNESS_ALIAS_ADDRESS.to_owned()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "Fresh news".to_owned(),
        body: "Something new.".to_owned(),
        threading: Default::default(),
    })?;
    store.queue_draft(draft_id)?;
    ensure!(sync_service.process_outbox_and_wait(Some(account_id))? == 1);
    // The outbox pass may start a sync of its own; wait it out.
    wait_for("the incremental sync", || {
        Ok(sync_service.sync_account_and_wait(account_id).is_ok())
    })
    .with_context(|| format!("server log:\n{}", harness.logs()))?;

    let status = store.list_accounts()?[0].sync_status.clone();
    ensure!(
        status.detail.contains("applied"),
        "expected an incremental sync, got {:?}",
        status.detail
    );
    ensure!(subjects(&store, account_id, "Archive")?.contains(&"Dinner on Sunday".to_owned()));
    ensure!(!subjects(&store, account_id, "INBOX")?.contains(&"Dinner on Sunday".to_owned()));
    let moved = store
        .thread_view(thread_id("Archive", "Dinner on Sunday")?)?
        .context("thread")?;
    ensure!(
        moved.messages[0].sanitized_html.is_some() || moved.messages[0].body_text.is_some(),
        "the opened body did not move with the message"
    );
    ensure!(!subjects(&store, account_id, "All Mail")?.contains(&"Old receipt".to_owned()));
    let rows_now: Vec<i64> = store
        .thread_view(untouched)?
        .context("thread")?
        .messages
        .iter()
        .map(|message| message.id)
        .collect();
    ensure!(
        rows_now == untouched_rows,
        "an unchanged thread was rewritten"
    );

    // Only the new message notifies: once, in the INBOX the alias got it
    // in (and in All Mail and Sent, which the mirror also had).
    let arrivals: Vec<String> = notifier
        .0
        .lock()
        .expect("notifications")
        .iter()
        .map(|arrival| arrival.subject.clone())
        .collect();
    ensure!(
        !arrivals.is_empty() && arrivals.iter().all(|subject| subject == "Fresh news"),
        "arrivals: {arrivals:?}"
    );

    // The same mirror a full sync of the account gives now.
    let reference = Arc::new(LocalStore::open_ephemeral()?);
    let reference_id = reference.insert_account(&proton_account(harness.username()))?;
    let reference_secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    sign_in(&harness, reference_secrets.as_ref(), reference_id)?;
    let reference_service = SyncService::new(
        runtime.handle().clone(),
        reference.clone(),
        reference_secrets,
    );
    reference_service.configure_proton(harness_config(&harness)?)?;
    reference_service.sync_account_and_wait(reference_id)?;
    let incremental = mirror_contents(&store, account_id)?;
    let full = mirror_contents(&reference, reference_id)?;
    ensure!(
        incremental == full,
        "incremental mirror {incremental:#?}\n!= full sync {full:#?}"
    );
    Ok(())
}
