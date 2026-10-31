//! Setup → Accounts: the list, every step of Add (presets, discovery with
//! its progress and the ISPDB question, the sign-in methods, the servers,
//! OAuth locally and over SSH, the connection test, the identity, saving),
//! Change and Delete, Proton's steps, and that no password or hostile
//! server string ever reaches the screen.
//!
//! Discovery runs against a fake network, so nothing here leaves the
//! machine except the connection test's attempt on a closed local port.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::KeyCode;
use url::Url;
use zmail_autodiscover::{
    Discovery, EmailAddress, Fetch, FetchResponse, Network, PortProbe, Step as DiscoveryStep,
    StepFindings,
};
use zmail_core::{AuthKind, ProtocolKind, time::HourCycle};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_setup::check::CheckPart;
use zmail_store::LocalStore;

use super::{
    assert_printable, assert_snapshot, control, draw, environment, key, keys, now, seeded_store,
    text_of,
};
use crate::{
    accounts::Step,
    app::{App, AppOptions, Effect, Environment, NO_ACCOUNTS, Prompt, Screen},
};

const AUTOCONFIG_URL: &str = "https://autoconfig.example.test/mail/config-v1.1.xml";

const AUTOCONFIG: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1"><emailProvider id="example.test">
  <incomingServer type="imap"><hostname>imap.example.test</hostname><port>993</port>
    <socketType>SSL</socketType><authentication>password-cleartext</authentication>
    <username>%EMAILADDRESS%</username></incomingServer>
  <outgoingServer type="smtp"><hostname>smtp.example.test</hostname><port>465</port>
    <socketType>SSL</socketType><authentication>password-cleartext</authentication>
    <username>%EMAILADDRESS%</username></outgoingServer>
</emailProvider></clientConfig>"#;

/// The password typed in these tests, which must never be drawn.
const PASSWORD: &str = "hunter2-secret-app-password";

/// Serves the autoconfig file, or fails every request with a hostile
/// error message.
struct FakeWeb {
    autoconfig: bool,
}

impl Fetch for FakeWeb {
    fn get(&self, url: &Url) -> Result<FetchResponse, String> {
        if self.autoconfig && url.as_str() == AUTOCONFIG_URL {
            return Ok(FetchResponse {
                status: 200,
                url: url.clone(),
                body: AUTOCONFIG.as_bytes().to_vec(),
            });
        }
        if self.autoconfig {
            return Ok(FetchResponse {
                status: 404,
                url: url.clone(),
                body: Vec::new(),
            });
        }
        Err("dns error: \x1b]0;owned\x07no record \x1b[2Jfound\u{202e}".to_owned())
    }
}

struct NoPorts;

impl PortProbe for NoPorts {
    fn reachable(&self, _: &str, _: u16) -> bool {
        false
    }
}

fn network(autoconfig: bool) -> Network {
    Network {
        fetch: Arc::new(FakeWeb { autoconfig }),
        dns: None,
        probe: Arc::new(NoPorts),
    }
}

/// An app that can add accounts: an in-memory keychain, the fake network
/// and (for the connection test) a runtime.
fn setup_app(
    store: Arc<LocalStore>,
    env: Environment,
    autoconfig: bool,
) -> (App, Arc<dyn SecretStore>) {
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    let app = App::new(AppOptions {
        store,
        sync: None,
        runtime: Some(runtime()),
        env,
        clock: Arc::new(now),
        hour_cycle: HourCycle::H24,
        version: "0.0.0-test".to_owned(),
        arrivals: None,
        secrets: Some(secrets.clone()),
        network: Some(network(autoconfig)),
    });
    (app, secrets)
}

/// A runtime shared by the tests, kept for the process.
fn runtime() -> tokio::runtime::Handle {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("a runtime")
        })
        .handle()
        .clone()
}

fn empty_app() -> (App, Arc<dyn SecretStore>) {
    let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
    setup_app(store, environment(), true)
}

fn add(app: &App) -> &crate::accounts::AddAccount {
    app.add.as_ref().expect("an account is being added")
}

fn add_mut(app: &mut App) -> &mut crate::accounts::AddAccount {
    app.add.as_mut().expect("an account is being added")
}

/// Ticks until `done`, or fails after a few seconds.
fn wait_for(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(app) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
        app.tick();
    }
}

/// The screen, checked for printable text and the password.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let buffer = draw(app, width, height);
    assert_printable(&buffer);
    let text = text_of(&buffer);
    assert!(!text.contains(PASSWORD), "the password is drawn:\n{text}");
    text
}

/// From the first run's Main Menu to the address step.
fn start_adding(app: &mut App) {
    assert_eq!(app.status.as_deref(), Some(NO_ACCOUNTS));
    keys(app, "s");
    assert_eq!(app.screen, Screen::AddAccount, "S goes straight to Add");
}

#[test]
fn the_accounts_list_and_its_commands() {
    let (mut app, _) = setup_app(seeded_store(), environment(), true);
    keys(&mut app, "s");
    assert_eq!(app.screen, Screen::Setup, "with accounts, S is Setup");
    keys(&mut app, "a");
    assert_eq!(app.screen, Screen::Accounts);
    for (width, height) in [(80, 24), (160, 48)] {
        let buffer = draw(&mut app, width, height);
        assert_snapshot(&format!("accounts-{width}x{height}"), &buffer);
    }
    keys(&mut app, "n");
    assert_eq!(app.accounts_cursor, 1);
    key(&mut app, KeyCode::Left);
    assert_eq!(app.screen, Screen::Setup, "< goes back to Setup");
}

#[test]
fn delete_asks_then_removes_the_account_and_its_secrets() {
    let (mut app, secrets) = setup_app(seeded_store(), environment(), true);
    let work = app.accounts[0].id;
    secrets.set_account_secret(work, PASSWORD).expect("kept");
    keys(&mut app, "sa");
    keys(&mut app, "d");
    assert!(matches!(app.prompt, Some(Prompt::RemoveAccount { .. })));
    assert_snapshot("accounts-delete-80x24", &draw(&mut app, 80, 24));
    keys(&mut app, "n");
    assert_eq!(app.accounts.len(), 2, "n keeps it");
    keys(&mut app, "dy");
    assert_eq!(app.accounts.len(), 1);
    assert!(secrets.get_account_secret(work).is_err());
    assert!(
        app.status
            .as_deref()
            .is_some_and(|status| status.starts_with("Removed Work")),
        "{:?}",
        app.status
    );
}

#[test]
fn change_saves_a_new_password_hidden_and_cycles_the_cache_budget() {
    let (mut app, secrets) = setup_app(seeded_store(), environment(), true);
    let work = app.accounts[0].id;
    keys(&mut app, "sa");
    keys(&mut app, "c");
    assert_eq!(app.screen, Screen::EditAccount);
    keys(&mut app, PASSWORD);
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("New password : [hidden]"), "{text}");
    assert_snapshot("account-change-80x24", &draw(&mut app, 80, 24));
    key(&mut app, KeyCode::Enter);
    assert_eq!(secrets.get_account_secret(work).expect("kept"), PASSWORD);
    assert!(
        app.edit
            .as_ref()
            .expect("still changing")
            .password
            .is_empty()
    );

    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.store.account_disk_budget_bytes(work).expect("read"),
        Some(250 * 1024 * 1024)
    );
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.screen, Screen::Accounts);
}

#[test]
fn a_preset_address_skips_discovery_and_the_password_never_shows() {
    let (mut app, secrets) = empty_app();
    start_adding(&mut app);
    // Builds with Proton offer it on this step.
    let name = if cfg!(feature = "proton") {
        "add-address-proton"
    } else {
        "add-address"
    };
    for (width, height) in [(80, 24), (160, 48)] {
        assert_snapshot(
            &format!("{name}-{width}x{height}"),
            &draw(&mut app, width, height),
        );
    }
    keys(&mut app, "jane@icloud.com");
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Method, "no discovery for iCloud");
    assert!(add(&app).discovery.is_none());
    assert_snapshot("add-method-80x24", &draw(&mut app, 80, 24));
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("App password for iCloud"), "{text}");
    assert!(
        text.contains("app-specific password"),
        "the GUI's hint:\n{text}"
    );

    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Credentials);
    keys(&mut app, "Jane Doe");
    key(&mut app, KeyCode::Tab);
    keys(&mut app, PASSWORD);
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("Password : [hidden]"), "{text}");
    assert_snapshot("add-credentials-80x24", &draw(&mut app, 80, 24));

    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Servers);
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("imap.mail.me.com"), "{text}");
    assert_snapshot("add-servers-80x24", &draw(&mut app, 80, 24));
    assert!(
        !format!("{:?}", add(&app).form).contains(PASSWORD),
        "Debug leaves the password out"
    );

    // Skip the live test: the identity step, then saving.
    add_mut(&mut app).step = Step::Identity;
    add_mut(&mut app).focus = 0;
    add_mut(&mut app).form.identity_email = "jane@icloud.com".to_owned();
    assert_snapshot("add-identity-80x24", &draw(&mut app, 80, 24));
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    keys(&mut app, "Jane");
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Done, "{:?}", add(&app).error);
    let account_id = add(&app).account_id.expect("saved");
    assert_eq!(
        secrets.get_account_secret(account_id).expect("kept"),
        PASSWORD
    );
    assert!(add(&app).form.password.is_empty(), "forgotten once saved");
    let stored = app.store.list_accounts().expect("accounts");
    assert_eq!(stored[0].username, "jane@icloud.com");
    let identities = app.store.list_identities(account_id).expect("identities");
    assert_eq!(identities[0].display_name, "Jane Doe");
    assert_eq!(identities[0].signature, "Jane");
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("jane@icloud.com is added"), "{text}");
    assert_snapshot("add-done-80x24", &draw(&mut app, 80, 24));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Accounts);
    assert_eq!(app.accounts.len(), 1);
}

#[test]
fn discovery_shows_each_step_and_asks_before_mozilla() {
    let (mut app, _) = empty_app();
    start_adding(&mut app);
    keys(&mut app, "jane@example.test");
    // A step part way through, for the snapshot.
    let email = EmailAddress::parse("jane@example.test").expect("valid");
    let mut discovery = Discovery::new(email.clone(), false);
    discovery.record(DiscoveryStep::DomainAutoconfig, Ok(StepFindings::default()));
    {
        let add = add_mut(&mut app);
        add.step = Step::Discovering;
        add.discovery = Some(discovery);
        add.running = Some(DiscoveryStep::WellKnownJmap);
    }
    assert_snapshot("add-discovering-80x24", &draw(&mut app, 80, 24));

    // Every step before ISPDB ran: the question.
    let mut discovery = Discovery::new(email, false);
    discovery.record(DiscoveryStep::DomainAutoconfig, Ok(StepFindings::default()));
    discovery.record(
        DiscoveryStep::WellKnownJmap,
        Err("error sending request: dns error: no record found".to_owned()),
    );
    {
        let add = add_mut(&mut app);
        add.discovery = Some(discovery);
        add.running = None;
    }
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("Ask Mozilla's database?"), "{text}");
    assert_snapshot("add-consent-80x24", &draw(&mut app, 80, 24));
    keys(&mut app, "n");
    wait_for(&mut app, "discovery to finish", |app| {
        app.add.as_ref().is_some_and(|add| add.step == Step::Method)
    });
    assert!(add(&app).found.is_none(), "nothing found");
    assert_eq!(
        add(&app).form.imap_host,
        "imap.example.test",
        "the usual guess"
    );
}

#[test]
fn discovery_runs_in_the_background_and_fills_the_servers() {
    let (mut app, _) = empty_app();
    start_adding(&mut app);
    keys(&mut app, "jane@example.test");
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Discovering);
    wait_for(&mut app, "the autoconfig file", |app| {
        app.add.as_ref().is_some_and(|add| add.step == Step::Method)
    });
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("imap.example.test:993"), "{text}");
    assert!(text.contains("Password"), "{text}");
}

#[test]
fn hostile_discovery_errors_are_filtered() {
    let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
    let (mut app, _) = setup_app(store, environment(), false);
    start_adding(&mut app);
    keys(&mut app, "jane@example.test");
    key(&mut app, KeyCode::Enter);
    wait_for(&mut app, "the ISPDB question", |app| {
        app.add.as_ref().is_some_and(|add| {
            add.running.is_none() && add.discovery.as_ref().is_some_and(Discovery::needs_consent)
        })
    });
    let text = screen(&mut app, 160, 48);
    assert!(text.contains("found"), "the error shows:\n{text}");
    assert!(
        !text.contains("owned\x07") && !text.contains('\u{202e}'),
        "{text}"
    );
}

#[test]
fn oauth_shows_the_url_and_opens_the_browser_locally() {
    let (mut app, _) = empty_app();
    start_adding(&mut app);
    keys(&mut app, "jane@gmail.com");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert!(add(&app).form.use_oauth);
    let text = screen(&mut app, 80, 24);
    assert!(
        text.contains("Create a Google Cloud project"),
        "the guide:\n{text}"
    );
    keys(&mut app, "Jane");
    key(&mut app, KeyCode::Tab);
    keys(&mut app, "42-abc.apps.googleusercontent.com");
    key(&mut app, KeyCode::Tab);
    keys(&mut app, "GOCSPX-not-shown");
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Servers);
    key(&mut app, KeyCode::End);
    while add(&app).focus + 1 < add(&app).rows().len() {
        key(&mut app, KeyCode::Tab);
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Browser);
    let effects = app.take_effects();
    let url = add(&app).sign_in_url.clone().expect("a URL");
    assert!(url.starts_with("https://accounts.google.com/"), "{url}");
    assert!(!url.contains("GOCSPX"), "the secret never goes in the URL");
    assert_eq!(effects, vec![Effect::Open(url)]);
    // A fixed URL for the snapshot: the real one has a random state.
    add_mut(&mut app).sign_in_url = Some(fixed_url());
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("open this address yourself"), "{text}");
    assert!(!text.contains("SSH"), "{text}");
    assert_snapshot("add-oauth-80x24", &draw(&mut app, 80, 24));
    // Leaving stops the wait and frees the loopback port.
    control(&mut app, 'c');
    assert!(app.add.is_none());
}

fn fixed_url() -> String {
    "https://accounts.google.com/o/oauth2/v2/auth?response_type=code&client_id=42-abc.apps.googleusercontent.com&redirect_uri=http%3A%2F%2F127.0.0.1%3A8357%2Fcallback&state=STATE&code_challenge=CHALLENGE&code_challenge_method=S256&scope=https%3A%2F%2Fmail.google.com%2F".to_owned()
}

#[test]
fn oauth_over_ssh_says_the_redirect_cannot_reach_this_machine() {
    let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
    let (mut app, _) = setup_app(
        store,
        Environment {
            over_ssh: true,
            ..environment()
        },
        true,
    );
    start_adding(&mut app);
    keys(&mut app, "jane@gmail.com");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Down);
    let text = screen(&mut app, 80, 48);
    assert!(
        text.contains("connected over SSH"),
        "said on the method step:\n{text}"
    );
    key(&mut app, KeyCode::Enter);
    keys(&mut app, "Jane");
    key(&mut app, KeyCode::Tab);
    keys(&mut app, "42-abc.apps.googleusercontent.com");
    key(&mut app, KeyCode::Tab);
    keys(&mut app, "GOCSPX-x");
    key(&mut app, KeyCode::Enter);
    while add(&app).focus + 1 < add(&app).rows().len() {
        key(&mut app, KeyCode::Tab);
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Browser);
    assert!(
        app.take_effects().is_empty(),
        "no browser opened on the remote machine"
    );
    add_mut(&mut app).sign_in_url = Some(fixed_url());
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("can't reach Zmail on this"), "{text}");
    assert_snapshot("add-oauth-ssh-80x24", &draw(&mut app, 80, 24));
    control(&mut app, 'c');
}

#[test]
fn the_connection_test_names_the_server_that_failed() {
    let (mut app, _) = empty_app();
    start_adding(&mut app);
    keys(&mut app, "jane@example.test");
    {
        // Straight to the servers: a closed port on this machine.
        let add = add_mut(&mut app);
        add.form.name = "Jane".to_owned();
        add.form.password = PASSWORD.to_owned();
        add.form.imap_host = "127.0.0.1".to_owned();
        add.form.imap_port = "9".to_owned();
        add.form.smtp_host = "127.0.0.1".to_owned();
        add.form.smtp_port = "9".to_owned();
        add.form
            .set_imap_security(zmail_autodiscover::Security::None);
        add.form.imap_port = "9".to_owned();
        add.form.allow_cleartext = true;
        add.step = Step::Servers;
    }
    while add(&app).focus + 1 < add(&app).rows().len() {
        key(&mut app, KeyCode::Tab);
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Testing);
    wait_for(&mut app, "the connection test", |app| {
        app.add
            .as_ref()
            .is_some_and(|add| add.step != Step::Testing)
    });
    assert_eq!(add(&app).step, Step::Servers, "back to fix the server");
    let error = add(&app).error.clone().unwrap_or_default();
    assert!(
        error.starts_with("Zmail couldn't connect to the incoming server (127.0.0.1:9)"),
        "{error}"
    );
    screen(&mut app, 80, 24);

    // The testing step and an error state, with fixed text for the
    // snapshots (the operating system's own words differ).
    add_mut(&mut app).step = Step::Testing;
    add_mut(&mut app).error = None;
    add_mut(&mut app).checking = Some(CheckPart::Outgoing);
    assert_snapshot("add-testing-80x24", &draw(&mut app, 80, 24));
    add_mut(&mut app).step = Step::Servers;
    add_mut(&mut app).error = Some(
        "Zmail couldn't connect to the incoming server (127.0.0.1:9): IMAP authentication \
         failed: \x1b[31mNO\x1b[0m [AUTHENTICATIONFAILED] Invalid credentials"
            .to_owned(),
    );
    let text = screen(&mut app, 80, 24);
    assert!(!text.contains("[31m"), "{text}");
    assert_snapshot("add-error-80x24", &draw(&mut app, 80, 24));
}

#[test]
fn esc_goes_back_a_step_and_ctrl_c_cancels() {
    let (mut app, _) = empty_app();
    start_adding(&mut app);
    keys(&mut app, "jane@icloud.com");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::Credentials);
    key(&mut app, KeyCode::Esc);
    assert_eq!(add(&app).step, Step::Method);
    key(&mut app, KeyCode::Esc);
    assert_eq!(add(&app).step, Step::Address);
    control(&mut app, 'c');
    assert!(app.add.is_none());
    assert_eq!(app.screen, Screen::Accounts);
    assert!(app.store.list_accounts().expect("accounts").is_empty());
}

#[test]
fn errors_show_beside_their_fields_and_pastes_are_text() {
    let (mut app, _) = empty_app();
    start_adding(&mut app);
    key(&mut app, KeyCode::Enter);
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("Enter your email address."), "{text}");
    app.handle_event(crossterm::event::Event::Paste(
        "jane@example.test\x1b[2J".to_owned(),
    ));
    assert_eq!(add(&app).form.email, "jane@example.test", "filtered");
}

#[test]
fn without_a_keychain_accounts_cannot_be_added() {
    let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
    let mut app = super::app_with(store, environment());
    keys(&mut app, "s");
    assert!(app.add.is_none());
    assert!(
        app.status
            .as_deref()
            .is_some_and(|status| status.contains("no keychain")),
        "{:?}",
        app.status
    );
}

#[test]
fn a_saved_account_is_an_imap_account_with_the_typed_servers() {
    let (mut app, _) = empty_app();
    start_adding(&mut app);
    keys(&mut app, "jane@example.test");
    {
        let add = add_mut(&mut app);
        add.form.password = PASSWORD.to_owned();
        add.form.imap_host = "imap.example.test".to_owned();
        add.form.smtp_host = "smtp.example.test".to_owned();
        add.form.identity_email = "jane@example.test".to_owned();
        add.step = Step::Identity;
    }
    while add(&app).focus + 1 < add(&app).rows().len() {
        key(&mut app, KeyCode::Tab);
    }
    key(&mut app, KeyCode::Enter);
    let account_id = add(&app).account_id.expect("saved");
    let target = app
        .store
        .sync_target(account_id)
        .expect("read")
        .expect("there");
    assert_eq!(target.protocol, ProtocolKind::Imap);
    assert_eq!(target.auth, AuthKind::Password);
    assert_eq!(target.endpoint, "imaps://imap.example.test:993");
}

#[cfg(feature = "proton")]
#[test]
fn proton_steps_show_the_risk_note_and_ask_for_the_code() {
    let (mut app, _) = empty_app();
    start_adding(&mut app);
    keys(&mut app, "zmail@proton.me");
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).step, Step::ProtonSignIn);
    keys(&mut app, PASSWORD);
    let text = screen(&mut app, 80, 48);
    assert!(
        text.contains("signs in as Bridge"),
        "the GUI's note:\n{text}"
    );
    assert_snapshot("add-proton-80x24", &draw(&mut app, 80, 24));
    // Without a sync service there is no Proton connection to sign in on.
    key(&mut app, KeyCode::Enter);
    assert!(add(&app).proton.error.is_some());

    // Back to the address: a server account again.
    key(&mut app, KeyCode::Esc);
    assert_eq!(add(&app).step, Step::Address);
    let email = &mut add_mut(&mut app).form.email;
    email.clear();
    email.push_str("jane@example.test");
    key(&mut app, KeyCode::Enter);
    assert_eq!(add(&app).form.protocol, ProtocolKind::Imap);

    add_mut(&mut app).step = Step::ProtonTotp;
    add_mut(&mut app).focus = 0;
    add_mut(&mut app).proton.error = None;
    keys(&mut app, "123456");
    assert_snapshot("add-proton-totp-80x24", &draw(&mut app, 80, 24));
    add_mut(&mut app).step = Step::ProtonMailboxPassword;
    add_mut(&mut app).proton_input = PASSWORD.to_owned();
    let text = screen(&mut app, 80, 24);
    assert!(text.contains("Mailbox password : [hidden]"), "{text}");
}
