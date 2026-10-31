//! Setup → Accounts: Alpine's collection list for mail accounts, with Add,
//! Change and Delete (ADR 0007, phase 4).
//!
//! Everything that isn't drawing comes from `zmail-setup`, which the GUI's
//! onboarding uses too: presets, autodiscovery step by step, validation,
//! the connection test, storing (secrets only in the keychain), the OAuth2
//! browser sign-in and Proton's sign-in steps.
//!
//! **Add** goes step by step: the address; the provider (a preset for
//! consumer domains, else autodiscovery with each step's progress, asking
//! before Mozilla's ISPDB); how to sign in (password, app password with the
//! provider's hint, or OAuth2 with a client of your own); the servers,
//! found or typed; for OAuth2, the browser; a connection test; the
//! identity; then the account is saved and its first sync starts, here when
//! this Zmail holds the sync lock, otherwise in the Zmail that does.
//!
//! Safety:
//!
//! - Password fields never echo: they show `[hidden]` once typed, and
//!   nothing about their length. They live only in the step's form, never
//!   in a [`crate::app::Prompt`] (which derives `Debug`), and are cleared
//!   once the account is saved. Secrets go only to the keychain.
//! - Every string from a server, DNS or Proton (discovery notes and
//!   errors, hosts, URLs, connection errors, sync progress) goes through
//!   [`sanitize_line`] before it is drawn or put on the status line.
//! - Over SSH (`SSH_CONNECTION`/`SSH_TTY`) the OAuth2 step says plainly
//!   that the browser's redirect to 127.0.0.1 can't reach this machine.
//!   There is no device flow: Google doesn't allow the Gmail scope with
//!   it. The URL is always shown, so it can be opened by hand.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use zmail_auth::{OAuth2Client, TokenSet};
use zmail_autodiscover::{Discovery, EmailAddress, MailSettings, Provider, Security, Source};
use zmail_core::{AccountSummary, AuthKind, ProtocolKind};
use zmail_security::terminal::sanitize_line;
use zmail_setup::{
    AccountField, AccountFormValues, AccountSetupSecret, Check, SyncProgress,
    accounts::{self, RemoveError},
    build_account_from_form,
    check::{self, CheckFailure, CheckPart},
    discovery::{self, Next, Outcome},
    form::{IMAP_SECURITY, SMTP_SECURITY},
    oauth::{BrowserSignIn, CancelSignIn, REDIRECT_TIMEOUT},
    providers, validate_account_form,
};

use crate::app::{App, CommandEntry, Effect, Notice, Prompt, Screen, entry};

/// How to sign in, as the method step offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The account's password.
    Password,
    /// An app password (the provider's hint shows).
    AppPassword,
    /// OAuth2 in the browser, with a client of your own.
    OAuth,
}

impl Method {
    /// The method's line on the method step.
    pub fn label(self, provider: Option<Provider>) -> String {
        match (self, provider) {
            (Self::Password, _) => "Password".to_owned(),
            (Self::AppPassword, Some(provider)) => format!("App password for {provider}"),
            (Self::AppPassword, None) => "App password".to_owned(),
            (Self::OAuth, Some(Provider::Google)) => {
                "OAuth in the browser, with a Google OAuth client of your own".to_owned()
            }
            (Self::OAuth, Some(Provider::Microsoft365)) => {
                "OAuth in the browser, with your organisation's client ID".to_owned()
            }
            (Self::OAuth, _) => "OAuth in the browser, with your provider's client".to_owned(),
        }
    }
}

/// The ways `provider` can be signed in to, the usual first.
pub fn methods(provider: Option<Provider>) -> Vec<Method> {
    match provider {
        // Microsoft 365 refuses passwords.
        Some(provider) if provider.needs_oauth2() => vec![Method::OAuth],
        Some(provider) if provider.needs_app_password() => {
            vec![Method::AppPassword, Method::OAuth]
        }
        _ => vec![Method::Password, Method::AppPassword, Method::OAuth],
    }
}

/// A step of Add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The email address.
    Address,
    /// Autodiscovery, step by step, with the ISPDB question.
    Discovering,
    /// How to sign in.
    Method,
    /// The name and the password, or the OAuth2 client.
    Credentials,
    /// The servers, found or typed.
    Servers,
    /// OAuth2: the browser sign-in.
    Browser,
    /// The connection test.
    Testing,
    /// The identity mail is sent as.
    Identity,
    /// Saved: the first sync's progress.
    Done,
    /// Proton Mail: the username and password, with the risk note.
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

impl Step {
    /// Whether one of Proton's steps.
    #[cfg_attr(not(feature = "proton"), allow(dead_code))]
    pub(crate) fn is_proton(self) -> bool {
        #[cfg(feature = "proton")]
        {
            matches!(
                self,
                Step::ProtonSignIn
                    | Step::ProtonVerification
                    | Step::ProtonTotp
                    | Step::ProtonMailboxPassword
            )
        }
        #[cfg(not(feature = "proton"))]
        {
            false
        }
    }
}

/// A row of a step's form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// A text field.
    Field(AccountField),
    /// IMAP and SMTP, or JMAP.
    Protocol,
    /// The IMAP server's security.
    ImapSecurity,
    /// The SMTP server's security.
    SmtpSecurity,
    /// The opt-in to an unencrypted connection.
    Cleartext,
}

impl Row {
    /// The row's label.
    pub fn label(self, form: &AccountFormValues) -> &'static str {
        match self {
            Row::Field(AccountField::OauthClientSecret) => {
                zmail_setup::form::oauth_secret_label(form.provider)
            }
            Row::Field(AccountField::Name) => "Your name",
            Row::Field(field) => field.label(),
            Row::Protocol => "Protocol",
            Row::ImapSecurity | Row::SmtpSecurity => "Security",
            Row::Cleartext => "Connect without encryption",
        }
    }
}

/// What background work reports back.
pub enum SetupEvent {
    /// A discovery step finished.
    Discovered {
        /// The Add it belongs to.
        generation: u64,
        /// The step.
        step: zmail_autodiscover::Step,
        /// What it found.
        result: zmail_autodiscover::StepResult,
    },
    /// The connection test moved on to `part`.
    Checking {
        /// The Add it belongs to.
        generation: u64,
        /// The half being tested.
        part: CheckPart,
    },
    /// The connection test finished.
    Checked {
        /// The Add it belongs to.
        generation: u64,
        /// How it went.
        result: Result<(), CheckFailure>,
    },
    /// The browser sign-in finished.
    Authorized {
        /// The Add it belongs to.
        generation: u64,
        /// The client and its tokens, or why not.
        result: Result<(OAuth2Client, TokenSet), String>,
    },
    /// Signing in again in the browser finished (Change).
    Reauthorized {
        /// The account.
        account_id: i64,
        /// Why it failed, if it did.
        error: Option<String>,
    },
    /// A Proton step answered.
    #[cfg(feature = "proton")]
    Proton {
        /// The Add it belongs to.
        generation: u64,
        /// Its answer.
        result: anyhow::Result<zmail_proton::SignInStep>,
    },
    /// The Proton session was kept, or not.
    #[cfg(feature = "proton")]
    ProtonStored {
        /// The Add it belongs to.
        generation: u64,
        /// The account stored.
        account_id: i64,
        /// Its name.
        name: String,
        /// Whether the session was kept.
        result: anyhow::Result<()>,
    },
}

impl std::fmt::Debug for SetupEvent {
    /// Leaves tokens and results out: they may carry secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Discovered { .. } => "Discovered",
            Self::Checking { .. } => "Checking",
            Self::Checked { .. } => "Checked",
            Self::Authorized { .. } => "Authorized",
            Self::Reauthorized { .. } => "Reauthorized",
            #[cfg(feature = "proton")]
            Self::Proton { .. } => "Proton",
            #[cfg(feature = "proton")]
            Self::ProtonStored { .. } => "ProtonStored",
        };
        f.debug_struct("SetupEvent")
            .field("event", &name)
            .finish_non_exhaustive()
    }
}

/// Adding an account.
pub struct AddAccount {
    /// The step shown.
    pub step: Step,
    history: Vec<Step>,
    /// The form, filled in as the steps go.
    pub form: AccountFormValues,
    /// How to sign in.
    pub method: Method,
    /// The method step's cursor.
    pub method_cursor: usize,
    /// The focused row of the step's form.
    pub focus: usize,
    /// Errors beside their fields.
    pub errors: BTreeMap<AccountField, String>,
    /// The step's error that belongs to no one field (untrusted text,
    /// filtered when drawn).
    pub error: Option<String>,
    /// Autodiscovery, when the address has no preset.
    pub discovery: Option<Discovery>,
    /// The discovery step running.
    pub running: Option<zmail_autodiscover::Step>,
    /// What a preset or discovery found.
    pub found: Option<MailSettings>,
    /// The browser sign-in's URL.
    pub sign_in_url: Option<String>,
    /// Stops the browser sign-in's wait, freeing its port.
    browser: Option<CancelSignIn>,
    /// The browser sign-in's result.
    tokens: Option<(OAuth2Client, TokenSet)>,
    /// The half of the connection test running.
    pub checking: Option<CheckPart>,
    /// The saved account.
    pub account_id: Option<i64>,
    /// Its first sync's progress.
    pub progress: Option<SyncProgress>,
    /// Background work for an older state of this Add is ignored.
    generation: u64,
    /// The Proton sign-in.
    #[cfg(feature = "proton")]
    pub proton: zmail_setup::proton::ProtonFlow,
    /// The Proton step's field (the code or the mailbox password).
    #[cfg(feature = "proton")]
    pub proton_input: String,
}

impl AddAccount {
    fn new(generation: u64) -> Self {
        Self {
            step: Step::Address,
            history: Vec::new(),
            form: AccountFormValues::default(),
            method: Method::Password,
            method_cursor: 0,
            focus: 0,
            errors: BTreeMap::new(),
            error: None,
            discovery: None,
            running: None,
            found: None,
            sign_in_url: None,
            browser: None,
            tokens: None,
            checking: None,
            account_id: None,
            progress: None,
            generation,
            #[cfg(feature = "proton")]
            proton: zmail_setup::proton::ProtonFlow::default(),
            #[cfg(feature = "proton")]
            proton_input: String::new(),
        }
    }

    /// The rows of the current step's form.
    pub fn rows(&self) -> Vec<Row> {
        let field = Row::Field;
        match self.step {
            Step::Address => vec![field(AccountField::Email)],
            Step::Credentials if self.form.use_oauth => {
                let mut rows = vec![
                    field(AccountField::Name),
                    field(AccountField::OauthClientId),
                    field(AccountField::OauthClientSecret),
                ];
                if !has_oauth_preset(&self.form) {
                    rows.extend([
                        field(AccountField::OauthAuthorizationEndpoint),
                        field(AccountField::OauthTokenEndpoint),
                        field(AccountField::OauthScopes),
                    ]);
                }
                rows
            }
            Step::Credentials => vec![field(AccountField::Name), field(AccountField::Password)],
            Step::Servers => {
                let mut rows = vec![Row::Protocol];
                match self.form.protocol {
                    ProtocolKind::Jmap => rows.push(field(AccountField::JmapUrl)),
                    _ => rows.extend([
                        field(AccountField::ImapHost),
                        field(AccountField::ImapPort),
                        Row::ImapSecurity,
                        field(AccountField::ImapUsername),
                        field(AccountField::SmtpHost),
                        field(AccountField::SmtpPort),
                        Row::SmtpSecurity,
                        field(AccountField::SmtpUsername),
                    ]),
                }
                if self.form.uses_cleartext() {
                    rows.push(Row::Cleartext);
                }
                rows
            }
            Step::Identity => vec![
                field(AccountField::Name),
                field(AccountField::IdentityEmail),
                field(AccountField::Signature),
            ],
            #[cfg(feature = "proton")]
            Step::ProtonSignIn => vec![field(AccountField::Email), field(AccountField::Password)],
            #[cfg(feature = "proton")]
            Step::ProtonTotp => vec![field(AccountField::ProtonTotp)],
            #[cfg(feature = "proton")]
            Step::ProtonMailboxPassword => vec![field(AccountField::ProtonMailboxPassword)],
            _ => Vec::new(),
        }
    }

    /// The focused row.
    pub fn focused(&self) -> Option<Row> {
        let rows = self.rows();
        rows.get(self.focus.min(rows.len().saturating_sub(1)))
            .copied()
    }

    /// Whether a Proton step is running, which shows below the form
    /// rather than above it.
    pub fn proton_busy(&self) -> bool {
        #[cfg(feature = "proton")]
        {
            self.step.is_proton() && self.proton.busy
        }
        #[cfg(not(feature = "proton"))]
        {
            false
        }
    }

    /// A text field's value.
    pub fn value(&self, field: AccountField) -> &str {
        match field {
            #[cfg(feature = "proton")]
            AccountField::ProtonTotp | AccountField::ProtonMailboxPassword => &self.proton_input,
            field => self.form.value(field),
        }
    }

    fn value_mut(&mut self, field: AccountField) -> Option<&mut String> {
        let form = &mut self.form;
        Some(match field {
            AccountField::Name => &mut form.name,
            AccountField::Email => &mut form.email,
            AccountField::Password => &mut form.password,
            AccountField::OauthClientId => &mut form.oauth_client_id,
            AccountField::OauthClientSecret => &mut form.oauth_client_secret,
            AccountField::OauthAuthorizationEndpoint => &mut form.oauth_authorization_endpoint,
            AccountField::OauthTokenEndpoint => &mut form.oauth_token_endpoint,
            AccountField::OauthScopes => &mut form.oauth_scopes,
            AccountField::JmapUrl => &mut form.jmap_url,
            AccountField::ImapHost => &mut form.imap_host,
            AccountField::ImapPort => &mut form.imap_port,
            AccountField::ImapUsername => &mut form.imap_username,
            AccountField::SmtpHost => &mut form.smtp_host,
            AccountField::SmtpPort => &mut form.smtp_port,
            AccountField::SmtpUsername => &mut form.smtp_username,
            AccountField::IdentityEmail => &mut form.identity_email,
            AccountField::Signature => &mut form.signature,
            #[cfg(feature = "proton")]
            AccountField::ProtonTotp | AccountField::ProtonMailboxPassword => {
                &mut self.proton_input
            }
            _ => return None,
        })
    }

    /// Moves to `step`, remembering this one for Back.
    fn go(&mut self, step: Step) {
        if step != self.step {
            self.history.push(self.step);
            self.step = step;
        }
        self.focus = 0;
    }

    /// Back to `step`, an earlier one, forgetting the steps after it.
    fn return_to(&mut self, step: Step) {
        if let Some(position) = self.history.iter().rposition(|earlier| *earlier == step) {
            self.history.truncate(position);
        }
        self.step = step;
        self.focus = 0;
    }

    /// Stops waiting for the browser, if Add is.
    fn stop_browser(&mut self) {
        if let Some(cancel) = self.browser.take() {
            cancel.cancel();
        }
    }

    /// Focuses the first field with an error, if it is on this step.
    fn focus_error(&mut self) {
        let rows = self.rows();
        if let Some(position) = rows.iter().position(|row| match row {
            Row::Field(field) => self.errors.contains_key(field),
            Row::ImapSecurity => self.errors.contains_key(&AccountField::ImapSecurity),
            Row::Cleartext => self.errors.contains_key(&AccountField::AllowCleartext),
            _ => false,
        }) {
            self.focus = position;
        }
    }

    /// Forgets the secrets typed so far.
    fn forget_secrets(&mut self) {
        for secret in [&mut self.form.password, &mut self.form.oauth_client_secret] {
            wipe(secret);
        }
        #[cfg(feature = "proton")]
        wipe(&mut self.proton_input);
        self.tokens = None;
        self.stop_browser();
    }
}

/// `text` with a capital first letter, for an error chain shown as a
/// sentence.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Overwrites a secret's bytes before dropping them, as far as safe Rust
/// can (the allocator may have kept earlier copies).
fn wipe(secret: &mut String) {
    let len = secret.len();
    secret.replace_range(.., &"\0".repeat(len));
    secret.clear();
}

/// Whether the form's provider has an OAuth2 preset (Gmail, Microsoft 365
/// over IMAP), so no endpoints are asked for.
fn has_oauth_preset(form: &AccountFormValues) -> bool {
    matches!(
        (form.provider, form.protocol),
        (
            Some(Provider::Google | Provider::Microsoft365),
            ProtocolKind::Imap
        )
    )
}

/// Changing an account.
pub struct EditAccount {
    /// The account.
    pub account: AccountSummary,
    /// How it signs in.
    pub auth: AuthKind,
    /// The focused row: 0 signing in again, 1 the cache budget.
    pub cursor: usize,
    /// A new password being typed.
    pub password: String,
    /// The account's cache budget, in MiB.
    pub budget: Option<u64>,
    /// The browser sign-in's URL, while signing in again.
    pub sign_in_url: Option<String>,
    /// Stops that sign-in's wait.
    browser: Option<CancelSignIn>,
}

/// The rows of Change.
pub const EDIT_ROWS: usize = 2;

impl App {
    /// The step Add is on, if an account is being added (for tests that
    /// drive the app from outside the crate, such as the live harness).
    pub fn add_step(&self) -> Option<Step> {
        self.add.as_ref().map(|add| add.step)
    }

    /// The error Add shows, if any (filtered when drawn, not here).
    pub fn add_error(&self) -> Option<&str> {
        self.add.as_ref().and_then(|add| add.error.as_deref())
    }

    // ------------------------------------------------------------------
    // The Accounts list
    // ------------------------------------------------------------------

    /// Setup → Accounts.
    pub(crate) fn open_accounts(&mut self) {
        self.accounts_cursor = self
            .accounts_cursor
            .min(self.accounts.len().saturating_sub(1));
        self.goto(Screen::Accounts);
    }

    /// The account the cursor is on.
    fn account_at_cursor(&self) -> Option<AccountSummary> {
        self.accounts.get(self.accounts_cursor).cloned()
    }

    /// How an account signs in.
    pub(crate) fn account_auth(&self, account_id: i64) -> AuthKind {
        self.store
            .sync_target(account_id)
            .ok()
            .flatten()
            .map_or(AuthKind::Password, |target| target.auth)
    }

    /// Whether this Zmail can add accounts: it needs the keychain.
    fn can_add_accounts(&mut self) -> bool {
        if self.secrets.is_none() {
            self.status = Some(
                "This Zmail has no keychain to keep a password in, so it can't add accounts"
                    .to_owned(),
            );
            return false;
        }
        true
    }

    /// Add: the first step.
    pub(crate) fn add_account(&mut self) {
        if !self.can_add_accounts() {
            return;
        }
        self.setup_generation += 1;
        self.add = Some(AddAccount::new(self.setup_generation));
        self.goto(Screen::AddAccount);
    }

    /// Change, for the account under the cursor.
    pub(crate) fn change_account(&mut self) {
        let Some(account) = self.account_at_cursor() else {
            self.status = Some("No account to change: A adds one".to_owned());
            return;
        };
        let budget = self
            .store
            .account_disk_budget_bytes(account.id)
            .ok()
            .flatten()
            .map(|bytes| bytes / (1024 * 1024));
        let auth = self.account_auth(account.id);
        self.edit = Some(EditAccount {
            account,
            auth,
            cursor: 0,
            password: String::new(),
            budget,
            sign_in_url: None,
            browser: None,
        });
        self.goto(Screen::EditAccount);
    }

    /// Delete, for the account under the cursor: asks first.
    pub(crate) fn delete_account(&mut self) {
        let Some(account) = self.account_at_cursor() else {
            self.status = Some("No account to delete".to_owned());
            return;
        };
        match self.store.account_removal_impact(account.id) {
            Ok(impact) => {
                if let Some(blocker) = accounts::removal_blocker(&impact) {
                    self.status = Some(blocker);
                    return;
                }
                self.prompt = Some(Prompt::RemoveAccount {
                    account_id: account.id,
                    lines: accounts::removal_lines(&impact),
                });
            }
            Err(error) => self.report(error),
        }
    }

    /// The question Delete asks.
    pub(crate) fn remove_question(&self, account_id: i64, lines: &[String]) -> String {
        let name = self
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .map(|account| sanitize_line(&account.display_name))
            .unwrap_or_default();
        let mut question =
            format!("Remove {name} from this computer? Its mail on the server stays.");
        for line in lines {
            question.push(' ');
            question.push_str(line);
        }
        question.push_str(" (y/n)");
        question
    }

    /// Removes the account Delete asked about.
    pub(crate) fn remove_account_now(&mut self, account_id: i64) {
        let Some(secrets) = self.secrets.clone() else {
            self.status =
                Some("This Zmail has no keychain, so it can't remove accounts".to_owned());
            return;
        };
        let name = self
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .map(|account| sanitize_line(&account.display_name))
            .unwrap_or_default();
        match accounts::remove_account(&self.store, secrets.as_ref(), account_id) {
            Ok(()) => {
                self.status = Some(format!(
                    "Removed {name} from this computer. Its mail on the server is untouched."
                ));
            }
            Err(RemoveError::LocalData(error)) => {
                self.status = Some(format!(
                    "Could not remove {name}: {}",
                    sanitize_line(&format!("{error:#}"))
                ));
                return;
            }
            Err(RemoveError::Keychain(error)) => {
                self.status = Some(format!(
                    "Removed {name}, but its keychain entries could not all be deleted: {}",
                    sanitize_line(&format!("{error:#}"))
                ));
            }
        }
        self.refresh();
        self.accounts_cursor = self
            .accounts_cursor
            .min(self.accounts.len().saturating_sub(1));
    }

    // ------------------------------------------------------------------
    // Add: keys
    // ------------------------------------------------------------------

    /// A key on Add: the step's form takes letters as text.
    pub(crate) fn add_key(&mut self, key: KeyEvent) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control {
            match key.code {
                KeyCode::Char('c') => return self.cancel_add(),
                #[cfg(feature = "proton")]
                KeyCode::Char('p') if add.step == Step::Address => return self.start_proton(),
                KeyCode::Char('u') => {
                    if let Some(Row::Field(field)) = add.focused()
                        && let Some(value) = add.value_mut(field)
                    {
                        wipe(value);
                        add.errors.remove(&field);
                    }
                    return;
                }
                _ => return,
            }
        }
        if key.code == KeyCode::Esc {
            return self.add_back();
        }
        match add.step {
            Step::Discovering => return self.discovery_key(key),
            Step::Method => {
                let count = methods(add.form.provider).len();
                match key.code {
                    KeyCode::Up | KeyCode::Char('p' | 'P') => {
                        add.method_cursor = add.method_cursor.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('n' | 'N') => {
                        add.method_cursor = (add.method_cursor + 1).min(count.saturating_sub(1));
                    }
                    KeyCode::Enter | KeyCode::Char('>' | '.') => self.choose_method(),
                    KeyCode::Char('<' | ',') | KeyCode::Left => self.add_back(),
                    _ => {}
                }
                return;
            }
            Step::Browser | Step::Testing => return,
            Step::Done => {
                if matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Char('<' | ',' | '>' | '.')
                ) {
                    self.finish_add();
                }
                return;
            }
            #[cfg(feature = "proton")]
            Step::ProtonVerification => {
                if key.code == KeyCode::Enter {
                    self.proton_continue();
                }
                return;
            }
            _ => {}
        }
        let rows = add.rows();
        add.focus = add.focus.min(rows.len().saturating_sub(1));
        let row = rows.get(add.focus).copied();
        match key.code {
            KeyCode::Up | KeyCode::BackTab => add.focus = add.focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                add.focus = (add.focus + 1).min(rows.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if add.focus + 1 < rows.len() {
                    add.focus += 1;
                } else {
                    self.add_continue();
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if row.is_some_and(|row| !matches!(row, Row::Field(_))) =>
            {
                let by = if key.code == KeyCode::Left { -1 } else { 1 };
                change_choice(add, row.expect("checked"), by);
            }
            KeyCode::Backspace => {
                if let Some(Row::Field(field)) = row
                    && let Some(value) = add.value_mut(field)
                {
                    value.pop();
                    add.errors.remove(&field);
                }
            }
            KeyCode::Char(ch) if !ch.is_control() => {
                if let Some(Row::Field(field)) = row
                    && let Some(value) = add.value_mut(field)
                {
                    value.push(ch);
                    add.errors.remove(&field);
                }
            }
            _ => {}
        }
    }

    /// A paste on Add: text for the focused field, never commands.
    pub(crate) fn add_paste(&mut self, pasted: &str) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        if let Some(Row::Field(field)) = add.focused()
            && let Some(value) = add.value_mut(field)
        {
            value.push_str(&sanitize_line(pasted));
            add.errors.remove(&field);
        }
    }

    /// A paste on Change: the new password, when its row is focused.
    pub(crate) fn edit_paste(&mut self, pasted: &str) {
        if let Some(edit) = self.edit.as_mut()
            && edit.cursor == 0
            && edit.auth == AuthKind::Password
        {
            edit.password.push_str(&sanitize_line(pasted));
        }
    }

    /// Esc: back a step, stopping what runs; from the first step, back to
    /// the Accounts list.
    fn add_back(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        if add.account_id.is_some() {
            return self.finish_add();
        }
        // Work still running for the step left is ignored when it ends.
        self.setup_generation += 1;
        add.generation = self.setup_generation;
        add.running = None;
        add.checking = None;
        add.error = None;
        add.stop_browser();
        #[cfg(feature = "proton")]
        if add.step.is_proton() {
            if let Some(abort) = add.proton.abort() {
                self.run_on_sync(abort);
            }
            let add = self.add.as_mut().expect("checked above");
            wipe(&mut add.proton_input);
            // Back to the sign-in, then to the address.
            add.history.retain(|step| !step.is_proton());
            if add.step != Step::ProtonSignIn {
                add.step = Step::ProtonSignIn;
                add.focus = 0;
                return;
            }
        }
        let add = self.add.as_mut().expect("checked above");
        // Discovery is not a step to go back to.
        while let Some(step) = add.history.pop() {
            if step != Step::Discovering && step != Step::Testing && step != Step::Browser {
                add.step = step;
                add.focus = 0;
                return;
            }
        }
        self.cancel_add();
    }

    /// ^C: leaves Add without saving anything.
    fn cancel_add(&mut self) {
        if let Some(mut add) = self.add.take() {
            #[cfg(feature = "proton")]
            if let Some(abort) = add.proton.abort() {
                self.run_on_sync(abort);
            }
            add.forget_secrets();
        }
        self.setup_generation += 1;
        self.goto(Screen::Accounts);
        self.status = Some("No account added".to_owned());
    }

    /// Done: back to the Accounts list, on the new account.
    fn finish_add(&mut self) {
        let account_id = self.add.take().and_then(|add| add.account_id);
        self.refresh();
        if let Some(position) =
            account_id.and_then(|id| self.accounts.iter().position(|account| account.id == id))
        {
            self.accounts_cursor = position;
        }
        self.goto(Screen::Accounts);
    }

    /// Enter on the step's last row.
    fn add_continue(&mut self) {
        let Some(step) = self.add.as_ref().map(|add| add.step) else {
            return;
        };
        match step {
            Step::Address => self.submit_address(),
            Step::Credentials => self.submit_credentials(),
            Step::Servers => self.submit_servers(),
            Step::Identity => self.save_account(),
            #[cfg(feature = "proton")]
            Step::ProtonSignIn => self.proton_sign_in(),
            #[cfg(feature = "proton")]
            Step::ProtonTotp | Step::ProtonMailboxPassword => self.proton_continue(),
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Add: the address and discovery
    // ------------------------------------------------------------------

    fn submit_address(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let email = match EmailAddress::parse(add.form.email.trim()) {
            Ok(email) => email,
            Err(error) => {
                let message = if add.form.email.trim().is_empty() {
                    "Enter your email address.".to_owned()
                } else {
                    error.to_string()
                };
                add.errors.insert(AccountField::Email, message);
                return;
            }
        };
        #[cfg(feature = "proton")]
        if providers::is_proton_address(&email) {
            return self.start_proton();
        }
        add.form.email = email.to_string();
        // Back from Proton's sign-in, the account is a server account again.
        add.form.protocol = ProtocolKind::Imap;
        add.found = None;
        add.discovery = None;
        match providers::provider_for_address(&email) {
            Some(provider) => {
                let settings = provider.settings(&email);
                add.form.provider = Some(provider);
                add.form.apply_settings(&settings);
                add.found = Some(settings);
                add.method_cursor = 0;
                add.go(Step::Method);
            }
            None => {
                add.form.provider = None;
                add.discovery = Some(Discovery::new(email, self.network.dns.is_some()));
                add.go(Step::Discovering);
                self.run_discovery();
            }
        }
    }

    /// Runs discovery's next step in the background, or moves on.
    fn run_discovery(&mut self) {
        let network = self.network.clone();
        let sender = self.notice_sender();
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let Some(discovery) = &add.discovery else {
            return;
        };
        match discovery::next(discovery) {
            // The question shows on the step; Y or N answers it.
            Next::AskConsent => add.running = None,
            Next::Finished => self.finish_discovery(),
            Next::Run(step) => {
                add.running = Some(step);
                let email = discovery.email().clone();
                let generation = add.generation;
                std::thread::spawn(move || {
                    let result = zmail_autodiscover::run_step(step, &email, &network);
                    let _ = sender.send(Notice::Setup(Box::new(SetupEvent::Discovered {
                        generation,
                        step,
                        result,
                    })));
                });
            }
        }
    }

    fn discovery_key(&mut self, key: KeyEvent) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let asking =
            add.running.is_none() && add.discovery.as_ref().is_some_and(Discovery::needs_consent);
        match key.code {
            KeyCode::Char('y' | 'Y') if asking => {
                if let Some(discovery) = add.discovery.as_mut() {
                    discovery.set_ispdb_consent(true);
                }
                self.run_discovery();
            }
            KeyCode::Char('n' | 'N') if asking => {
                if let Some(discovery) = add.discovery.as_mut() {
                    discovery.set_ispdb_consent(false);
                }
                self.run_discovery();
            }
            // Enter the servers by hand: stop discovering.
            KeyCode::Char('e' | 'E') => {
                self.setup_generation += 1;
                add.generation = self.setup_generation;
                add.running = None;
                if let Some(email) = add.discovery.as_ref().map(|d| d.email().clone()) {
                    add.form.prefill_guesses(&email);
                }
                add.found = None;
                add.method_cursor = 0;
                add.go(Step::Method);
            }
            _ => {}
        }
    }

    /// Discovery is over: what it found, then how to sign in.
    fn finish_discovery(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let Some(discovery) = add.discovery.clone() else {
            return;
        };
        add.running = None;
        match discovery::finish(&discovery, &mut add.form) {
            Outcome::Found(settings) => {
                // A domain whose mail goes to a provider Zmail knows (Google
                // Workspace, say) gets that provider's sign-in rules.
                if let MailSettings::ImapSmtp { imap, .. } = &settings
                    && let Source::MxGuess { provider, .. } = imap.source
                {
                    add.form.provider = Some(provider);
                }
                add.found = Some(settings);
            }
            Outcome::Manual => add.found = None,
        }
        add.method_cursor = 0;
        add.go(Step::Method);
    }

    // ------------------------------------------------------------------
    // Add: signing in and the servers
    // ------------------------------------------------------------------

    fn choose_method(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let methods = methods(add.form.provider);
        add.method = methods
            .get(add.method_cursor)
            .copied()
            .unwrap_or(Method::Password);
        add.form.use_oauth = add.method == Method::OAuth;
        add.errors.clear();
        add.go(Step::Credentials);
        // Name first, unless it is already there.
        if !add.form.name.trim().is_empty() {
            add.focus = 1;
        }
    }

    fn submit_credentials(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let errors = validate_account_form(&add.form, Check::Credentials);
        add.errors = errors.into_iter().collect();
        if !add.errors.is_empty() {
            add.focus_error();
            return;
        }
        add.go(Step::Servers);
    }

    fn submit_servers(&mut self) {
        let over_ssh = self.env.over_ssh;
        let sender = self.notice_sender();
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let errors = validate_account_form(&add.form, Check::All);
        if !errors.is_empty() {
            let credentials = errors.iter().any(|(field, _)| {
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
            });
            add.errors = errors.into_iter().collect();
            if credentials {
                add.go(Step::Credentials);
            }
            add.focus_error();
            return;
        }
        add.errors.clear();
        add.error = None;
        if !add.form.use_oauth {
            return self.start_check();
        }
        // OAuth2: sign in in the browser first, so the test can use the
        // token.
        let client = match build_account_from_form(&add.form) {
            Ok((_, AccountSetupSecret::OAuth2(client))) => client,
            Ok(_) => return,
            Err(error) => {
                add.error = Some(format!("{error:#}"));
                return;
            }
        };
        let sign_in = BrowserSignIn::new(client);
        let url = sign_in.url().to_owned();
        add.sign_in_url = Some(url.clone());
        add.browser = Some(sign_in.canceller());
        add.go(Step::Browser);
        let generation = add.generation;
        // A browser opened over SSH is on the wrong machine; the URL is
        // on screen either way.
        if !over_ssh {
            self.effects.push(Effect::Open(url));
        }
        std::thread::spawn(move || {
            let result = sign_in
                .wait(REDIRECT_TIMEOUT)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Notice::Setup(Box::new(SetupEvent::Authorized {
                generation,
                result,
            })));
        });
    }

    /// Starts the connection test with the password or the browser's
    /// token.
    fn start_check(&mut self) {
        let runtime = self.runtime.clone();
        let sender = self.notice_sender();
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let secret = match (&add.tokens, add.form.use_oauth) {
            (Some((_, tokens)), true) => tokens.access_token.clone(),
            (None, true) => return,
            (_, false) => add.form.password.clone(),
        };
        let account = match build_account_from_form(&add.form) {
            Ok((account, _)) => account,
            Err(error) => {
                add.error = Some(format!("{error:#}"));
                return;
            }
        };
        let Some(runtime) = runtime else {
            add.error = Some("This Zmail has no network runtime to test the connection".to_owned());
            return;
        };
        add.go(Step::Testing);
        add.checking = Some(CheckPart::Incoming);
        let generation = add.generation;
        runtime.spawn(async move {
            let mut result = Ok(());
            for part in [CheckPart::Incoming, CheckPart::Outgoing] {
                let _ = sender.send(Notice::Setup(Box::new(SetupEvent::Checking {
                    generation,
                    part,
                })));
                result = check::check_part(&account, &secret, part).await;
                if result.is_err() {
                    break;
                }
            }
            let _ = sender.send(Notice::Setup(Box::new(SetupEvent::Checked {
                generation,
                result,
            })));
        });
    }

    // ------------------------------------------------------------------
    // Add: saving
    // ------------------------------------------------------------------

    fn save_account(&mut self) {
        let (Some(secrets), Some(add)) = (self.secrets.clone(), self.add.as_mut()) else {
            return;
        };
        let errors = validate_account_form(&add.form, Check::All);
        if let Some((_, message)) = errors.into_iter().next() {
            add.error = Some(message);
            return;
        }
        let saved = match add.tokens.as_ref() {
            Some((_, tokens)) if add.form.use_oauth => accounts::store_authorized_account(
                &self.store,
                secrets.as_ref(),
                self.sync.as_ref(),
                &add.form,
                tokens,
            ),
            _ => accounts::store_account(&self.store, &secrets, self.sync.as_ref(), &add.form),
        };
        match saved {
            Ok((account_id, message)) => self.account_saved(account_id, message),
            Err(error) => {
                if let Some(add) = self.add.as_mut() {
                    add.error = Some(format!("Couldn't save the account: {error:#}"));
                }
            }
        }
    }

    /// The account is in the store: forget its secrets and watch its
    /// first sync.
    fn account_saved(&mut self, account_id: i64, message: String) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        add.forget_secrets();
        add.account_id = Some(account_id);
        add.history.clear();
        add.go(Step::Done);
        self.status = Some(sanitize_line(&message));
        self.refresh();
        self.refresh_add_progress();
    }

    /// Reads the new account's first sync from the store.
    pub(crate) fn refresh_add_progress(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        if let Some(account_id) = add.account_id {
            add.progress = accounts::read_sync_progress(&self.store, account_id);
        }
    }

    // ------------------------------------------------------------------
    // Background work reporting back
    // ------------------------------------------------------------------

    /// News from Add's or Change's background work.
    pub(crate) fn setup_event(&mut self, event: SetupEvent) {
        if let SetupEvent::Reauthorized { account_id, error } = event {
            return self.reauthorized(account_id, error);
        }
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let generation = match &event {
            SetupEvent::Discovered { generation, .. }
            | SetupEvent::Checking { generation, .. }
            | SetupEvent::Checked { generation, .. }
            | SetupEvent::Authorized { generation, .. } => *generation,
            #[cfg(feature = "proton")]
            SetupEvent::Proton { generation, .. } | SetupEvent::ProtonStored { generation, .. } => {
                *generation
            }
            SetupEvent::Reauthorized { .. } => return,
        };
        if generation != add.generation {
            return;
        }
        match event {
            SetupEvent::Discovered { step, result, .. } => {
                add.running = None;
                if let Some(discovery) = add.discovery.as_mut() {
                    discovery.record(step, result);
                }
                self.run_discovery();
            }
            SetupEvent::Checking { part, .. } => add.checking = Some(part),
            SetupEvent::Checked { result, .. } => {
                add.checking = None;
                match result {
                    Ok(()) => {
                        if add.form.identity_email.trim().is_empty() {
                            add.form.identity_email = add.form.email.clone();
                        }
                        add.go(Step::Identity);
                    }
                    Err(failure) => {
                        let field = if failure.refused && add.form.use_oauth {
                            AccountField::OauthClientId
                        } else {
                            failure.field()
                        };
                        add.error = Some(failure.summary());
                        add.errors.insert(
                            field,
                            if failure.refused {
                                "The server refused this sign-in.".to_owned()
                            } else {
                                "Check this server.".to_owned()
                            },
                        );
                        // Back to where it is fixed, without the test.
                        add.return_to(if failure.refused {
                            Step::Credentials
                        } else {
                            Step::Servers
                        });
                        add.focus_error();
                    }
                }
            }
            SetupEvent::Authorized { result, .. } => {
                add.browser = None;
                match result {
                    Ok(tokens) => {
                        add.tokens = Some(tokens);
                        self.start_check();
                    }
                    Err(error) => {
                        add.error = Some(sentence(&error));
                        add.return_to(Step::Servers);
                    }
                }
            }
            #[cfg(feature = "proton")]
            SetupEvent::Proton { result, .. } => self.proton_answer(result),
            #[cfg(feature = "proton")]
            SetupEvent::ProtonStored {
                account_id,
                name,
                result,
                ..
            } => self.proton_stored(account_id, name, result),
            SetupEvent::Reauthorized { .. } => {}
        }
    }

    // ------------------------------------------------------------------
    // Change
    // ------------------------------------------------------------------

    /// A key on Change.
    pub(crate) fn edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let typing = edit.cursor == 0 && edit.auth == AuthKind::Password;
        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || control => {
                if let Some(mut edit) = self.edit.take() {
                    wipe(&mut edit.password);
                    if let Some(cancel) = edit.browser.take() {
                        cancel.cancel();
                    }
                }
                self.goto(Screen::Accounts);
            }
            KeyCode::Char('u') if control && typing => wipe(&mut edit.password),
            _ if control => {}
            KeyCode::Up | KeyCode::BackTab => edit.cursor = edit.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => edit.cursor = (edit.cursor + 1).min(EDIT_ROWS - 1),
            KeyCode::Backspace if typing => {
                edit.password.pop();
            }
            KeyCode::Char(ch) if typing && !ch.is_control() => edit.password.push(ch),
            KeyCode::Char('<' | ',') => {
                if let Some(cancel) = self.edit.take().and_then(|mut edit| edit.browser.take()) {
                    cancel.cancel();
                }
                self.goto(Screen::Accounts);
            }
            KeyCode::Enter if edit.cursor == 0 => self.sign_in_again(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ' | '>' | '.')
                if edit.cursor == 1 =>
            {
                self.change_budget(1);
            }
            KeyCode::Left if edit.cursor == 1 => self.change_budget(-1),
            _ => {}
        }
    }

    /// Change's first row: a new password, or the browser sign-in again.
    fn sign_in_again(&mut self) {
        let (Some(secrets), Some(edit)) = (self.secrets.clone(), self.edit.as_mut()) else {
            return;
        };
        let account_id = edit.account.id;
        let name = sanitize_line(&edit.account.display_name);
        match edit.auth {
            AuthKind::Password => {
                if edit.password.is_empty() {
                    self.status = Some("Type the new password, then Enter".to_owned());
                    return;
                }
                let saved = match &self.sync {
                    Some(sync) => {
                        accounts::save_password(secrets.as_ref(), sync, account_id, &edit.password)
                    }
                    None => secrets.set_account_secret(account_id, &edit.password),
                };
                wipe(&mut edit.password);
                self.status = Some(match saved {
                    Ok(()) => format!("Password saved. {name} is syncing."),
                    Err(error) => format!(
                        "Could not save the password in the keychain: {}",
                        sanitize_line(&format!("{error:#}"))
                    ),
                });
            }
            AuthKind::OAuth2 => {
                let (preset, client) = match accounts::reauthorization_client(
                    &self.store,
                    secrets.as_ref(),
                    account_id,
                ) {
                    Ok(found) => found,
                    Err(message) => {
                        self.status = Some(message.to_owned());
                        return;
                    }
                };
                let sign_in = BrowserSignIn::new(client);
                let url = sign_in.url().to_owned();
                edit.sign_in_url = Some(url.clone());
                if let Some(earlier) = edit.browser.replace(sign_in.canceller()) {
                    earlier.cancel();
                }
                if !self.env.over_ssh {
                    self.effects.push(Effect::Open(url));
                }
                let _ = self.store.begin_sync_attempt(
                    account_id,
                    zmail_core::SyncPhase::Connecting,
                    "Waiting for browser authorization",
                );
                let (store, sync, sender) =
                    (self.store.clone(), self.sync.clone(), self.notice_sender());
                std::thread::spawn(move || {
                    let (client, tokens) = match sign_in.wait(REDIRECT_TIMEOUT) {
                        Ok((client, tokens)) => (client, Ok(tokens)),
                        Err(error) => {
                            let message = format!("{error:#}");
                            let _ = store.record_sync_failure(
                                account_id,
                                "Browser authorization failed",
                                message.clone(),
                            );
                            let _ =
                                sender.send(Notice::Setup(Box::new(SetupEvent::Reauthorized {
                                    account_id,
                                    error: Some(message),
                                })));
                            return;
                        }
                    };
                    if let Some(sync) = &sync {
                        accounts::finish_reauthorization(
                            &store,
                            secrets.as_ref(),
                            sync,
                            account_id,
                            &client,
                            tokens,
                        );
                    }
                    let _ = sender.send(Notice::Setup(Box::new(SetupEvent::Reauthorized {
                        account_id,
                        error: None,
                    })));
                });
                self.status = Some(format!(
                    "Finish signing in to {} in your browser.",
                    preset.name()
                ));
            }
            AuthKind::ProtonSession => {
                self.status = Some(
                    "To sign in to Proton again, delete the account and add it again".to_owned(),
                );
            }
        }
    }

    fn reauthorized(&mut self, account_id: i64, error: Option<String>) {
        if let Some(edit) = self.edit.as_mut()
            && edit.account.id == account_id
        {
            edit.sign_in_url = None;
        }
        self.status = Some(match error {
            None => "Signed in again".to_owned(),
            Some(error) => sanitize_line(&sentence(&error)),
        });
        self.refresh();
    }

    /// Change's second row: the next (or previous) cache budget.
    fn change_budget(&mut self, by: isize) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let options = accounts::budget_options(edit.budget);
        let position = options
            .iter()
            .position(|budget| *budget == edit.budget)
            .unwrap_or(0);
        let next = (position as isize + by).rem_euclid(options.len() as isize) as usize;
        let budget = options[next];
        let account_id = edit.account.id;
        if let Err(error) = self.store.set_account_disk_budget(account_id, budget) {
            self.report(error);
            return;
        }
        edit.budget = budget;
        self.status = Some(format!("Cache budget: {}", accounts::budget_label(budget)));
        if let Some(budget_mb) = budget {
            // Clear what is over the new limit in the background, as the
            // GUI does.
            let store = self.store.clone();
            std::thread::spawn(move || {
                let _ = store.evict_account_blobs(account_id, budget_mb * 1024 * 1024);
            });
        }
    }

    // ------------------------------------------------------------------
    // The command bar
    // ------------------------------------------------------------------

    /// The command bar on the Accounts list, Add and Change.
    pub(crate) fn accounts_commands(&self) -> Vec<CommandEntry> {
        use crate::app::Cmd::*;
        match self.screen {
            Screen::Accounts => vec![
                entry("?", "Help", Help),
                entry("<", "Setup", Back),
                entry(">", "[Change]", Select),
                entry("P", "Prev", Prev),
                entry("N", "Next", Next),
                entry("A", "Add", AddAccount),
                entry("C", "Change", ChangeAccount),
                entry("D", "Delete", DeleteAccount),
                entry("M", "Main Menu", MainMenu),
                entry("Q", "Quit", Quit),
            ],
            Screen::EditAccount => vec![
                entry("Ret", "[Change]", Press(KeyCode::Enter, false)),
                entry("Tab", "NextRow", Press(KeyCode::Tab, false)),
                entry("^U", "Clear", Press(KeyCode::Char('u'), true)),
                entry("Esc", "Accounts", Press(KeyCode::Esc, false)),
            ],
            _ => self.add_commands(),
        }
    }

    fn add_commands(&self) -> Vec<CommandEntry> {
        use crate::app::Cmd::Press;
        let Some(add) = self.add.as_ref() else {
            return Vec::new();
        };
        let key = |code| Press(code, false);
        let enter = key(KeyCode::Enter);
        let cancel = entry("^C", "Cancel", Press(KeyCode::Char('c'), true));
        let back = entry("Esc", "Back", key(KeyCode::Esc));
        match add.step {
            Step::Discovering => {
                let asking = add.running.is_none()
                    && add.discovery.as_ref().is_some_and(Discovery::needs_consent);
                let mut entries = Vec::new();
                if asking {
                    entries.extend([
                        entry("Y", "AskMozilla", key(KeyCode::Char('y'))),
                        entry("N", "Don't ask", key(KeyCode::Char('n'))),
                    ]);
                }
                entries.extend([entry("E", "Manual", key(KeyCode::Char('e'))), back, cancel]);
                entries
            }
            Step::Method => vec![
                entry(">", "[Select]", enter),
                entry("P", "Prev", key(KeyCode::Up)),
                entry("N", "Next", key(KeyCode::Down)),
                back,
                cancel,
            ],
            Step::Browser | Step::Testing => vec![entry("Esc", "Stop", key(KeyCode::Esc)), cancel],
            Step::Done => vec![entry("Ret", "Done", enter)],
            #[cfg(feature = "proton")]
            Step::ProtonVerification => vec![entry("Ret", "I've finished", enter), back, cancel],
            step => {
                let last = add.focus + 1 >= add.rows().len();
                let mut entries = vec![
                    entry("Ret", if last { "Continue" } else { "NextFld" }, enter),
                    entry("Tab", "NextFld", key(KeyCode::Tab)),
                    entry("^U", "Clear", Press(KeyCode::Char('u'), true)),
                    back,
                    cancel,
                ];
                if add
                    .focused()
                    .is_some_and(|row| !matches!(row, Row::Field(_)))
                {
                    entries.insert(1, entry("Spc", "Change", key(KeyCode::Char(' '))));
                }
                #[cfg(feature = "proton")]
                if step == Step::Address {
                    entries.insert(1, entry("^P", "Proton", Press(KeyCode::Char('p'), true)));
                }
                let _ = step;
                entries
            }
        }
    }

    // ------------------------------------------------------------------
    // Proton Mail
    // ------------------------------------------------------------------

    /// Runs a future on the sync runtime, ignoring its result.
    #[cfg(feature = "proton")]
    fn run_on_sync(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        if let (Some(sync), Some(runtime)) = (&self.sync, &self.runtime) {
            let run = sync.run_proton(future);
            runtime.spawn(async move {
                let _ = run.await;
            });
        }
    }

    /// ^P on the address step (or a Proton address): Proton's sign-in.
    #[cfg(feature = "proton")]
    fn start_proton(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        add.form.provider = None;
        add.form.use_oauth = false;
        add.form.protocol = ProtocolKind::Proton;
        add.errors.clear();
        add.proton.error = None;
        add.go(Step::ProtonSignIn);
        if !add.form.email.trim().is_empty() {
            add.focus = 1;
        }
    }

    /// The sign-in step's Enter: checks, then signs in.
    #[cfg(feature = "proton")]
    fn proton_sign_in(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        if add.proton.busy {
            return;
        }
        let email = add.form.email.trim().to_owned();
        let errors = zmail_setup::proton::check_sign_in(&email, &add.form.password);
        if !errors.is_empty() {
            add.errors = errors.into_iter().collect();
            add.focus_error();
            return;
        }
        if let Some(duplicate) = zmail_setup::proton::duplicate(&self.store, &email) {
            add.proton.error = Some(duplicate);
            return;
        }
        let Some(sync) = self.sync.clone() else {
            add.proton.error = Some("This Zmail has no sync service to sign in with".to_owned());
            return;
        };
        if let Some(abort) = add.proton.abort() {
            self.run_on_sync(abort);
        }
        let add = self.add.as_mut().expect("checked above");
        let sign_in = match sync.begin_proton_sign_in(&email, &add.form.password) {
            Ok(sign_in) => sign_in,
            Err(error) => {
                add.proton.error = Some(format!("{error:#}"));
                return;
            }
        };
        let step = add.proton.start(sign_in);
        self.run_proton_step(Box::pin(step));
    }

    /// The verification, code and mailbox password steps' Enter.
    #[cfg(feature = "proton")]
    fn proton_continue(&mut self) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let input = add.proton_input.clone();
        match add.proton.submit(&input) {
            Ok(Some(step)) => self.run_proton_step(step),
            Ok(None) => {}
            Err((field, message)) => {
                add.errors.insert(field, message);
            }
        }
    }

    #[cfg(feature = "proton")]
    fn run_proton_step(
        &mut self,
        step: std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<zmail_proton::SignInStep>> + Send>,
        >,
    ) {
        let (Some(sync), Some(runtime), Some(add)) =
            (self.sync.clone(), self.runtime.clone(), self.add.as_ref())
        else {
            return;
        };
        let generation = add.generation;
        let sender = self.notice_sender();
        let run = sync.run_proton(step);
        runtime.spawn(async move {
            let result = run.await.and_then(|step| step);
            let _ = sender.send(Notice::Setup(Box::new(SetupEvent::Proton {
                generation,
                result,
            })));
        });
    }

    #[cfg(feature = "proton")]
    fn proton_answer(&mut self, result: anyhow::Result<zmail_proton::SignInStep>) {
        use zmail_setup::proton::ProtonAction;
        let Some(add) = self.add.as_mut() else {
            return;
        };
        let Some(action) = add.proton.finish(result) else {
            return;
        };
        match action {
            ProtonAction::Verify { url } => {
                add.go(Step::ProtonVerification);
                self.effects.push(Effect::Open(url));
            }
            ProtonAction::AskTotp => {
                wipe(&mut add.proton_input);
                add.go(Step::ProtonTotp);
            }
            ProtonAction::AskMailboxPassword => {
                wipe(&mut add.proton_input);
                add.go(Step::ProtonMailboxPassword);
            }
            ProtonAction::SignedIn(account) => self.proton_store(account),
        }
    }

    #[cfg(feature = "proton")]
    fn proton_store(&mut self, account: zmail_proton::SignedInAccount) {
        let (Some(secrets), Some(add)) = (self.secrets.clone(), self.add.as_mut()) else {
            return;
        };
        let Some(sign_in) = add.proton.sign_in().cloned() else {
            return;
        };
        let stored = zmail_setup::proton::store_signed_in(
            &self.store,
            &secrets,
            &sign_in,
            &add.form.email,
            &account,
        );
        let (new_account, account_id, session) = match stored {
            Ok(stored) => stored,
            Err(error) => {
                add.proton.error = Some(format!("{error:#}"));
                return;
            }
        };
        add.proton.busy = true;
        let generation = add.generation;
        let (Some(sync), Some(runtime)) = (self.sync.clone(), self.runtime.clone()) else {
            return;
        };
        let sender = self.notice_sender();
        let run = sync.run_proton(session);
        let name = new_account.display_name;
        runtime.spawn(async move {
            let result = run.await.and_then(|stored| stored);
            let _ = sender.send(Notice::Setup(Box::new(SetupEvent::ProtonStored {
                generation,
                account_id,
                name,
                result,
            })));
        });
    }

    #[cfg(feature = "proton")]
    fn proton_stored(&mut self, account_id: i64, name: String, result: anyhow::Result<()>) {
        let Some(add) = self.add.as_mut() else {
            return;
        };
        add.proton.busy = false;
        if let Err(error) = result {
            add.proton.error = Some(format!("{error:#}"));
            return;
        }
        add.proton.done();
        if let Some(sync) = &self.sync {
            let _ = sync.sync_account(account_id);
            let _ = sync.sync_watchers();
        }
        self.account_saved(account_id, format!("{name} added."));
    }
}

/// Space, Left or Right on a choice row.
fn change_choice(add: &mut AddAccount, row: Row, by: isize) {
    let cycle = |choices: &[Security], current: Security| {
        let position = choices
            .iter()
            .position(|choice| *choice == current)
            .unwrap_or(0);
        choices[(position as isize + by).rem_euclid(choices.len() as isize) as usize]
    };
    match row {
        Row::Protocol => {
            add.form.protocol = match add.form.protocol {
                ProtocolKind::Jmap => ProtocolKind::Imap,
                _ => ProtocolKind::Jmap,
            };
        }
        Row::ImapSecurity => {
            let next = cycle(&IMAP_SECURITY, add.form.imap_security);
            add.form.set_imap_security(next);
            add.errors.remove(&AccountField::ImapSecurity);
        }
        Row::SmtpSecurity => {
            let next = cycle(&SMTP_SECURITY, add.form.smtp_security);
            add.form.set_smtp_security(next);
            add.errors.remove(&AccountField::SmtpSecurity);
        }
        Row::Cleartext => {
            add.form.allow_cleartext = !add.form.allow_cleartext;
            add.errors.remove(&AccountField::AllowCleartext);
        }
        Row::Field(_) => {}
    }
    // The cleartext row comes and goes with the choices.
    add.focus = add.focus.min(add.rows().len().saturating_sub(1));
}
