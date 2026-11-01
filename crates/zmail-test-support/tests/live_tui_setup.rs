//! The terminal UI adds an IMAP account against the Docker mail harness
//! (ADR 0007, phase 4): S on a store with no account goes straight to Add;
//! the address finds nothing (discovery runs on a fake network, so nothing
//! leaves the machine), so the servers are typed in, unencrypted with the
//! opt-in as the harness needs; the connection test signs in to the
//! harness's IMAP and SMTP servers; the identity is changed; and the saved
//! account's first sync brings the harness's preloaded message.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use url::Url;
use zmail_autodiscover::{Fetch, FetchResponse, Network, PortProbe};
use zmail_core::{AuthKind, ProtocolKind, time::HourCycle};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_test_support::{
    DockerMailHarness, MAIL_HARNESS_HOST, MAIL_HARNESS_IMAP_ENDPOINT,
    MAIL_HARNESS_PRELOADED_SUBJECT, MAIL_HARNESS_RECIPIENT, MAIL_HARNESS_RECIPIENT_LOGIN,
    MAIL_HARNESS_RECIPIENT_PASSWORD, MAIL_HARNESS_SMTP_PORT, live_harness_lock,
};
use zmail_tui::{AddStep, App, AppOptions, Environment};

/// The address typed: the harness's own (`phase2@localhost`) has a single
/// label domain, which an address can't have. The identity sends as the
/// harness's address.
const ADDRESS: &str = "phase2@harness.example";

/// Every HTTPS request finds nothing.
struct NothingOnTheWeb;

impl Fetch for NothingOnTheWeb {
    fn get(&self, url: &Url) -> Result<FetchResponse, String> {
        Ok(FetchResponse {
            status: 404,
            url: url.clone(),
            body: Vec::new(),
        })
    }
}

/// No port answers the hostname guess.
struct NoPorts;

impl PortProbe for NoPorts {
    fn reachable(&self, _: &str, _: u16) -> bool {
        false
    }
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
}

fn key(app: &mut App, code: KeyCode) {
    press(app, code, KeyModifiers::NONE);
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        key(app, KeyCode::Char(ch));
    }
}

/// Clears the focused field and types `text` into it.
fn replace(app: &mut App, text: &str) {
    press(app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_text(app, text);
}

/// Ticks the app until the store says `done`, or fails with what the app
/// shows.
fn wait(app: &mut App, what: &str, seconds: u64, mut done: impl FnMut(&App) -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !done(app) {
        if Instant::now() > deadline {
            bail!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(100));
        app.tick();
    }
    Ok(())
}

#[test]
#[ignore = "requires docker compose and the Phase 2b live mail harness"]
fn the_terminal_ui_adds_an_imap_account_and_syncs_it() -> Result<()> {
    let _harness_lock = live_harness_lock();
    let _harness = DockerMailHarness::start()?;
    let store = Arc::new(LocalStore::open_ephemeral()?);
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    let runtime = BackgroundRuntime::start(2)?;
    let sync = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
    let mut app = App::new(AppOptions {
        store: store.clone(),
        sync: Some(sync),
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
        secrets: Some(secrets.clone()),
        network: Some(Network {
            fetch: Arc::new(NothingOnTheWeb),
            dns: None,
            probe: Arc::new(NoPorts),
        }),
    });

    // No account yet: S goes straight to Add. The address, then discovery,
    // which finds nothing and asks about Mozilla's database: no.
    type_text(&mut app, "s");
    type_text(&mut app, ADDRESS);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.add_step(), Some(AddStep::Discovering));
    wait(
        &mut app,
        "the ISPDB question or the method step",
        30,
        |app| {
            app.commands()
                .iter()
                .any(|entry| entry.label == "AskMozilla")
                || app.add_step() == Some(AddStep::Method)
        },
    )?;
    if app.add_step() == Some(AddStep::Discovering) {
        type_text(&mut app, "n");
    }
    wait(&mut app, "the method step", 30, |app| {
        app.add_step() == Some(AddStep::Method)
    })?;

    // A password; the name and the password.
    key(&mut app, KeyCode::Enter);
    type_text(&mut app, "Terminal Harness");
    key(&mut app, KeyCode::Tab);
    type_text(&mut app, MAIL_HARNESS_RECIPIENT_PASSWORD);
    key(&mut app, KeyCode::Enter);

    // The servers: Protocol, IMAP server, port, security, user name, SMTP
    // server, port, security, user name, then the cleartext opt-in.
    let imap_port = Url::parse(MAIL_HARNESS_IMAP_ENDPOINT)?
        .port()
        .context("the harness's IMAP port")?;
    key(&mut app, KeyCode::Tab);
    replace(&mut app, MAIL_HARNESS_HOST);
    key(&mut app, KeyCode::Tab);
    replace(&mut app, &imap_port.to_string());
    key(&mut app, KeyCode::Tab);
    type_text(&mut app, " "); // TLS → None
    key(&mut app, KeyCode::Tab);
    replace(&mut app, MAIL_HARNESS_RECIPIENT_LOGIN);
    key(&mut app, KeyCode::Tab);
    replace(&mut app, MAIL_HARNESS_HOST);
    key(&mut app, KeyCode::Tab);
    replace(&mut app, &MAIL_HARNESS_SMTP_PORT.to_string());
    key(&mut app, KeyCode::Tab);
    type_text(&mut app, "  "); // TLS → STARTTLS → None
    key(&mut app, KeyCode::Tab);
    replace(&mut app, MAIL_HARNESS_RECIPIENT_LOGIN);
    key(&mut app, KeyCode::Tab);
    type_text(&mut app, " "); // Connect without encryption
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.add_step(),
        Some(AddStep::Testing),
        "{:?}",
        app.add_error()
    );

    // The connection test signs in to both servers.
    wait(&mut app, "the connection test", 60, |app| {
        app.add_step() != Some(AddStep::Testing)
    })?;
    assert_eq!(
        app.add_step(),
        Some(AddStep::Identity),
        "the connection test failed: {:?}",
        app.add_error()
    );
    // On the identity step: send as the harness's address, then save.
    key(&mut app, KeyCode::Tab);
    replace(&mut app, MAIL_HARNESS_RECIPIENT);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.add_step(), Some(AddStep::Done), "{:?}", app.add_error());

    let account = store
        .list_accounts()?
        .into_iter()
        .next()
        .context("the account was saved; the connection test or saving failed")?;
    assert_eq!(account.protocol, ProtocolKind::Imap);
    let target = store.sync_target(account.id)?.context("its sync target")?;
    assert_eq!(target.auth, AuthKind::Password);
    assert_eq!(target.endpoint, MAIL_HARNESS_IMAP_ENDPOINT);
    assert_eq!(target.username, MAIL_HARNESS_RECIPIENT_LOGIN);
    assert_eq!(
        secrets.get_account_secret(account.id)?,
        MAIL_HARNESS_RECIPIENT_PASSWORD,
        "the password is in the keychain"
    );
    let identities = store.list_identities(account.id)?;
    assert_eq!(identities[0].email_address, MAIL_HARNESS_RECIPIENT);
    assert_eq!(identities[0].display_name, "Terminal Harness");

    // The first sync started when the account was saved.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        app.tick();
        let inbox = store
            .list_mailboxes(account.id)?
            .into_iter()
            .find(|mailbox| mailbox.name.eq_ignore_ascii_case("INBOX"));
        if let Some(inbox) = inbox
            && store
                .list_threads(inbox.id, 0..50)?
                .iter()
                .any(|thread| thread.subject == MAIL_HARNESS_PRELOADED_SUBJECT)
        {
            break;
        }
        if Instant::now() > deadline {
            bail!("the first sync never brought the preloaded message");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    drop(app);
    drop(runtime);
    Ok(())
}
