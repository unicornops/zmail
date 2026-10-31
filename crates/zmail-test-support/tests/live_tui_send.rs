//! The terminal UI sends a message through the Docker mail harness: Compose
//! (ADR 0007, phase 3) writes it, `^X` queues it through the sync service as
//! the desktop app does, the outbox delivers it over SMTP, and the next
//! sync finds it in the inbox it was sent to.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutboxJobState, OutgoingConfig,
    ProtocolKind, SubmissionKind, SubmissionSecurity, time::HourCycle,
};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{
    DockerMailHarness, MAIL_HARNESS_HOST, MAIL_HARNESS_IMAP_ENDPOINT, MAIL_HARNESS_RECIPIENT,
    MAIL_HARNESS_RECIPIENT_LOGIN, MAIL_HARNESS_RECIPIENT_PASSWORD, MAIL_HARNESS_SMTP_PORT,
    live_harness_lock,
};
use zmail_tui::{App, AppOptions, Environment};

const SUBJECT: &str = "Sent from the terminal UI";

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn the_terminal_ui_sends_through_the_outbox() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let account_id = store.insert_account(&NewAccount {
        display_name: "Terminal Harness".to_owned(),
        protocol: ProtocolKind::Imap,
        auth: AuthKind::Password,
        endpoint: MAIL_HARNESS_IMAP_ENDPOINT.to_owned(),
        username: MAIL_HARNESS_RECIPIENT_LOGIN.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: Some(OutgoingConfig {
            kind: SubmissionKind::Smtp,
            host: Some(MAIL_HARNESS_HOST.to_owned()),
            port: Some(MAIL_HARNESS_SMTP_PORT),
            username: Some(MAIL_HARNESS_RECIPIENT_LOGIN.to_owned()),
            security: Some(SubmissionSecurity::Plain),
            hello_name: "localhost".to_owned(),
        }),
        identities: vec![NewIdentity {
            display_name: "Terminal Recipient".to_owned(),
            email_address: MAIL_HARNESS_RECIPIENT.to_owned(),
            signature: String::new(),
            is_primary: true,
        }],
    })?;
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    secrets.set_account_secret(account_id, MAIL_HARNESS_RECIPIENT_PASSWORD)?;
    // No undo-send delay: the message goes as soon as it is queued.
    store.update_ui_preferences(|preferences| preferences.undo_send_delay_secs = 0)?;

    let runtime = BackgroundRuntime::start(2)?;
    let sync = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    sync.sync_account_and_wait(account_id)?;
    let mut app = App::new(AppOptions {
        store: store.clone(),
        sync: Some(sync.clone()),
        runtime: Some(runtime.handle().clone()),
        env: Environment {
            color: false,
            unicode: true,
            over_ssh: false,
            macos: false,
        },
        clock: Arc::new(|| chrono::Local::now().fixed_offset()),
        hour_cycle: HourCycle::H24,
        version: "0.0.0-test".to_owned(),
        arrivals: None,
    });

    // C, To, down to Subject, the body, ^X, y.
    type_text(&mut app, "c");
    type_text(&mut app, MAIL_HARNESS_RECIPIENT);
    for _ in 0..4 {
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    type_text(&mut app, SUBJECT);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    type_text(&mut app, "Written in Compose.");
    press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    type_text(&mut app, "y");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let jobs = store.list_outbox_jobs(None)?;
        let job = jobs.first().context("the message was queued")?;
        match job.state {
            OutboxJobState::Sent => break,
            OutboxJobState::Failed => bail!("delivery failed: {:?}", job.last_error_message),
            _ if Instant::now() > deadline => bail!("not sent in time: {job:?}"),
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }

    // It arrives in the inbox it was sent to.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        sync.sync_account_and_wait(account_id)?;
        let inbox = store
            .list_mailboxes(account_id)?
            .into_iter()
            .find(|mailbox| mailbox.name.eq_ignore_ascii_case("INBOX"))
            .context("an INBOX")?;
        if store
            .list_threads(inbox.id, 0..50)?
            .iter()
            .any(|thread| thread.subject == SUBJECT)
        {
            break;
        }
        if Instant::now() > deadline {
            bail!("the sent message never arrived");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    drop(app);
    drop(runtime);
    Ok(())
}
