//! Adding an account (UI plan Phase 6): the first-run onboarding, the Add
//! Account window, autodiscovery with its ISPDB consent, the manual form
//! and the cleartext opt-in.
//!
//! Discovery runs against a fake network ([`fake_network`]), so nothing
//! here leaves the machine; the stored accounts use `example.test` hosts,
//! which never resolve, for the first sync the store starts.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, px, size};
use url::Url;
use zmail_autodiscover::{Fetch, FetchResponse, Network, PortProbe, Security};
use zmail_core::{AccountColor, ProtocolKind};

use super::{Harness, Setup, click, drawn, keys, open_workspace, open_workspace_on, text};
use crate::account_setup::{
    AccountField, AccountSetupServices, AccountSetupView, Stage, open_window,
};
use crate::pane::Pane;
use crate::{InitialView, OnboardingStep, WorkspaceView};

const AUTOCONFIG_URL: &str = "https://autoconfig.example.test/mail/config-v1.1.xml";

/// Serves canned HTTPS responses (404 for anything else) and records what
/// was asked for.
#[derive(Default)]
struct FakeWeb {
    pages: HashMap<String, String>,
    requested: Mutex<Vec<String>>,
}

impl Fetch for FakeWeb {
    fn get(&self, url: &Url) -> Result<FetchResponse, String> {
        self.requested
            .lock()
            .map_err(|_| "poisoned".to_owned())?
            .push(url.to_string());
        let (status, body) = match self.pages.get(url.as_str()) {
            Some(body) => (200, body.clone().into_bytes()),
            None => (404, Vec::new()),
        };
        Ok(FetchResponse {
            status,
            url: url.clone(),
            body,
        })
    }
}

/// Every port is closed.
struct NoPorts;

impl PortProbe for NoPorts {
    fn reachable(&self, _: &str, _: u16) -> bool {
        false
    }
}

/// Points discovery at `web`.
fn fake_network(web: &Arc<FakeWeb>, cx: &mut VisualTestContext) {
    let network = Network {
        fetch: web.clone(),
        dns: None,
        probe: Arc::new(NoPorts),
    };
    cx.update(|_, cx| cx.global_mut::<AccountSetupServices>().network = network);
}

/// An autoconfig file for example.test.
fn autoconfig() -> String {
    r#"<?xml version="1.0"?>
<clientConfig version="1.1"><emailProvider id="example.test">
  <incomingServer type="imap"><hostname>imap.example.test</hostname><port>993</port>
    <socketType>SSL</socketType><authentication>password-cleartext</authentication>
    <username>%EMAILADDRESS%</username></incomingServer>
  <outgoingServer type="smtp"><hostname>smtp.example.test</hostname><port>465</port>
    <socketType>SSL</socketType><authentication>password-cleartext</authentication>
    <username>%EMAILADDRESS%</username></outgoingServer>
</emailProvider></clientConfig>"#
        .to_owned()
}

/// The main window's first-run onboarding.
fn onboarding(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> Entity<AccountSetupView> {
    cx.read_entity(view, |view, _| {
        view.onboarding
            .as_ref()
            .map(|(onboarding, _)| onboarding.clone())
            .expect("the onboarding shows")
    })
}

fn stage(setup: &Entity<AccountSetupView>, cx: &mut VisualTestContext) -> Stage {
    cx.read_entity(setup, |setup, _| setup.stage)
}

/// Types into one of the flow's fields.
fn fill(
    setup: &Entity<AccountSetupView>,
    field: AccountField,
    value: &str,
    cx: &mut VisualTestContext,
) {
    let input = cx.read_entity(setup, |setup, _| setup.form.input(field).clone());
    cx.update(|_, cx| input.update(cx, |input, cx| input.set_text(value, cx)));
    cx.run_until_parked();
}

fn errors(setup: &Entity<AccountSetupView>, cx: &mut VisualTestContext) -> Vec<AccountField> {
    cx.read_entity(setup, |setup, _| {
        setup.form.errors.keys().copied().collect()
    })
}

/// The only account in the store.
fn only_account(harness: &Harness) -> zmail_core::AccountSummary {
    let mut accounts = harness.store.list_accounts().expect("accounts load");
    assert_eq!(accounts.len(), 1, "one account was added");
    accounts.remove(0)
}

/// The open Add Account window's flow and a context for it.
fn add_account_window(cx: &mut VisualTestContext) -> (Entity<AccountSetupView>, VisualTestContext) {
    let handle = cx
        .update(|_, cx| open_window(cx))
        .expect("the Add Account window is open");
    let setup = handle
        .read_with(cx, |root, _| root.setup().clone())
        .expect("the window is open");
    (setup, VisualTestContext::from_window(handle.into(), cx))
}

/// Whether the Add Account window is open.
fn add_account_window_open(cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| open_window(cx).is_some_and(|handle| handle.read_with(cx, |_, _| ()).is_ok()))
}

#[gpui::test]
fn first_launch_shows_the_onboarding_instead_of_the_panes(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::NoAccount);
    let setup = onboarding(&view, cx);
    assert_eq!(stage(&setup, cx), Stage::Welcome);
    assert!(drawn("account-setup-get-started".into(), cx));
    assert!(!drawn("sidebar-scrim".into(), cx));

    // Secondary+Enter gets started; there is nothing to go back to from
    // Welcome, and Escape does not close the first-run flow.
    keys("escape", cx);
    assert_eq!(stage(&setup, cx), Stage::Welcome);
    keys("secondary-enter", cx);
    assert_eq!(stage(&setup, cx), Stage::Provider);

    // Add Account (from the menu) goes to the onboarding rather than a
    // second flow in a window.
    keys("secondary-shift-a", cx);
    assert!(!add_account_window_open(cx));

    // Google's sign-in step says it needs an app password.
    click("account-setup-provider-google", cx);
    assert_eq!(stage(&setup, cx), Stage::Credentials);
    assert!(drawn("account-setup-app-password".into(), cx));
    assert!(!drawn("account-setup-oauth-only".into(), cx));

    // Continuing with nothing filled in reports the fields, with the first
    // one focused, and typing clears its error.
    click("account-setup-continue", cx);
    assert_eq!(
        errors(&setup, cx),
        [AccountField::Email, AccountField::Password]
    );
    cx.simulate_input("me@example.test");
    let email = cx.read_entity(&setup, |setup, _| {
        setup.form.input(AccountField::Email).clone()
    });
    assert_eq!(text(&email, cx), "me@example.test");
    assert_eq!(errors(&setup, cx), [AccountField::Password]);

    // Back returns to the provider step.
    click("account-setup-back", cx);
    assert_eq!(stage(&setup, cx), Stage::Provider);
}

#[gpui::test]
fn microsoft_365_starts_on_oauth_under_advanced(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::NoAccount);
    let setup = onboarding(&view, cx);
    keys("secondary-enter", cx);
    click("account-setup-provider-microsoft365", cx);
    assert!(drawn("account-setup-oauth-only".into(), cx));
    cx.read_entity(&setup, |setup, _| {
        assert!(setup.form.use_oauth);
        assert!(setup.advanced_open, "the client fields show");
    });

    // The client ID is required; no password is asked for.
    fill(&setup, AccountField::Email, "me@example.test", cx);
    click("account-setup-continue", cx);
    assert_eq!(errors(&setup, cx), [AccountField::OauthClientId]);
    assert_eq!(stage(&setup, cx), Stage::Credentials);
}

#[gpui::test]
fn google_oauth_needs_the_client_secret_and_microsoft_does_not(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::NoAccount);
    let setup = onboarding(&view, cx);
    let secret_label = |setup: &Entity<AccountSetupView>, cx: &mut VisualTestContext| {
        cx.read_entity(setup, |setup, _| {
            setup.form.label(AccountField::OauthClientSecret)
        })
    };
    keys("secondary-enter", cx);

    // Google: OAuth under Advanced asks for the ID and the secret.
    click("account-setup-provider-google", cx);
    cx.update(|_, cx| {
        setup.update(cx, |setup, cx| {
            setup.toggle_advanced(cx);
            setup.set_use_oauth(true, cx);
        })
    });
    cx.run_until_parked();
    assert_eq!(secret_label(&setup, cx), "OAuth client secret");
    fill(&setup, AccountField::Email, "me@example.test", cx);
    fill(
        &setup,
        AccountField::OauthClientId,
        "1234-abc.apps.googleusercontent.com",
        cx,
    );
    click("account-setup-continue", cx);
    assert_eq!(errors(&setup, cx), [AccountField::OauthClientSecret]);

    // Microsoft 365's public clients have no secret.
    click("account-setup-back", cx);
    click("account-setup-provider-microsoft365", cx);
    assert_eq!(secret_label(&setup, cx), "OAuth client secret (optional)");
    fill(&setup, AccountField::OauthClientSecret, "", cx);
    let values = cx.read_entity(&setup, |setup, cx| setup.form.values(cx));
    assert!(
        crate::account_setup::validate_account_form(
            &values,
            crate::account_setup::Check::Credentials
        )
        .is_empty(),
        "an empty secret is fine for Microsoft 365"
    );
}

#[gpui::test]
fn discovery_asks_before_ispdb_and_falls_back_to_the_manual_form(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::NoAccount);
    let web = Arc::new(FakeWeb::default());
    fake_network(&web, cx);
    let setup = onboarding(&view, cx);
    keys("secondary-enter", cx);
    click("account-setup-provider-other", cx);
    fill(&setup, AccountField::Name, "Me", cx);
    fill(&setup, AccountField::Email, "me@example.test", cx);
    fill(&setup, AccountField::Password, "secret", cx);
    click("account-setup-continue", cx);

    // The domain's own files came up empty: ask before contacting Mozilla.
    assert_eq!(stage(&setup, cx), Stage::Consent);
    assert!(
        !web.requested
            .lock()
            .expect("lock")
            .iter()
            .any(|url| url.contains("thunderbird.net")),
        "nothing was sent to Mozilla before consent"
    );
    click("account-setup-consent-no", cx);

    // Nothing else answers: the manual form, pre-filled with the usual
    // names and TLS.
    assert_eq!(stage(&setup, cx), Stage::Manual);
    assert!(drawn("account-setup-not-found".into(), cx));
    let values = cx.read_entity(&setup, |setup, cx| setup.form.values(cx));
    assert_eq!(values.imap_host, "imap.example.test");
    assert_eq!(values.smtp_host, "smtp.example.test");
    assert_eq!(
        (values.imap_security, values.smtp_security),
        (Security::Tls, Security::Tls)
    );
    assert!(
        !web.requested
            .lock()
            .expect("lock")
            .iter()
            .any(|url| url.contains("thunderbird.net")),
        "declining kept the domain from Mozilla"
    );

    // An unencrypted SMTP server needs the explicit opt-in.
    cx.update(|_, cx| {
        setup.update(cx, |setup, cx| {
            setup.form.set_smtp_security(Security::None, cx);
            cx.notify();
        });
    });
    // Tall enough for the whole form, so clicks land on what they aim at.
    cx.simulate_resize(size(px(1200.), px(1600.)));
    cx.run_until_parked();
    assert!(drawn("account-setup-cleartext".into(), cx));
    let values = cx.read_entity(&setup, |setup, cx| setup.form.values(cx));
    assert_eq!(
        values.smtp_port, "587",
        "the port followed the security choice"
    );
    click("account-setup-add", cx);
    assert_eq!(errors(&setup, cx), [AccountField::AllowCleartext]);
    assert!(harness.store.list_accounts().expect("accounts").is_empty());

    click("account-setup-allow-cleartext", cx);
    click("account-setup-add", cx);
    assert_eq!(stage(&setup, cx), Stage::Colour);
    let account = only_account(&harness);
    assert_eq!(account.endpoint, "imaps://imap.example.test:993");
    assert_eq!(account.display_name, "me@example.test");
}

#[gpui::test]
fn discovered_settings_are_shown_with_their_source_then_stored(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::NoAccount);
    let web = Arc::new(FakeWeb {
        pages: HashMap::from([(AUTOCONFIG_URL.to_owned(), autoconfig())]),
        ..FakeWeb::default()
    });
    fake_network(&web, cx);
    let setup = onboarding(&view, cx);
    keys("secondary-enter", cx);
    click("account-setup-provider-other", cx);
    fill(&setup, AccountField::Email, "me@example.test", cx);
    fill(&setup, AccountField::Password, "secret", cx);
    // Enter in a field continues.
    keys("enter", cx);

    assert_eq!(stage(&setup, cx), Stage::Result);
    assert_eq!(
        web.requested.lock().expect("lock").as_slice(),
        [AUTOCONFIG_URL],
        "discovery stopped at the domain's own file"
    );
    click("account-setup-add", cx);
    assert_eq!(stage(&setup, cx), Stage::Colour);
    let account = only_account(&harness);
    assert_eq!(account.protocol, ProtocolKind::Imap);
    assert_eq!(account.endpoint, "imaps://imap.example.test:993");

    // The colour step stores a choice other than the derived colour, and
    // the main window shows the account behind the onboarding.
    click("account-setup-colour-violet", cx);
    click("account-setup-continue", cx);
    assert_eq!(stage(&setup, cx), Stage::Syncing);
    assert_eq!(only_account(&harness).color(), AccountColor::Violet);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.selected_account_id),
        Some(account.id)
    );

    // Done ends the onboarding: the panes come back with the list focused.
    click("account-setup-done", cx);
    assert!(cx.read_entity(&view, |view, _| view.onboarding.is_none()));
    assert_eq!(super::focused_pane(&view, cx), Some(Pane::MessageList));
}

#[gpui::test]
fn add_account_opens_a_window_that_escape_closes(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace(cx, Setup::Mail);
    keys("secondary-shift-a", cx);
    let (setup, mut window_cx) = add_account_window(cx);
    assert_eq!(stage(&setup, &mut window_cx), Stage::Provider);

    // Escape on the first step cancels.
    window_cx.simulate_keystrokes("escape");
    window_cx.run_until_parked();
    assert!(!add_account_window_open(cx));
}

#[gpui::test]
fn the_window_can_open_on_the_add_account_window(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace_on(cx, Setup::Mail, |_| InitialView::AddAccount);
    cx.run_until_parked();
    let (setup, mut window_cx) = add_account_window(cx);
    assert_eq!(stage(&setup, &mut window_cx), Stage::Provider);
}

#[gpui::test]
fn every_onboarding_fixture_step_renders(cx: &mut TestAppContext) {
    for step in [
        OnboardingStep::Welcome,
        OnboardingStep::Provider,
        OnboardingStep::Credentials,
        OnboardingStep::Discovering,
        OnboardingStep::Consent,
        OnboardingStep::Result,
        OnboardingStep::Manual,
        OnboardingStep::ManualCleartext,
        OnboardingStep::Colour,
        OnboardingStep::Syncing,
    ] {
        let mut app = cx.new_app();
        let (view, window_cx, _harness) = open_workspace_on(&mut app, Setup::Mail, move |_| {
            InitialView::Onboarding(step)
        });
        let setup = onboarding(&view, window_cx);
        let expected = match step {
            OnboardingStep::Welcome => Stage::Welcome,
            OnboardingStep::Provider => Stage::Provider,
            OnboardingStep::Credentials => Stage::Credentials,
            OnboardingStep::Discovering => Stage::Discovering,
            OnboardingStep::Consent => Stage::Consent,
            OnboardingStep::Result => Stage::Result,
            OnboardingStep::Manual | OnboardingStep::ManualCleartext => Stage::Manual,
            OnboardingStep::Colour => Stage::Colour,
            OnboardingStep::Syncing => Stage::Syncing,
        };
        assert_eq!(stage(&setup, window_cx), expected, "{step:?}");
        if step == OnboardingStep::ManualCleartext {
            assert!(drawn("account-setup-cleartext".into(), window_cx));
        }
    }
}

#[gpui::test]
fn a_provider_preset_fills_the_server_fields(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::NoAccount);
    let setup = onboarding(&view, cx);
    let email = zmail_autodiscover::EmailAddress::parse("me@example.test").expect("address");
    cx.update(|_, cx| {
        setup.update(cx, |setup, cx| {
            let settings = zmail_autodiscover::Provider::ICloud.settings(&email);
            setup.form.apply_settings(&settings, cx);
        });
    });
    let values = cx.read_entity(&setup, |setup, cx| setup.form.values(cx));
    assert_eq!(values.protocol, ProtocolKind::Imap);
    assert_eq!(
        (values.imap_host.as_str(), values.imap_port.as_str()),
        ("imap.mail.me.com", "993")
    );
    assert_eq!(
        (
            values.smtp_host.as_str(),
            values.smtp_port.as_str(),
            values.smtp_security
        ),
        ("smtp.mail.me.com", "587", Security::StartTls)
    );
    assert_eq!(values.imap_username, "me@example.test");
}
