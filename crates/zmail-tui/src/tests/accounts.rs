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
    app_with, assert_printable, assert_snapshot, control, draw, environment, key, keys, now,
    open_work_inbox, seeded_store, text_of,
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

/// Links Work's identity to a server identity whose name differs and
/// whose strings are hostile (a conflict), and leaves a second identity
/// the server stopped listing.
fn seed_identities_needing_attention(store: &LocalStore) {
    use zmail_core::identity_sync::{IdentityValues, ServerIdentity};
    let work = store.list_accounts().expect("accounts")[0].id;
    let server = |id: &str, email: &str, name: &str, signature: &str| ServerIdentity {
        id: id.to_owned(),
        email: email.to_owned(),
        values: IdentityValues {
            name: name.to_owned(),
            reply_to: String::new(),
            signature: signature.to_owned(),
        },
        may_delete: true,
    };
    let both = [
        server(
            "i1",
            "me@work.example",
            "Work\x1b[2J \u{202E}Server",
            "-- sent\x1b]0;pwned\x07 from\nthe web",
        ),
        server("i2", "help@work.example", "Help desk", ""),
    ];
    store.merge_server_identities(work, &both).expect("merged");
    store
        .merge_server_identities(work, &both[..1])
        .expect("merged");
}

#[test]
fn change_lists_identities_and_offers_both_directions_for_a_differing_one() {
    use zmail_core::identity_sync::IdentitySyncStatus;

    let store = seeded_store();
    seed_identities_needing_attention(&store);
    let (mut app, _) = setup_app(store, environment(), true);
    let work = app.accounts[0].id;
    keys(&mut app, "sa");
    keys(&mut app, "c");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    for (width, height) in [(80, 24), (160, 48)] {
        let text = screen(&mut app, width, height);
        assert!(
            text.contains("changed in Zmail and on the server"),
            "{text}"
        );
        assert!(
            text.contains("UpdServer") && text.contains("UseServer"),
            "{text}"
        );
        assert!(!text.contains("pwned\u{7}"), "{text}");
        assert_snapshot(
            &format!("account-identities-{width}x{height}"),
            &draw(&mut app, width, height),
        );
    }

    // Updating the server needs the sync service, which tests lack.
    keys(&mut app, "u");
    assert_eq!(
        app.status.as_deref(),
        Some("This Zmail can't reach the server right now")
    );

    // S takes the server's values, filtered only when drawn.
    keys(&mut app, "s");
    let identities = app.store.list_synced_identities(work).expect("identities");
    assert_eq!(identities[0].status, IdentitySyncStatus::InSync);
    assert_eq!(identities[0].local.name, "Work\x1b[2J \u{202E}Server");
    screen(&mut app, 80, 24);

    // The one the server no longer lists: R or D, and D asks first.
    key(&mut app, KeyCode::Down);
    let text = screen(&mut app, 160, 48);
    assert!(
        text.contains("help@work.example is no longer on the server"),
        "{text}"
    );
    assert!(
        text.contains("Recreate") && text.contains("Remove"),
        "{text}"
    );
    keys(&mut app, "d");
    assert!(matches!(app.prompt, Some(Prompt::RemoveIdentity { .. })));
    keys(&mut app, "n");
    assert_eq!(
        app.store.list_identities(work).expect("identities").len(),
        2
    );
    keys(&mut app, "d");
    keys(&mut app, "y");
    assert_eq!(
        app.store.list_identities(work).expect("identities").len(),
        1
    );
    assert_eq!(
        app.status.as_deref(),
        Some("Removed help@work.example from Zmail")
    );
    assert_eq!(app.edit.as_ref().expect("still changing").cursor, 2);
}

#[test]
fn the_identity_form_edits_an_identity_and_new_mail_uses_it() {
    let store = seeded_store();
    let (mut app, _) = setup_app(store.clone(), environment(), true);
    let work = app.accounts[0].id;
    keys(&mut app, "sa");
    keys(&mut app, "c");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);

    // Ret opens the form; ^C leaves it unchanged.
    key(&mut app, KeyCode::Enter);
    assert!(app.edit.as_ref().is_some_and(|edit| edit.form.is_some()));
    assert!(screen(&mut app, 80, 24).contains("CHANGE IDENTITY"));
    keys(&mut app, "zz");
    control(&mut app, 'c');
    assert_eq!(app.status.as_deref(), Some("Nothing changed"));
    assert_eq!(
        app.store.list_identities(work).expect("identities")[0].display_name,
        "Work"
    );

    // A bad address keeps the form open, saying why under the field.
    keys(&mut app, "e");
    control(&mut app, 'u');
    keys(&mut app, "Me at work");
    key(&mut app, KeyCode::Tab);
    control(&mut app, 'u');
    keys(&mut app, "not an address");
    control(&mut app, 'x');
    assert_eq!(
        app.status.as_deref(),
        Some("Address: Enter an address like name@example.com.")
    );
    for (width, height) in [(80, 24), (160, 48)] {
        let text = screen(&mut app, width, height);
        assert!(text.contains("Enter an address like"), "{text}");
        assert_snapshot(
            &format!("identity-form-error-{width}x{height}"),
            &draw(&mut app, width, height),
        );
    }

    // Fixed, with a reply-to and a two-line signature: saved in Zmail only
    // (IMAP keeps identities locally).
    control(&mut app, 'u');
    keys(&mut app, "me@work.example");
    key(&mut app, KeyCode::Down);
    keys(&mut app, "team@work.example");
    key(&mut app, KeyCode::Enter);
    keys(&mut app, "Regards");
    key(&mut app, KeyCode::Enter);
    keys(&mut app, "Me");
    control(&mut app, 'x');
    assert!(app.prompt.is_none());
    assert_eq!(app.status.as_deref(), Some("Saved me@work.example"));
    let saved = &app.store.list_synced_identities(work).expect("identities")[0];
    assert_eq!(saved.local.name, "Me at work");
    assert_eq!(saved.local.reply_to, "team@work.example");
    assert_eq!(saved.local.signature, "Regards\nMe");
    assert!(app.edit.as_ref().is_some_and(|edit| edit.form.is_none()));

    // A new message starts from the edited signature.
    let mut app = app_with(store, environment());
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, "c");
    let compose = app.compose.as_ref().expect("composing");
    assert_eq!(compose.text(crate::compose::Field::Body), "\n\nRegards\nMe");
}

/// Adds a JMAP account, Fastmail, whose identity and help desk are linked
/// to server identities the user may delete (the help desk's name is
/// hostile), and a Proton account; returns their ids.
fn seed_jmap_and_proton(store: &LocalStore) -> (i64, i64) {
    use zmail_core::{
        CachePolicySummary, NewAccount, NewIdentity,
        identity_sync::{IdentityValues, ServerIdentity},
    };
    let account = |name: &str, protocol: ProtocolKind, email: &str| NewAccount {
        display_name: name.to_owned(),
        protocol,
        auth: AuthKind::Password,
        endpoint: "https://api.fast.example/jmap/session".to_owned(),
        username: email.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: None,
        identities: vec![NewIdentity {
            display_name: name.to_owned(),
            email_address: email.to_owned(),
            signature: String::new(),
            is_primary: true,
        }],
    };
    let jmap = store
        .insert_account(&account("Fastmail", ProtocolKind::Jmap, "me@fast.example"))
        .expect("stored");
    let proton = store
        .insert_account(&account(
            "Proton",
            ProtocolKind::Proton,
            "me@proton.example",
        ))
        .expect("stored");
    let server = |id: &str, email: &str, name: &str| ServerIdentity {
        id: id.to_owned(),
        email: email.to_owned(),
        values: IdentityValues {
            name: name.to_owned(),
            reply_to: String::new(),
            signature: "-- sent\x1b]0;pwned\x07 from the server".to_owned(),
        },
        may_delete: true,
    };
    store
        .merge_server_identities(
            jmap,
            &[
                server("i1", "me@fast.example", "Fastmail"),
                server("i2", "help@fast.example", "Help\x1b[2J \u{202E}desk"),
            ],
        )
        .expect("merged");
    (jmap, proton)
}

#[test]
fn a_jmap_identity_keeps_its_address_and_the_server_is_offered_each_change() {
    use zmail_core::identity_sync::IdentitySyncStatus;

    let store = seeded_store();
    let (jmap, proton) = seed_jmap_and_proton(&store);
    let (mut app, _) = setup_app(store, environment(), true);
    keys(&mut app, "sa");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    keys(&mut app, "c");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    for (width, height) in [(80, 24), (160, 48)] {
        let text = screen(&mut app, width, height);
        assert!(text.contains("(primary, same as the server)"), "{text}");
        assert!(!text.contains("pwned\u{7}"), "{text}");
        assert_snapshot(
            &format!("identity-list-{width}x{height}"),
            &draw(&mut app, width, height),
        );
    }

    // The help desk: its address is the server's, drawn fixed, and the
    // cursor skips it (Name, Reply-To, Signature).
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    for (width, height) in [(80, 24), (160, 48)] {
        let text = screen(&mut app, width, height);
        assert!(text.contains("The server sets this address"), "{text}");
        assert!(text.contains("^X") && text.contains("Save"), "{text}");
        assert!(!text.contains("pwned\u{7}"), "{text}");
        assert_snapshot(
            &format!("identity-form-{width}x{height}"),
            &draw(&mut app, width, height),
        );
    }
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    control(&mut app, 'u');
    keys(&mut app, "-- The help desk");
    control(&mut app, 'x');
    // Saved; the server can have it too.
    assert!(matches!(
        app.prompt,
        Some(Prompt::IdentityToServer { create: false, .. })
    ));
    let help = app
        .store
        .list_synced_identities(jmap)
        .expect("identities")
        .into_iter()
        .find(|identity| identity.identity.email_address == "help@fast.example")
        .expect("listed");
    assert_eq!(help.status, IdentitySyncStatus::ChangedHere);
    assert_eq!(help.local.signature, "-- The help desk");
    // The untouched name keeps its stored value, not the filtered one.
    assert_eq!(help.local.name, "Help\x1b[2J \u{202E}desk");
    assert!(
        app.prompt_text()
            .is_some_and(|text| text.contains("Update the server too?"))
    );
    // (Updating needs the sync service, which tests lack; the live James
    // test covers it.)
    keys(&mut app, "y");
    assert_eq!(
        app.status.as_deref(),
        Some("This Zmail can't reach the server right now")
    );

    // Add, and keep it in Zmail.
    keys(&mut app, "a");
    assert!(screen(&mut app, 80, 24).contains("ADD IDENTITY"));
    keys(&mut app, "Alias");
    key(&mut app, KeyCode::Tab);
    keys(&mut app, "alias@fast.example");
    control(&mut app, 'x');
    assert!(matches!(
        app.prompt,
        Some(Prompt::IdentityToServer { create: true, .. })
    ));
    keys(&mut app, "n");
    assert_eq!(app.status.as_deref(), Some("Added alias@fast.example"));
    let identities = app.store.list_synced_identities(jmap).expect("identities");
    assert_eq!(identities.len(), 3);
    assert_eq!(identities[2].status, IdentitySyncStatus::Local);

    // The cursor is on it: make it primary, and it lists first.
    keys(&mut app, "*");
    assert_eq!(
        app.status.as_deref(),
        Some("New mail now starts from alias@fast.example")
    );
    let identities = app.store.list_identities(jmap).expect("identities");
    assert_eq!(identities[0].email_address, "alias@fast.example");
    assert!(identities[0].is_primary);
    let edit = app.edit.as_ref().expect("changing");
    assert_eq!(
        edit.identity_at_cursor()
            .map(|identity| identity.identity.id),
        Some(identities[0].id)
    );

    // The primary stays.
    keys(&mut app, "d");
    assert!(app.prompt.is_none());
    assert!(
        app.status
            .as_deref()
            .is_some_and(|status| status.contains("primary identity, so it stays"))
    );

    // The help desk: removing it asks, offering the server too.
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    keys(&mut app, "d");
    assert!(matches!(
        app.prompt,
        Some(Prompt::RemoveIdentity {
            everywhere: true,
            ..
        })
    ));
    assert!(
        app.commands()
            .iter()
            .any(|entry| entry.label == "AndServer"),
        "{:?}",
        app.commands()
    );
    keys(&mut app, "n");
    assert_eq!(app.status.as_deref(), Some("Nothing removed"));
    keys(&mut app, "d");
    keys(&mut app, "s");
    assert_eq!(
        app.status.as_deref(),
        Some("This Zmail can't reach the server right now")
    );
    keys(&mut app, "d");
    keys(&mut app, "y");
    assert_eq!(
        app.status.as_deref(),
        Some("Removed help@fast.example from Zmail")
    );
    assert_eq!(
        app.store.list_identities(jmap).expect("identities").len(),
        2
    );

    // The server still lists it, so it is hidden from later syncs, under
    // Hidden identities; Ret brings it back.
    let edit = app.edit.as_ref().expect("changing");
    assert_eq!(edit.hidden.len(), 1);
    let rows = edit.rows();
    while app.edit.as_ref().is_some_and(|edit| edit.cursor + 1 < rows) {
        key(&mut app, KeyCode::Down);
    }
    for (width, height) in [(80, 24), (160, 48)] {
        let text = screen(&mut app, width, height);
        assert!(text.contains("Hidden identities"), "{text}");
        assert!(text.contains("BringBack"), "{text}");
        assert!(!text.contains('\u{202E}'), "{text}");
        assert_snapshot(
            &format!("identity-hidden-{width}x{height}"),
            &draw(&mut app, width, height),
        );
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.status.as_deref(),
        Some("Brought back help@fast.example")
    );
    assert_eq!(
        app.store.list_identities(jmap).expect("identities").len(),
        3
    );
    assert!(
        app.store
            .list_hidden_identities(jmap)
            .expect("hidden")
            .is_empty()
    );

    // Proton: no adding, and the address is Proton's.
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Down);
    keys(&mut app, "c");
    // (On the first row, letters type the new password.)
    key(&mut app, KeyCode::Down);
    keys(&mut app, "a");
    assert!(
        app.status
            .as_deref()
            .is_some_and(|status| status.contains("Proton's settings")),
        "{:?}",
        app.status
    );
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    let form = app
        .edit
        .as_ref()
        .and_then(|edit| edit.form.as_ref())
        .expect("the form is open");
    assert!(!form.address_editable);
    assert_eq!(app.edit.as_ref().map(|edit| edit.account.id), Some(proton));
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
