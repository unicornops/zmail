//! Canned states of the flow for the UI screenshot matrix. Each fills the
//! view as if the user had got there, with made-up values on the reserved
//! `example.com` domain and without touching the network.

use gpui::{Context, Window};
use zmail_autodiscover::{
    AuthMethod, Discovery, EmailAddress, Found, Provider, Security, ServerSettings, Source, Step,
    StepFindings,
};
use zmail_core::AccountColor;

use super::{
    AccountField, AccountSetupServices, AccountSetupView, Check, OnboardingStep, Stage,
    validate_account_form,
};

/// A made-up Google client, shaped like a real one.
const FIXTURE_CLIENT_ID: &str = "123456789012-fixture0example.apps.googleusercontent.com";
const FIXTURE_SECRET: &str = "GOCSPX-fixture-not-a-secret";

const NAME: &str = "Sam Taylor";
const EMAIL: &str = "sam.taylor@example.com";

impl AccountSetupView {
    /// Puts the flow into `step`'s screenshot state.
    pub(crate) fn show_fixture(
        &mut self,
        step: OnboardingStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(email) = EmailAddress::parse(EMAIL) else {
            return;
        };
        let fill = |view: &mut Self, provider: Option<Provider>, cx: &mut Context<Self>| {
            view.form.set_provider(provider, cx);
            view.form.set_text(AccountField::Name, NAME, cx);
            view.form.set_text(AccountField::Email, EMAIL, cx);
            view.form
                .set_text(AccountField::Password, "app-password-1234", cx);
        };
        let mut path = vec![Stage::Provider, Stage::Credentials];
        if self.first_run {
            path.insert(0, Stage::Welcome);
        }
        let stage = match step {
            OnboardingStep::Welcome => Stage::Welcome,
            OnboardingStep::Provider => Stage::Provider,
            OnboardingStep::Credentials => {
                fill(self, Some(Provider::Google), cx);
                Stage::Credentials
            }
            OnboardingStep::GoogleGuide | OnboardingStep::GoogleGuideInvalid => {
                fill(self, Some(Provider::Google), cx);
                self.form.set_text(AccountField::Password, "", cx);
                self.form.use_oauth = true;
                path.push(Stage::Credentials);
                if step == OnboardingStep::GoogleGuideInvalid {
                    // The secret pasted as the ID, and the ID as the secret.
                    self.form
                        .set_text(AccountField::OauthClientId, FIXTURE_SECRET, cx);
                    self.form
                        .set_text(AccountField::OauthClientSecret, FIXTURE_CLIENT_ID, cx);
                    let values = self.form.values(cx);
                    self.form.errors = validate_account_form(&values, Check::Credentials)
                        .into_iter()
                        .collect();
                } else {
                    self.form
                        .set_text(AccountField::OauthClientId, FIXTURE_CLIENT_ID, cx);
                }
                Stage::GoogleGuide
            }
            OnboardingStep::Discovering => {
                fill(self, None, cx);
                let mut discovery = Discovery::new(email, false);
                discovery.record(Step::DomainAutoconfig, Ok(StepFindings::default()));
                self.discovery = Some(discovery);
                self.running_step = Some(Step::WellKnownJmap);
                Stage::Discovering
            }
            OnboardingStep::Consent => {
                fill(self, None, cx);
                let mut discovery = Discovery::new(email, false);
                discovery.record(Step::DomainAutoconfig, Ok(StepFindings::default()));
                discovery.record(
                    Step::WellKnownJmap,
                    Err("error sending request: dns error: no record found".to_owned()),
                );
                self.discovery = Some(discovery);
                Stage::Consent
            }
            OnboardingStep::Result => {
                fill(self, None, cx);
                let source = Source::DomainAutoconfig {
                    url: "https://autoconfig.example.com/mail/config-v1.1.xml".to_owned(),
                };
                let server = |host: &str, port, security| {
                    Some(Found::new(
                        ServerSettings {
                            host: host.to_owned(),
                            port,
                            security,
                            username: EMAIL.to_owned(),
                            auth: AuthMethod::Password,
                        },
                        source.clone(),
                    ))
                };
                let mut discovery = Discovery::new(email, false);
                discovery.record(
                    Step::DomainAutoconfig,
                    Ok(StepFindings {
                        imap: server("imap.example.com", 993, Security::Tls),
                        smtp: server("smtp.example.com", 465, Security::Tls),
                        ..StepFindings::default()
                    }),
                );
                self.discovery = Some(discovery);
                self.finish_discovery(window, cx);
                path.push(Stage::Discovering);
                Stage::Result
            }
            OnboardingStep::Manual | OnboardingStep::ManualCleartext => {
                fill(self, None, cx);
                let mut discovery = Discovery::new(email, false);
                for step in [Step::DomainAutoconfig, Step::WellKnownJmap] {
                    discovery.record(step, Ok(StepFindings::default()));
                }
                discovery.set_ispdb_consent(false);
                discovery.record(Step::HostnameGuess, Ok(StepFindings::default()));
                self.discovery = Some(discovery);
                self.finish_discovery(window, cx);
                if step == OnboardingStep::ManualCleartext {
                    self.form.set_smtp_security(Security::None, cx);
                    self.sync_selects(cx);
                }
                path.push(Stage::Discovering);
                Stage::Manual
            }
            OnboardingStep::Colour | OnboardingStep::Syncing => {
                let store = cx.global::<AccountSetupServices>().store.clone();
                let Some(account) = store
                    .list_accounts()
                    .ok()
                    .and_then(|accounts| accounts.into_iter().next())
                else {
                    return;
                };
                fill(self, None, cx);
                self.form
                    .set_text(AccountField::Email, &account.username, cx);
                self.account_id = Some(account.id);
                self.color = AccountColor::derived(account.id);
                path.clear();
                if step == OnboardingStep::Syncing {
                    self.refresh_sync(cx);
                    Stage::Syncing
                } else {
                    // Show a choice other than the default.
                    self.color = AccountColor::Violet;
                    Stage::Colour
                }
            }
        };
        if let Some(index) = path.iter().position(|earlier| *earlier == stage) {
            path.truncate(index);
        }
        self.history = path;
        self.stage = stage;
        self.focus_stage(window, cx);
        cx.notify();
    }
}
