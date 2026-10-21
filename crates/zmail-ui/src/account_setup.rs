//! Adding a mail account: first-run onboarding and the Add Account window.
//!
//! [`AccountSetupView`] walks through the steps of [`Stage`]:
//!
//! 1. **Welcome** (first run only).
//! 2. **Provider:** Google, Microsoft 365, Fastmail, iCloud or Other.
//! 3. **Sign in:** name, address and password. OAuth2 and its client
//!    fields sit under Advanced until zmail has client IDs of its own
//!    (#89); Microsoft 365, which refuses passwords, opens it. Gmail keeps
//!    app passwords as the default and offers a guide to creating a
//!    Google OAuth client of your own instead (`google_guide.rs`).
//! 4. **Finding settings:** a provider preset needs none. For Other,
//!    `zmail_autodiscover` runs one step at a time off the main thread with
//!    progress, and stops to ask before the ISPDB lookup, which sends the
//!    domain to Mozilla.
//! 5. **Result**, or the **manual form** when nothing was found (or the
//!    user wants to edit): separate host and port fields and a security
//!    Select per server. TLS is the default; an unencrypted connection
//!    needs an explicit opt-in.
//! 6. **Colour**, then the **first sync** with progress.
//!
//! The account is stored at the end of step 5. On first launch the main
//! window shows this view instead of its panes; afterwards
//! [`open_add_account`] opens it in an Add Account window of its own, from
//! the sidebar, the menus or Settings → Accounts. The view reads its
//! services from the [`AccountSetupServices`] global, so any window can
//! host it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Global,
    SharedString, Subscription, Task, Window,
};
use zmail_autodiscover::{Discovery, EmailAddress, MailSettings, Provider, Step};
use zmail_core::{AccountColor, ProtocolKind, SyncPhase};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use crate::actions::{Cancel, ContinueSetup};
use crate::components::{Select, SelectEvent, TextInputEvent};

mod fixture;
mod form;
mod google_guide;
#[cfg(feature = "proton")]
mod proton;
mod render;
mod window;

pub(crate) use form::{AccountField, AccountFormValues, Check, validate_account_form};
#[cfg(test)]
pub(crate) use window::open_window;

use form::{
    AccountForm, AccountSetupSecret, IMAP_SECURITY, SMTP_SECURITY, build_account_from_form,
    security_label,
};

/// The key context the view sets.
pub(crate) const KEY_CONTEXT: &str = "AccountSetup";

/// How often the first-sync step re-reads the account's sync status.
const SYNC_POLL_INTERVAL: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Services and entry points
// ---------------------------------------------------------------------------

/// What adding an account needs from the app. `zmail_ui::run` (and the
/// main window, for tests) installs it as a global.
#[derive(Clone)]
pub(crate) struct AccountSetupServices {
    pub(crate) store: Arc<LocalStore>,
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) sync_service: SyncService,
    /// What autodiscovery talks to; tests swap in fakes.
    pub(crate) network: zmail_autodiscover::Network,
}

impl Global for AccountSetupServices {}

/// Opens the add-account flow in the Add Account window, at the provider
/// step, or brings that window forward when it is already open. Settings →
/// Accounts, the sidebar and the Account menu all come here.
pub(crate) fn open_add_account(cx: &mut App) {
    open_add_account_with_notice(None, cx);
}

/// [`open_add_account`], explaining at the top why it opened when the user
/// did not ask for it (an account that can't sync, say).
pub(crate) fn open_add_account_with_notice(notice: Option<SharedString>, cx: &mut App) {
    window::open(notice, cx);
}

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

/// A step of the flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// What zmail is, and a Get Started button (first run only).
    Welcome,
    /// Pick the provider.
    Provider,
    /// Name, address and password (or OAuth2 under Advanced).
    Credentials,
    /// Gmail with a Google OAuth client of the user's own: the steps to
    /// create one, and its fields.
    GoogleGuide,
    /// Autodiscovery with progress.
    Discovering,
    /// May zmail ask Mozilla's ISPDB?
    Consent,
    /// What discovery found, and where.
    Result,
    /// Server settings by hand.
    Manual,
    /// The account is stored: pick its colour.
    Colour,
    /// The first sync, with progress.
    Syncing,
    /// Proton Mail: address and password, with the risk note.
    #[cfg(feature = "proton")]
    ProtonSignIn,
    /// Proton Mail: human verification in the browser.
    #[cfg(feature = "proton")]
    ProtonVerification,
    /// Proton Mail: the two-factor code.
    #[cfg(feature = "proton")]
    ProtonTotp,
    /// Proton Mail: the mailbox password.
    #[cfg(feature = "proton")]
    ProtonMailboxPassword,
}

#[cfg(feature = "proton")]
impl Stage {
    /// Whether this is one of the Proton sign-in steps.
    fn is_proton(self) -> bool {
        matches!(
            self,
            Stage::ProtonSignIn
                | Stage::ProtonVerification
                | Stage::ProtonTotp
                | Stage::ProtonMailboxPassword
        )
    }
}

/// A state of the flow for the UI screenshot matrix
/// (`zmail-app launch-ui-fixture`), filled with made-up values and no
/// network. See `account_setup/fixture.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStep {
    /// The welcome step.
    Welcome,
    /// The provider choice.
    Provider,
    /// Google's sign-in step, with its app password note.
    Credentials,
    /// The guide to a Google OAuth client of your own, part filled in.
    GoogleGuide,
    /// The guide with a client pasted the wrong way round, and the
    /// errors it gets.
    GoogleGuideInvalid,
    /// Autodiscovery part way through.
    Discovering,
    /// The ISPDB consent question.
    Consent,
    /// Settings found in the domain's own autoconfig file.
    Result,
    /// The manual form after discovery found nothing.
    Manual,
    /// The manual form with an unencrypted SMTP server chosen.
    ManualCleartext,
    /// The colour step for the first seeded account.
    Colour,
    /// The first sync of the first seeded account.
    Syncing,
}

/// What the flow tells its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccountSetupEvent {
    /// The account was stored; `message` tells the user what happens next.
    /// The flow carries on with the colour and first-sync steps.
    Added { account_id: i64, message: String },
    /// The user finished (or, before any account was stored, cancelled)
    /// the flow.
    Finished { account_id: Option<i64> },
    /// Storing the account failed.
    Error(String),
}

/// The first sync's progress, read from the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SyncProgress {
    pub(crate) phase: Option<SyncPhase>,
    pub(crate) detail: String,
    pub(crate) error: Option<String>,
    pub(crate) folders: usize,
    pub(crate) conversations: usize,
    /// The sync has finished at least once.
    pub(crate) done: bool,
}

// ---------------------------------------------------------------------------
// AccountSetupView
// ---------------------------------------------------------------------------

pub(crate) struct AccountSetupView {
    focus_handle: FocusHandle,
    /// Whether this is the main window's first-run onboarding (it starts
    /// at Welcome and can't be cancelled) rather than the Add Account
    /// window.
    first_run: bool,
    pub(crate) stage: Stage,
    /// The stages Back returns to.
    history: Vec<Stage>,
    pub(crate) form: AccountForm,
    /// Why the flow opened when the user did not ask for it.
    pub(crate) notice: Option<SharedString>,
    /// Whether the sign-in step shows its Advanced (OAuth2) fields.
    pub(crate) advanced_open: bool,
    pub(crate) discovery: Option<Discovery>,
    /// The discovery step running now.
    pub(crate) running_step: Option<Step>,
    discovery_task: Option<Task<()>>,
    /// What discovery (or a preset) found, shown on the result step.
    pub(crate) found: Option<MailSettings>,
    /// The manual form's controls.
    pub(crate) protocol_select: Entity<Select>,
    pub(crate) imap_security_select: Entity<Select>,
    pub(crate) smtp_security_select: Entity<Select>,
    /// The stored account, from the colour step on.
    pub(crate) account_id: Option<i64>,
    pub(crate) color: AccountColor,
    pub(crate) sync: SyncProgress,
    sync_task: Option<Task<()>>,
    /// The Proton sign-in in progress.
    #[cfg(feature = "proton")]
    pub(crate) proton: proton::ProtonFlow,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AccountSetupEvent> for AccountSetupView {}

impl Focusable for AccountSetupView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AccountSetupView {
    /// A new flow: first-run onboarding starts at Welcome, the Add Account
    /// window at the provider step.
    pub(crate) fn new(first_run: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let form = AccountForm::new(cx);
        let protocol_select =
            cx.new(|cx| Select::new(["IMAP and SMTP", "JMAP"], Some(0), cx).label("Protocol"));
        let imap_security_select = cx.new(|cx| {
            Select::new(IMAP_SECURITY.map(security_label), Some(0), cx).label("IMAP security")
        });
        let smtp_security_select = cx.new(|cx| {
            Select::new(SMTP_SECURITY.map(security_label), Some(0), cx).label("SMTP security")
        });
        let mut subscriptions = Self::watch_inputs(&form, window, cx);
        subscriptions.push(
            cx.subscribe(&protocol_select, |this, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = event;
                this.form.protocol = if *index == 1 {
                    ProtocolKind::Jmap
                } else {
                    ProtocolKind::Imap
                };
                cx.notify();
            }),
        );
        subscriptions.push(cx.subscribe(
            &imap_security_select,
            |this, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = event;
                if let Some(security) = IMAP_SECURITY.get(*index) {
                    this.form.set_imap_security(*security, cx);
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &smtp_security_select,
            |this, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = event;
                if let Some(security) = SMTP_SECURITY.get(*index) {
                    this.form.set_smtp_security(*security, cx);
                    cx.notify();
                }
            },
        ));
        Self {
            focus_handle: cx.focus_handle(),
            first_run,
            stage: if first_run {
                Stage::Welcome
            } else {
                Stage::Provider
            },
            history: Vec::new(),
            form,
            notice: None,
            advanced_open: false,
            discovery: None,
            running_step: None,
            discovery_task: None,
            found: None,
            protocol_select,
            imap_security_select,
            smtp_security_select,
            account_id: None,
            color: AccountColor::Blue,
            sync: SyncProgress::default(),
            sync_task: None,
            #[cfg(feature = "proton")]
            proton: proton::ProtonFlow::default(),
            _subscriptions: subscriptions,
        }
    }

    /// Sets the note shown at the top of the flow.
    pub(crate) fn set_notice(&mut self, notice: Option<SharedString>, cx: &mut Context<Self>) {
        self.notice = notice;
        cx.notify();
    }

    /// Subscribes to the text inputs: editing a field clears its error,
    /// and Enter continues to the next step.
    fn watch_inputs(
        form: &AccountForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        AccountField::TEXT
            .into_iter()
            .map(|field| {
                cx.subscribe_in(
                    form.input(field),
                    window,
                    move |this: &mut Self, _, event: &TextInputEvent, window, cx| match event {
                        TextInputEvent::Changed => {
                            if this.form.errors.remove(&field).is_some() {
                                cx.notify();
                            }
                        }
                        TextInputEvent::Submitted => this.advance(window, cx),
                    },
                )
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Moving between stages
    // -----------------------------------------------------------------------

    /// Moves to `stage`, remembering the current one for Back.
    fn go(&mut self, stage: Stage, window: &mut Window, cx: &mut Context<Self>) {
        if stage != self.stage {
            self.history.push(self.stage);
            self.stage = stage;
        }
        self.focus_stage(window, cx);
        cx.notify();
    }

    /// Focuses the first thing to act on in the current stage.
    pub(crate) fn focus_stage(&self, window: &mut Window, cx: &mut App) {
        let field = match self.stage {
            Stage::Credentials => Some(if self.form.text(AccountField::Email, cx).is_empty() {
                AccountField::Name
            } else {
                AccountField::Password
            }),
            Stage::GoogleGuide => Some(if self.form.text(AccountField::Email, cx).is_empty() {
                AccountField::Email
            } else {
                AccountField::OauthClientId
            }),
            Stage::Manual => Some(match self.form.protocol {
                ProtocolKind::Imap => AccountField::ImapHost,
                ProtocolKind::Jmap => AccountField::JmapUrl,
                ProtocolKind::Proton => AccountField::Email,
            }),
            #[cfg(feature = "proton")]
            Stage::ProtonSignIn => Some(if self.form.text(AccountField::Email, cx).is_empty() {
                AccountField::Email
            } else {
                AccountField::Password
            }),
            #[cfg(feature = "proton")]
            Stage::ProtonTotp => Some(AccountField::ProtonTotp),
            #[cfg(feature = "proton")]
            Stage::ProtonMailboxPassword => Some(AccountField::ProtonMailboxPassword),
            _ => None,
        };
        match field {
            Some(field) => self
                .form
                .input(field)
                .update(cx, |input, cx| input.focus(window, cx)),
            None => window.focus(&self.focus_handle, cx),
        }
    }

    /// Back: the previous stage. Leaving discovery stops it; nothing goes
    /// back past a stored account.
    pub(crate) fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account_id.is_some() {
            return;
        }
        // Leaving the guide goes back to the app password.
        if self.stage == Stage::GoogleGuide {
            self.form.use_oauth = false;
            self.form.errors.clear();
        }
        // Leaving a Proton step signs the unfinished session out; signing
        // in starts again from the password.
        #[cfg(feature = "proton")]
        if self.stage.is_proton() {
            self.proton_abort(cx);
            while self
                .history
                .last()
                .is_some_and(|stage| stage.is_proton() && *stage != Stage::ProtonSignIn)
            {
                self.history.pop();
            }
            if self.stage != Stage::ProtonSignIn
                && self.history.last() == Some(&Stage::ProtonSignIn)
            {
                self.history.pop();
                self.stage = Stage::ProtonSignIn;
                self.focus_stage(window, cx);
                cx.notify();
                return;
            }
        }
        // Discovery and consent are not places to go back to: Back from
        // the result or the manual form returns to the sign-in step.
        while let Some(stage) = self.history.pop() {
            if !matches!(stage, Stage::Discovering | Stage::Consent) {
                self.stage = stage;
                self.discovery_task = None;
                self.running_step = None;
                self.focus_stage(window, cx);
                cx.notify();
                return;
            }
        }
    }

    /// Whether Back has somewhere to go.
    pub(crate) fn can_go_back(&self) -> bool {
        self.account_id.is_none()
            && self
                .history
                .iter()
                .any(|stage| !matches!(stage, Stage::Discovering | Stage::Consent))
    }

    /// Escape: back a step, or close the Add Account window from its first
    /// step.
    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_go_back() {
            self.back(window, cx);
        } else if !self.first_run && self.account_id.is_none() {
            cx.emit(AccountSetupEvent::Finished { account_id: None });
        }
    }

    fn continue_action(&mut self, _: &ContinueSetup, window: &mut Window, cx: &mut Context<Self>) {
        self.advance(window, cx);
    }

    /// The current stage's main action (Enter, Secondary+Enter, or its
    /// primary button).
    pub(crate) fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.stage {
            Stage::Welcome => self.go(Stage::Provider, window, cx),
            // Picking a provider is the action here.
            Stage::Provider | Stage::Discovering | Stage::Consent => {}
            Stage::Credentials | Stage::GoogleGuide => self.submit_credentials(window, cx),
            Stage::Result | Stage::Manual => self.save(window, cx),
            Stage::Colour => self.confirm_colour(window, cx),
            Stage::Syncing => self.finish(cx),
            #[cfg(feature = "proton")]
            Stage::ProtonSignIn
            | Stage::ProtonVerification
            | Stage::ProtonTotp
            | Stage::ProtonMailboxPassword => self.proton_advance(window, cx),
        }
    }

    /// The provider step: remember the choice and ask for the sign-in.
    pub(crate) fn choose_provider(
        &mut self,
        provider: Option<Provider>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.form.set_provider(provider, cx);
        // Microsoft 365 refuses passwords: start on OAuth2, whose client
        // fields are under Advanced.
        let oauth = provider.is_some_and(Provider::needs_oauth2);
        self.form.use_oauth = oauth;
        self.advanced_open = oauth;
        self.form.errors.clear();
        self.go(Stage::Credentials, window, cx);
    }

    /// Gmail's "Set up your own client": the guide, signing in with
    /// OAuth2 instead of an app password.
    pub(crate) fn open_google_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form.use_oauth = true;
        self.form.errors.clear();
        self.go(Stage::GoogleGuide, window, cx);
    }

    /// Shows or hides the Advanced fields.
    pub(crate) fn toggle_advanced(&mut self, cx: &mut Context<Self>) {
        self.advanced_open = !self.advanced_open;
        cx.notify();
    }

    /// Switches between password and OAuth2 sign-in.
    pub(crate) fn set_use_oauth(&mut self, use_oauth: bool, cx: &mut Context<Self>) {
        self.form.use_oauth = use_oauth;
        self.form.errors.clear();
        cx.notify();
    }

    /// Turns the cleartext opt-in on or off.
    pub(crate) fn toggle_cleartext(&mut self, cx: &mut Context<Self>) {
        self.form.allow_cleartext = !self.form.allow_cleartext;
        self.form.errors.remove(&AccountField::AllowCleartext);
        cx.notify();
    }

    /// Shows the errors from a check, focusing the first text field with
    /// one. Returns whether there were any.
    fn show_errors(
        &mut self,
        errors: Vec<(AccountField, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.form.errors = errors.into_iter().collect();
        if let Some(field) = self
            .form
            .errors
            .keys()
            .find(|field| AccountField::TEXT.contains(field))
        {
            self.form
                .input(*field)
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
        !self.form.errors.is_empty()
    }

    /// The sign-in step's Continue: a provider preset fills the servers
    /// and stores the account; Other runs autodiscovery.
    fn submit_credentials(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = self.form.values(cx);
        if self.show_errors(
            validate_account_form(&values, Check::Credentials),
            window,
            cx,
        ) {
            return;
        }
        let Ok(email) = EmailAddress::parse(&values.email) else {
            return;
        };
        match values.provider {
            Some(provider) => {
                let settings = provider.settings(&email);
                self.form.apply_settings(&settings, cx);
                self.sync_selects(cx);
                self.found = Some(settings);
                self.save(window, cx);
            }
            None => self.start_discovery(email, window, cx),
        }
    }

    // -----------------------------------------------------------------------
    // Autodiscovery
    // -----------------------------------------------------------------------

    /// Starts discovery for `email`.
    fn start_discovery(
        &mut self,
        email: EmailAddress,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let network = &cx.global::<AccountSetupServices>().network;
        self.discovery = Some(Discovery::new(email, network.dns.is_some()));
        self.found = None;
        self.go(Stage::Discovering, window, cx);
        self.run_next_step(window, cx);
    }

    /// Runs discovery's next step in the background, or moves on when it
    /// has a result, needs consent, or has run out of steps.
    fn run_next_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(discovery) = &self.discovery else {
            return;
        };
        if discovery.needs_consent() {
            self.go(Stage::Consent, window, cx);
            return;
        }
        let Some(step) = discovery.next_step() else {
            self.finish_discovery(window, cx);
            return;
        };
        let email = discovery.email().clone();
        let network = cx.global::<AccountSetupServices>().network.clone();
        self.running_step = Some(step);
        let run = cx
            .background_spawn(async move { zmail_autodiscover::run_step(step, &email, &network) });
        self.discovery_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = run.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.running_step = None;
                if let Some(discovery) = &mut this.discovery {
                    discovery.record(step, result);
                }
                this.run_next_step(window, cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The consent step's answer.
    pub(crate) fn answer_consent(
        &mut self,
        granted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(discovery) = &mut self.discovery else {
            return;
        };
        discovery.set_ispdb_consent(granted);
        // Consent is a pause in discovery, not a stage Back returns to.
        self.stage = Stage::Discovering;
        self.run_next_step(window, cx);
        cx.notify();
    }

    /// Discovery is over: show what it found, or the manual form.
    fn finish_discovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(discovery) = &self.discovery else {
            return;
        };
        match discovery.result() {
            Some(settings) => {
                self.form.apply_settings(&settings, cx);
                self.sync_selects(cx);
                self.found = Some(settings);
                self.go(Stage::Result, window, cx);
            }
            None => {
                let email = discovery.email().clone();
                let (imap, smtp) = discovery.partial();
                let (imap, smtp) = (imap.cloned(), smtp.cloned());
                if let Some(imap) = imap {
                    self.form.apply_imap(&imap.value, cx);
                }
                if let Some(smtp) = smtp {
                    self.form.apply_smtp(&smtp.value, cx);
                }
                self.form.prefill_guesses(&email, cx);
                self.sync_selects(cx);
                self.go(Stage::Manual, window, cx);
            }
        }
    }

    /// The result step's "Edit settings": the manual form, filled in.
    pub(crate) fn edit_manually(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.go(Stage::Manual, window, cx);
    }

    /// Puts the manual form's Selects in step with the form's values.
    fn sync_selects(&mut self, cx: &mut Context<Self>) {
        let protocol = match self.form.protocol {
            // The manual form never holds a Proton account.
            ProtocolKind::Imap | ProtocolKind::Proton => 0,
            ProtocolKind::Jmap => 1,
        };
        let imap = IMAP_SECURITY
            .iter()
            .position(|security| *security == self.form.imap_security);
        let smtp = SMTP_SECURITY
            .iter()
            .position(|security| *security == self.form.smtp_security);
        self.protocol_select
            .update(cx, |select, cx| select.set_selected(Some(protocol), cx));
        self.imap_security_select
            .update(cx, |select, cx| select.set_selected(imap, cx));
        self.smtp_security_select
            .update(cx, |select, cx| select.set_selected(smtp, cx));
    }

    // -----------------------------------------------------------------------
    // Storing the account
    // -----------------------------------------------------------------------

    /// Validates everything and stores the account, then moves to the
    /// colour step.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = self.form.values(cx);
        let errors = validate_account_form(&values, Check::All);
        if !errors.is_empty() {
            // Server problems are fixed in the manual form; sign-in ones
            // where they were typed.
            let stage = if errors.iter().any(|(field, _)| {
                matches!(
                    field,
                    AccountField::Name
                        | AccountField::Email
                        | AccountField::Password
                        | AccountField::OauthClientId
                        | AccountField::OauthClientSecret
                        | AccountField::OauthAuthorizationEndpoint
                        | AccountField::OauthTokenEndpoint
                        | AccountField::OauthScopes
                )
            }) {
                if self.stage == Stage::GoogleGuide {
                    Stage::GoogleGuide
                } else {
                    self.advanced_open |= values.use_oauth;
                    Stage::Credentials
                }
            } else {
                Stage::Manual
            };
            self.go(stage, window, cx);
            self.show_errors(errors, window, cx);
            return;
        }
        let services = cx.global::<AccountSetupServices>().clone();
        match store_account(&services, &values) {
            Ok((account_id, message)) => {
                self.account_id = Some(account_id);
                self.color = AccountColor::derived(account_id);
                self.history.clear();
                cx.emit(AccountSetupEvent::Added {
                    account_id,
                    message,
                });
                self.go(Stage::Colour, window, cx);
            }
            Err(error) => cx.emit(AccountSetupEvent::Error(error.to_string())),
        }
    }

    /// The colour step: pick a colour.
    pub(crate) fn pick_colour(&mut self, color: AccountColor, cx: &mut Context<Self>) {
        self.color = color;
        cx.notify();
    }

    /// The colour step's Continue: store the colour (only when it differs
    /// from the derived one, so the default stays derived) and start
    /// watching the first sync.
    fn confirm_colour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.account_id else {
            return;
        };
        let derived = AccountColor::derived(account_id);
        let color = (self.color != derived).then_some(self.color);
        let store = cx.global::<AccountSetupServices>().store.clone();
        if let Err(error) = store.set_account_color(account_id, color) {
            cx.emit(AccountSetupEvent::Error(error.to_string()));
        }
        // The account's colour shows in the sidebar straight away.
        cx.emit(AccountSetupEvent::Added {
            account_id,
            message: String::new(),
        });
        self.go(Stage::Syncing, window, cx);
        self.watch_sync(window, cx);
    }

    /// Re-reads the account's sync status until the first sync finishes.
    fn watch_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_sync(cx);
        self.sync_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(SYNC_POLL_INTERVAL).await;
                let Ok(done) = this.update(cx, |this, cx| {
                    this.refresh_sync(cx);
                    this.sync.done || this.sync.error.is_some()
                }) else {
                    return;
                };
                if done {
                    return;
                }
            }
        }));
    }

    /// Reads the first sync's progress from the store.
    pub(crate) fn refresh_sync(&mut self, cx: &mut Context<Self>) {
        let Some(account_id) = self.account_id else {
            return;
        };
        let store = cx.global::<AccountSetupServices>().store.clone();
        let Some(account) = store.list_accounts().ok().and_then(|accounts| {
            accounts
                .into_iter()
                .find(|account| account.id == account_id)
        }) else {
            return;
        };
        let mailboxes = store.list_mailboxes(account_id).unwrap_or_default();
        let status = account.sync_status;
        let progress = SyncProgress {
            phase: Some(status.phase),
            detail: status.detail,
            error: (status.phase == SyncPhase::Failed)
                .then(|| status.last_error_message.unwrap_or_default()),
            folders: mailboxes.len(),
            conversations: mailboxes.iter().map(|mailbox| mailbox.total_threads).sum(),
            done: status.phase == SyncPhase::Idle && status.last_success_at.is_some(),
        };
        if progress != self.sync {
            self.sync = progress;
            cx.notify();
        }
    }

    /// The last step's Done: the host closes the flow.
    fn finish(&mut self, cx: &mut Context<Self>) {
        self.sync_task = None;
        cx.emit(AccountSetupEvent::Finished {
            account_id: self.account_id,
        });
    }
}

/// Stores a validated form and returns the new account id and the status
/// message for the user. Then starts the first sync, or the OAuth2 browser
/// flow that ends in it.
fn store_account(
    services: &AccountSetupServices,
    form: &AccountFormValues,
) -> Result<(i64, String)> {
    let (account, secret) = build_account_from_form(form)?;
    let account_id = services.store.insert_account(&account)?;
    let message = match secret {
        AccountSetupSecret::Password(secret) => {
            services.secrets.set_account_secret(account_id, &secret)?;
            let _ = services.sync_service.sync_account(account_id);
            format!("{} added.", account.display_name)
        }
        AccountSetupSecret::OAuth2(client) => {
            // The browser flow blocks for up to its redirect timeout, so it
            // runs off the GUI thread; the account's sync status marks the
            // wait, and the grant (tokens, client config, expiry) is
            // persisted before the first sync starts. Syncing here would
            // only record a missing-credentials failure.
            let _ = services.store.begin_sync_attempt(
                account_id,
                SyncPhase::Connecting,
                "Waiting for browser authorization",
            );
            spawn_oauth2_authorization(services, account_id, client);
            format!(
                "{} added. Finish signing in in your browser; the account syncs once you approve access.",
                account.display_name
            )
        }
    };
    // Start the new account's background watcher immediately instead of
    // waiting for the next app launch.
    let _ = services.sync_service.sync_watchers();
    Ok((account_id, message))
}

/// Runs the OAuth2 browser flow on a background thread and stores the
/// resulting grant, so the GUI never blocks on the redirect wait.
fn spawn_oauth2_authorization(
    services: &AccountSetupServices,
    account_id: i64,
    client: zmail_auth::OAuth2Client,
) {
    let store = services.store.clone();
    let secrets = services.secrets.clone();
    let sync_service = services.sync_service.clone();
    let spawn = std::thread::Builder::new()
        .name("zmail-oauth2-setup".to_owned())
        .spawn(move || {
            let tokens = match client.authorize_interactively() {
                Ok(tokens) => tokens,
                Err(error) => {
                    let _ = store.record_sync_failure(
                        account_id,
                        "Browser authorization failed",
                        error.to_string(),
                    );
                    return;
                }
            };
            if let Err(error) = zmail_sync::persist_oauth2_grant(
                &store,
                secrets.as_ref(),
                account_id,
                &client,
                &tokens,
            ) {
                let _ = store.record_sync_failure(
                    account_id,
                    "Storing the authorization failed",
                    error.to_string(),
                );
                return;
            }
            let _ = sync_service.sync_account(account_id);
            let _ = sync_service.sync_watchers();
        });
    if let Err(error) = spawn {
        let _ = services.store.record_sync_failure(
            account_id,
            "Browser authorization failed",
            error.to_string(),
        );
    }
}
