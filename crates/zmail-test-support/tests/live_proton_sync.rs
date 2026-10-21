//! Live tests for Proton accounts in the sync runtime (`zmail-sync` with
//! its `proton` feature) against the Proton harness.
//!
//! An account is signed in the way onboarding will do it (SRP login, salted
//! key password, the three Proton secret slots) and then synced through
//! `SyncService`, which resumes the session from the keychain.

use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use zmail_core::{AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind};
use zmail_proton::{ClientConfig, store_new_session};
use zmail_proton_api::{AppVersion, Manager};
use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{ProtonHarness, live_harness_lock};

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
