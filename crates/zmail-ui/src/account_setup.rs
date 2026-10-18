//! The account setup pane: the new-account form.
//!
//! [`AccountSetupView`] is its own entity with its own focus handle and the
//! `AccountSetup` key context. It validates the form, stores the account
//! and its credentials, starts the first sync (or the OAuth2 browser flow),
//! and reports the result to the shell as an [`AccountSetupEvent`]. It is
//! the piece a future Settings window will host.
//!
//! [`AccountSetupState`] holds the form's inputs and choices,
//! [`AccountFormValues`] a snapshot of them that validation and
//! [`build_account_from_form`] work on.

use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context as _, Result, bail};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, SharedString, Subscription, Window, div, prelude::*, rgb,
};
use url::Url;
use zmail_auth::OAuth2Preset;
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutgoingConfig, ProtocolKind,
    SubmissionSecurity, SyncPhase,
};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use crate::actions::SaveAccount;
use crate::components::{TextInput, TextInputEvent};
use crate::pane::pane_focus_indicator;
use crate::styles::{ICON_SIZE_SM, UiColors};
use crate::widgets::{EllipsisExt, icon, input_card, section_card, toolbar_button};
use crate::workspace::split_addresses;

/// The key context the account form sets.
pub(crate) const KEY_CONTEXT: &str = "AccountSetup";

// ---------------------------------------------------------------------------
// AccountField
// ---------------------------------------------------------------------------

/// Identifies one text field of the account setup form, for validation
/// errors and focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum AccountField {
    DisplayName,
    Endpoint,
    Username,
    Password,
    OauthClientId,
    OauthClientSecret,
    OauthAuthorizationEndpoint,
    OauthTokenEndpoint,
    OauthScopes,
    DiskBudget,
    IdentityName,
    IdentityEmail,
    Signature,
    AliasEmails,
    SmtpHost,
    SmtpPort,
    SmtpSecurity,
    SmtpUsername,
}

impl AccountField {
    /// Every field, in form order.
    pub(crate) const ALL: [AccountField; 18] = [
        AccountField::DisplayName,
        AccountField::Endpoint,
        AccountField::Username,
        AccountField::Password,
        AccountField::OauthClientId,
        AccountField::OauthClientSecret,
        AccountField::OauthAuthorizationEndpoint,
        AccountField::OauthTokenEndpoint,
        AccountField::OauthScopes,
        AccountField::DiskBudget,
        AccountField::IdentityName,
        AccountField::IdentityEmail,
        AccountField::Signature,
        AccountField::AliasEmails,
        AccountField::SmtpHost,
        AccountField::SmtpPort,
        AccountField::SmtpSecurity,
        AccountField::SmtpUsername,
    ];

    /// The visible label, also the field's accessible name.
    pub(crate) fn label(self) -> &'static str {
        match self {
            AccountField::DisplayName => "Display Name",
            AccountField::Endpoint => "Endpoint",
            AccountField::Username => "Username",
            AccountField::Password => "Password",
            AccountField::OauthClientId => "OAuth2 Client Id",
            AccountField::OauthClientSecret => "OAuth2 Client Secret",
            AccountField::OauthAuthorizationEndpoint => "Authorization Endpoint",
            AccountField::OauthTokenEndpoint => "Token Endpoint",
            AccountField::OauthScopes => "Scopes",
            AccountField::DiskBudget => "Disk Budget (MiB)",
            AccountField::IdentityName => "Primary Identity Name",
            AccountField::IdentityEmail => "Primary Identity Email",
            AccountField::Signature => "Signature",
            AccountField::AliasEmails => "Additional Identity Emails",
            AccountField::SmtpHost => "SMTP Host",
            AccountField::SmtpPort => "SMTP Port",
            AccountField::SmtpSecurity => "SMTP Security",
            AccountField::SmtpUsername => "SMTP Username",
        }
    }

    /// Placeholder text. The endpoint's depends on the protocol; see
    /// [`endpoint_placeholder`].
    fn placeholder(self) -> &'static str {
        match self {
            AccountField::DisplayName => "Work Mail",
            AccountField::Endpoint => endpoint_placeholder(ProtocolKind::Imap),
            AccountField::Username => "name@example.com",
            AccountField::Password => "App password or account secret",
            AccountField::OauthClientId => "Provider application client id",
            AccountField::OauthClientSecret => "Optional for public clients",
            AccountField::OauthAuthorizationEndpoint => "https://provider.example.test/authorize",
            AccountField::OauthTokenEndpoint => "https://provider.example.test/token",
            AccountField::OauthScopes => "Space-separated, for example: email",
            AccountField::DiskBudget => "Empty for unlimited",
            AccountField::IdentityName => "Jane Doe",
            AccountField::IdentityEmail => "jane@example.com",
            AccountField::Signature => "Regards,\nJane",
            AccountField::AliasEmails => "alias@example.com, billing@example.com",
            AccountField::SmtpHost => "smtp.example.com",
            AccountField::SmtpPort => "587",
            AccountField::SmtpSecurity => "plain, starttls, or implicit_tls",
            AccountField::SmtpUsername => "Optional override for SMTP auth",
        }
    }

    /// Whether Enter in this field submits the form (the last field of
    /// each section).
    pub(crate) fn submits_form(self) -> bool {
        matches!(
            self,
            AccountField::Password
                | AccountField::OauthClientSecret
                | AccountField::AliasEmails
                | AccountField::SmtpUsername
        )
    }
}

// ---------------------------------------------------------------------------
// AccountFormValues
// ---------------------------------------------------------------------------

/// A snapshot of the account setup form's values, which validation and
/// account building work on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountFormValues {
    pub(crate) protocol: ProtocolKind,
    pub(crate) auth: AuthKind,
    pub(crate) oauth_preset: Option<OAuth2Preset>,
    pub(crate) display_name: String,
    pub(crate) endpoint: String,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) oauth_client_id: String,
    pub(crate) oauth_client_secret: String,
    pub(crate) oauth_authorization_endpoint: String,
    pub(crate) oauth_token_endpoint: String,
    pub(crate) oauth_scopes: String,
    pub(crate) disk_budget_mb: String,
    pub(crate) identity_name: String,
    pub(crate) identity_email: String,
    pub(crate) signature: String,
    pub(crate) alias_emails: String,
    pub(crate) smtp_host: String,
    pub(crate) smtp_port: String,
    pub(crate) smtp_security: String,
    pub(crate) smtp_username: String,
}

impl Default for AccountFormValues {
    /// The values a new form starts with.
    fn default() -> Self {
        Self {
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            oauth_preset: None,
            display_name: String::new(),
            // Empty, so the protocol-specific placeholder shows instead of
            // an IMAP URL that JMAP accounts would have to delete.
            endpoint: String::new(),
            username: String::new(),
            password: String::new(),
            oauth_client_id: String::new(),
            oauth_client_secret: String::new(),
            // Custom provider endpoints, used by JMAP accounts: no consumer
            // provider presets offer JMAP.
            oauth_authorization_endpoint: String::new(),
            oauth_token_endpoint: String::new(),
            oauth_scopes: "email".to_owned(),
            // Empty means unlimited; a number is the budget in MiB that
            // the post-sync eviction pass enforces.
            disk_budget_mb: String::new(),
            identity_name: String::new(),
            identity_email: String::new(),
            signature: "Regards,".to_owned(),
            alias_emails: String::new(),
            smtp_host: String::new(),
            smtp_port: "587".to_owned(),
            smtp_security: "starttls".to_owned(),
            smtp_username: String::new(),
        }
    }
}

impl AccountFormValues {
    /// The value of one field.
    fn value(&self, field: AccountField) -> &str {
        match field {
            AccountField::DisplayName => &self.display_name,
            AccountField::Endpoint => &self.endpoint,
            AccountField::Username => &self.username,
            AccountField::Password => &self.password,
            AccountField::OauthClientId => &self.oauth_client_id,
            AccountField::OauthClientSecret => &self.oauth_client_secret,
            AccountField::OauthAuthorizationEndpoint => &self.oauth_authorization_endpoint,
            AccountField::OauthTokenEndpoint => &self.oauth_token_endpoint,
            AccountField::OauthScopes => &self.oauth_scopes,
            AccountField::DiskBudget => &self.disk_budget_mb,
            AccountField::IdentityName => &self.identity_name,
            AccountField::IdentityEmail => &self.identity_email,
            AccountField::Signature => &self.signature,
            AccountField::AliasEmails => &self.alias_emails,
            AccountField::SmtpHost => &self.smtp_host,
            AccountField::SmtpPort => &self.smtp_port,
            AccountField::SmtpSecurity => &self.smtp_security,
            AccountField::SmtpUsername => &self.smtp_username,
        }
    }
}

// ---------------------------------------------------------------------------
// AccountSetupState
// ---------------------------------------------------------------------------

/// In-memory form state for the new-account setup wizard.
///
/// Each text field is its own [`TextInput`] entity, which owns its text,
/// cursor and undo history. The `protocol` field drives which additional
/// SMTP fields are shown, and `auth` switches between password entry and
/// the OAuth2 provider flow.
pub(crate) struct AccountSetupState {
    pub(crate) protocol: ProtocolKind,
    pub(crate) auth: AuthKind,
    /// Chosen OAuth2 provider preset; `None` until a provider button is
    /// picked, which also fills the endpoint and SMTP defaults.
    pub(crate) oauth_preset: Option<OAuth2Preset>,
    inputs: BTreeMap<AccountField, Entity<TextInput>>,
    /// Validation errors shown under their fields; editing a field clears
    /// its error.
    pub(crate) errors: BTreeMap<AccountField, String>,
    /// Why the form opened when the user did not ask for it, for example
    /// Compose with no account.
    pub(crate) notice: Option<SharedString>,
}

/// Endpoint placeholder for the selected protocol.
pub(crate) fn endpoint_placeholder(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::Imap => "imaps://mail.example.com:993",
        ProtocolKind::Jmap => "https://mail.example.com/.well-known/jmap",
    }
}

/// The IMAP endpoint a provider preset uses.
pub(crate) fn preset_endpoint(preset: OAuth2Preset) -> &'static str {
    match preset {
        OAuth2Preset::Gmail => "imaps://imap.gmail.com:993",
        OAuth2Preset::Microsoft365 => "imaps://outlook.office365.com:993",
    }
}

/// The SMTP submission defaults a provider preset fills in.
pub(crate) fn preset_smtp(preset: OAuth2Preset) -> (&'static str, &'static str, &'static str) {
    match preset {
        OAuth2Preset::Gmail => ("smtp.gmail.com", "465", "implicit_tls"),
        OAuth2Preset::Microsoft365 => ("smtp.office365.com", "587", "starttls"),
    }
}

impl AccountSetupState {
    /// A new, empty form with its default values.
    pub(crate) fn new(cx: &mut App) -> Self {
        let defaults = AccountFormValues::default();
        let inputs = AccountField::ALL
            .into_iter()
            .map(|field| {
                let value = defaults.value(field).to_owned();
                let input = cx.new(|cx| {
                    let input = TextInput::new(cx)
                        .label(field.label())
                        .placeholder(field.placeholder());
                    let input = match field {
                        AccountField::Password | AccountField::OauthClientSecret => input.masked(),
                        AccountField::Signature => input.multiline().rows(3, 8),
                        _ => input,
                    };
                    input.with_text(value)
                });
                (field, input)
            })
            .collect();
        Self {
            protocol: defaults.protocol,
            auth: defaults.auth,
            oauth_preset: defaults.oauth_preset,
            inputs,
            errors: BTreeMap::new(),
            notice: None,
        }
    }

    /// The input entity for `field`.
    pub(crate) fn input(&self, field: AccountField) -> &Entity<TextInput> {
        &self.inputs[&field]
    }

    /// Replace a field's text, as the provider presets do.
    pub(crate) fn set_text(&self, field: AccountField, text: &str, cx: &mut App) {
        self.input(field)
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    /// Switch the protocol, updating the protocol-specific placeholder.
    pub(crate) fn set_protocol(&mut self, protocol: ProtocolKind, cx: &mut App) {
        self.protocol = protocol;
        self.input(AccountField::Endpoint).update(cx, |input, cx| {
            input.set_placeholder(endpoint_placeholder(protocol), cx);
        });
    }

    /// Snapshot the form's current values.
    pub(crate) fn values(&self, cx: &App) -> AccountFormValues {
        let text = |field: AccountField| self.input(field).read(cx).text().to_owned();
        AccountFormValues {
            protocol: self.protocol,
            auth: self.auth,
            oauth_preset: self.oauth_preset,
            display_name: text(AccountField::DisplayName),
            endpoint: text(AccountField::Endpoint),
            username: text(AccountField::Username),
            password: text(AccountField::Password),
            oauth_client_id: text(AccountField::OauthClientId),
            oauth_client_secret: text(AccountField::OauthClientSecret),
            oauth_authorization_endpoint: text(AccountField::OauthAuthorizationEndpoint),
            oauth_token_endpoint: text(AccountField::OauthTokenEndpoint),
            oauth_scopes: text(AccountField::OauthScopes),
            disk_budget_mb: text(AccountField::DiskBudget),
            identity_name: text(AccountField::IdentityName),
            identity_email: text(AccountField::IdentityEmail),
            signature: text(AccountField::Signature),
            alias_emails: text(AccountField::AliasEmails),
            smtp_host: text(AccountField::SmtpHost),
            smtp_port: text(AccountField::SmtpPort),
            smtp_security: text(AccountField::SmtpSecurity),
            smtp_username: text(AccountField::SmtpUsername),
        }
    }
}

// ---------------------------------------------------------------------------
// AccountSetupView
// ---------------------------------------------------------------------------

/// What happened in the account form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccountSetupEvent {
    /// The account was stored; `message` tells the user what happens next.
    Added { account_id: i64, message: String },
    /// Storing the account failed.
    Error(String),
}

pub(crate) struct AccountSetupView {
    focus_handle: FocusHandle,
    store: Arc<LocalStore>,
    secrets: Arc<dyn SecretStore>,
    sync_service: SyncService,
    pub(crate) form: AccountSetupState,
    /// Subscriptions to the form inputs' events.
    _input_subscriptions: Vec<Subscription>,
}

impl EventEmitter<AccountSetupEvent> for AccountSetupView {}

impl Focusable for AccountSetupView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AccountSetupView {
    pub(crate) fn new(
        store: Arc<LocalStore>,
        secrets: Arc<dyn SecretStore>,
        sync_service: SyncService,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let form = AccountSetupState::new(cx);
        let subscriptions = Self::watch_inputs(&form, window, cx);
        Self {
            focus_handle: cx.focus_handle(),
            store,
            secrets,
            sync_service,
            form,
            _input_subscriptions: subscriptions,
        }
    }

    /// Shows the form, optionally explaining why it opened when the user
    /// did not ask for it, and focuses the first field.
    pub(crate) fn open(
        &mut self,
        notice: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.form.notice = notice;
        self.focus_first_field(window, cx);
        cx.notify();
    }

    /// Focuses the form's first field.
    pub(crate) fn focus_first_field(&self, window: &mut Window, cx: &mut App) {
        focus_input(self.form.input(AccountField::DisplayName), window, cx);
    }

    /// Subscribes to the inputs: editing a field clears its error, and
    /// Enter in the last field of a section submits the form.
    fn watch_inputs(
        form: &AccountSetupState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        AccountField::ALL
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
                        TextInputEvent::Submitted => {
                            if field.submits_form() {
                                this.submit(window, cx);
                            }
                        }
                    },
                )
            })
            .collect()
    }

    fn save_account(&mut self, _: &SaveAccount, window: &mut Window, cx: &mut Context<Self>) {
        self.submit(window, cx);
    }

    /// Validates and submits the form: stores the account and its secret,
    /// then starts the first sync or the OAuth2 browser flow.
    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = self.form.values(cx);
        let errors = validate_account_form(&form);
        self.form.errors = errors.into_iter().collect();
        if let Some(first) = self.form.errors.keys().next().copied() {
            // Errors show under their fields; focus the first one.
            focus_input(self.form.input(first), window, cx);
            cx.notify();
            return;
        }
        match self.store_account(&form) {
            Ok((account_id, message)) => {
                self.form = AccountSetupState::new(cx);
                self._input_subscriptions = Self::watch_inputs(&self.form, window, cx);
                cx.emit(AccountSetupEvent::Added {
                    account_id,
                    message,
                });
            }
            Err(error) => cx.emit(AccountSetupEvent::Error(error.to_string())),
        }
        cx.notify();
    }

    /// Stores a validated form and returns the new account id and the
    /// status message for the user.
    fn store_account(&self, form: &AccountFormValues) -> Result<(i64, String)> {
        let (account, secret) = build_account_from_form(form)?;
        let account_id = self.store.insert_account(&account)?;
        // The optional disk budget applies after the account exists.
        if let Some(budget_mb) = parse_disk_budget_mb(&form.disk_budget_mb)? {
            self.store
                .set_account_disk_budget(account_id, Some(budget_mb))?;
        }
        let message = match secret {
            AccountSetupSecret::Password(secret) => {
                self.secrets.set_account_secret(account_id, &secret)?;
                let _ = self.sync_service.sync_account(account_id);
                format!("{} added.", account.display_name)
            }
            AccountSetupSecret::OAuth2(client) => {
                // The browser flow blocks for up to its redirect timeout, so
                // it runs off the GUI thread; the account's sync status marks
                // the wait, and the grant (tokens, client config, expiry) is
                // persisted before the first sync starts. Syncing here would
                // only record a missing-credentials failure.
                let _ = self.store.begin_sync_attempt(
                    account_id,
                    SyncPhase::Connecting,
                    "Waiting for browser authorization",
                );
                self.spawn_oauth2_authorization(account_id, client);
                format!(
                    "{} added. Finish signing in in your browser; the account syncs once you approve access.",
                    account.display_name
                )
            }
        };
        // Start the new account's background watcher immediately instead of
        // waiting for the next app launch.
        let _ = self.sync_service.sync_watchers();
        Ok((account_id, message))
    }

    /// Runs the OAuth2 browser flow on a background thread and stores the
    /// resulting grant, so the GUI never blocks on the redirect wait.
    fn spawn_oauth2_authorization(&self, account_id: i64, client: zmail_auth::OAuth2Client) {
        let store = self.store.clone();
        let secrets = self.secrets.clone();
        let sync_service = self.sync_service.clone();
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
            let _ = self.store.record_sync_failure(
                account_id,
                "Browser authorization failed",
                error.to_string(),
            );
        }
    }
}

impl Render for AccountSetupView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.global::<UiColors>();
        let focused = self.focus_handle.contains_focused(window, cx);
        let focus_handle = self.focus_handle.clone();

        div()
            .id("account-setup")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::save_account))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            .child(
                pane_focus_indicator(
                    div()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(rgb(colors.border)),
                    focused,
                    &colors,
                )
                .child("Account Setup"),
            )
            .child(
                div()
                    .id("account-setup-scroll")
                    .flex_1()
                    // A flex column stretches its content to the pane width,
                    // so form fields size to the pane rather than to their
                    // own content.
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .px_4()
                    .py_4()
                    .child(render_account_setup_detail(&self.form, &colors, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .child(toolbar_button(
                        "Save Account",
                        Some("check-circle"),
                        colors.button_background,
                        colors.border,
                        colors.foreground,
                        move |_, window, cx| focus_handle.dispatch_action(&SaveAccount, window, cx),
                    )),
            )
    }
}

// ---------------------------------------------------------------------------
// render_account_setup_detail
// ---------------------------------------------------------------------------

/// Render the account setup form: connection fields, identity fields, and
/// (for IMAP) SMTP submission fields.
///
/// Returns a [`gpui::Div`] ready to be placed inside the detail pane's
/// scrollable content area.
fn render_account_setup_detail(
    form: &AccountSetupState,
    colors: &UiColors,
    cx: &mut Context<AccountSetupView>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .w_full()
        .min_w_0()
        .when_some(form.notice.clone(), |this, notice| {
            this.child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .rounded_md()
                    .bg(rgb(colors.surface))
                    .border_1()
                    .border_color(rgb(colors.info))
                    .child(icon("user-plus", ICON_SIZE_SM, colors.info))
                    .child(div().flex_1().min_w_0().child(notice)),
            )
        })
        .child(
            section_card("Connection", colors)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(account_protocol_buttons(form.protocol, colors, cx)),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(account_auth_buttons(form, colors, cx)),
                )
                .children(
                    if form.auth == AuthKind::OAuth2 && form.protocol == ProtocolKind::Imap {
                        vec![
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(account_oauth_provider_buttons(form, colors, cx)),
                    oauth_client_fields(form, colors),
                    div()
                        .w_full()
                        .rounded_md()
                        .bg(rgb(colors.background))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .px_3()
                        .py_2()
                        .child(
                            // One line on purpose: multi-line wrapped text in
                            // this form makes GPUI 0.2.2 size the card from a
                            // stale text measurement (the card grows by
                            // hundreds of pixels). Revisit after the GPUI
                            // upgrade.
                            div()
                                .w_full()
                                .ellipsis()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .child(
                                    "Saving opens your provider's sign-in page in the browser.",
                                ),
                        ),
                ]
                    } else if form.auth == AuthKind::OAuth2 && form.protocol == ProtocolKind::Jmap {
                        vec![
                    oauth_client_fields(form, colors),
                    div()
                        .flex()
                        .gap_3()
                        .child(
                            div().flex_1().min_w_0().child(
                                account_input(form, AccountField::OauthAuthorizationEndpoint, colors),
                            ),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                account_input(form, AccountField::OauthTokenEndpoint, colors),
                            ),
                        ),
                    div().child(
                        account_input(form, AccountField::OauthScopes, colors),
                    ),
                    div()
                        .w_full()
                        .rounded_md()
                        .bg(rgb(colors.background))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .px_3()
                        .py_2()
                        .child(
                            // One line on purpose: multi-line wrapped text in
                            // this form makes GPUI 0.2.2 size the card from a
                            // stale text measurement (the card grows by
                            // hundreds of pixels). Revisit after the GPUI
                            // upgrade.
                            div()
                                .w_full()
                                .ellipsis()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .child(
                                    "Saving opens your provider's sign-in page in the browser.",
                                ),
                        ),
                ]
                    } else {
                        Vec::new()
                    },
                )
                .child(account_input(form, AccountField::DisplayName, colors))
                .child(account_input(form, AccountField::Endpoint, colors))
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(account_input(form, AccountField::Username, colors)))
                        .children(if form.auth == AuthKind::Password {
                            vec![div().flex_1().min_w_0().child(account_input(form, AccountField::Password, colors))]
                        } else {
                            Vec::new()
                        }),
                ),
        )
        .child(account_input(form, AccountField::DiskBudget, colors))
        .child(
            section_card("Identity", colors)
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(account_input(form, AccountField::IdentityName, colors)))
                        .child(div().flex_1().min_w_0().child(account_input(form, AccountField::IdentityEmail, colors))),
                )
                .child(account_input(form, AccountField::AliasEmails, colors))
                .child(account_input(form, AccountField::Signature, colors)),
        )
        // JMAP submits through the account's own endpoint, so the SMTP
        // section only appears for IMAP accounts.
        .when(form.protocol == ProtocolKind::Imap, |this| {
            this.child(
                section_card("Outgoing Mail (SMTP)", colors).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(account_input(form, AccountField::SmtpHost, colors))
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child(div().flex_1().min_w_0().child(account_input(form, AccountField::SmtpPort, colors)))
                                .child(div().flex_1().min_w_0().child(account_input(form, AccountField::SmtpSecurity, colors))),
                        )
                        .child(account_input(form, AccountField::SmtpUsername, colors)),
                ),
            )
        })
}

// ---------------------------------------------------------------------------
// account_protocol_buttons
// ---------------------------------------------------------------------------

/// Build the protocol toggle buttons ("IMAP + SMTP" / "JMAP") rendered inside
/// the connection section of the account-setup form.
fn account_protocol_buttons(
    protocol: ProtocolKind,
    colors: &UiColors,
    cx: &mut Context<AccountSetupView>,
) -> Vec<AnyElement> {
    vec![
        toolbar_button(
            "IMAP + SMTP",
            Some("server"),
            if protocol == ProtocolKind::Imap {
                colors.selected_background
            } else {
                colors.button_background
            },
            if protocol == ProtocolKind::Imap {
                colors.accent
            } else {
                colors.border
            },
            colors.foreground,
            cx.listener(|this: &mut AccountSetupView, _, _, cx| {
                this.form.set_protocol(ProtocolKind::Imap, cx);
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            "JMAP",
            Some("server"),
            if protocol == ProtocolKind::Jmap {
                colors.selected_background
            } else {
                colors.button_background
            },
            if protocol == ProtocolKind::Jmap {
                colors.accent
            } else {
                colors.border
            },
            colors.foreground,
            cx.listener(|this: &mut AccountSetupView, _, _, cx| {
                this.form.set_protocol(ProtocolKind::Jmap, cx);
                // Provider presets carry IMAP endpoints and SMTP submission
                // defaults; they do not apply to JMAP accounts.
                this.form.oauth_preset = None;
                cx.notify();
            }),
        )
        .into_any_element(),
    ]
}

// ---------------------------------------------------------------------------
// account_auth_buttons / account_oauth_provider_buttons
// ---------------------------------------------------------------------------

/// Renders the shared OAuth2 client id and optional client secret inputs.
fn oauth_client_fields(form: &AccountSetupState, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .gap_3()
        .child(div().flex_1().min_w_0().child(account_input(
            form,
            AccountField::OauthClientId,
            colors,
        )))
        .child(div().flex_1().min_w_0().child(account_input(
            form,
            AccountField::OauthClientSecret,
            colors,
        )))
}

/// Build the auth-kind toggle buttons ("Password" / "OAuth2") rendered
/// inside the connection section for IMAP accounts.
fn account_auth_buttons(
    form: &AccountSetupState,
    colors: &UiColors,
    cx: &mut Context<AccountSetupView>,
) -> Vec<AnyElement> {
    vec![
        toolbar_button(
            "Password",
            Some("key"),
            if form.auth == AuthKind::Password {
                colors.selected_background
            } else {
                colors.button_background
            },
            if form.auth == AuthKind::Password {
                colors.accent
            } else {
                colors.border
            },
            colors.foreground,
            cx.listener(|this: &mut AccountSetupView, _, _, cx| {
                this.form.auth = AuthKind::Password;
                this.form.oauth_preset = None;
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            "OAuth2",
            Some("key"),
            if form.auth == AuthKind::OAuth2 {
                colors.selected_background
            } else {
                colors.button_background
            },
            if form.auth == AuthKind::OAuth2 {
                colors.accent
            } else {
                colors.border
            },
            colors.foreground,
            cx.listener(|this: &mut AccountSetupView, _, _, cx| {
                this.form.auth = AuthKind::OAuth2;
                cx.notify();
            }),
        )
        .into_any_element(),
    ]
}

/// Build the OAuth2 provider toggle buttons ("Gmail" / "Microsoft 365").
///
/// Picking a provider fills the endpoint and SMTP submission defaults so
/// the wizard stays one-click for the two consumer presets.
fn account_oauth_provider_buttons(
    form: &AccountSetupState,
    colors: &UiColors,
    cx: &mut Context<AccountSetupView>,
) -> Vec<AnyElement> {
    [OAuth2Preset::Gmail, OAuth2Preset::Microsoft365]
        .into_iter()
        .map(|preset| {
            toolbar_button(
                preset.name(),
                Some("server"),
                if form.oauth_preset == Some(preset) {
                    colors.selected_background
                } else {
                    colors.button_background
                },
                if form.oauth_preset == Some(preset) {
                    colors.accent
                } else {
                    colors.border
                },
                colors.foreground,
                cx.listener(move |this: &mut AccountSetupView, _, _, cx| {
                    this.form.oauth_preset = Some(preset);
                    let form = &this.form;
                    form.set_text(AccountField::Endpoint, preset_endpoint(preset), cx);
                    let (host, port, security) = preset_smtp(preset);
                    form.set_text(AccountField::SmtpHost, host, cx);
                    form.set_text(AccountField::SmtpPort, port, cx);
                    form.set_text(AccountField::SmtpSecurity, security, cx);
                    cx.notify();
                }),
            )
            .into_any_element()
        })
        .collect()
}

/// An account-setup input with its label and, when the last submit found
/// a problem with it, the error underneath.
fn account_input(form: &AccountSetupState, field: AccountField, colors: &UiColors) -> gpui::Div {
    let danger = colors.danger;
    input_card(field.label(), form.input(field), colors).when_some(
        form.errors.get(&field).cloned(),
        move |this, error| {
            this.child(
                div()
                    .w_full()
                    .text_sm()
                    .text_color(rgb(danger))
                    .ellipsis()
                    .child(error),
            )
        },
    )
}

// ---------------------------------------------------------------------------
// Building and validating the account
// ---------------------------------------------------------------------------

/// Moves keyboard focus to a text input.
fn focus_input(input: &Entity<TextInput>, window: &mut Window, cx: &mut App) {
    input.update(cx, |input, cx| input.focus(window, cx));
}

/// Build a [`NewAccount`] plus the form's credential plan.
pub(crate) fn build_account_from_form(
    form: &AccountFormValues,
) -> Result<(NewAccount, AccountSetupSecret)> {
    let display_name = required_text("Display name", &form.display_name)?;
    let endpoint = required_text("Endpoint", &form.endpoint)?;
    validate_endpoint(form.protocol, &endpoint)?;
    let username = required_text("Username", &form.username)?;

    // JMAP accounts authenticate with a password; bearer-token auth for
    // JMAP is not wired yet.
    // JMAP accounts can authenticate with a password or an OAuth2
    // bearer token; no consumer presets offer JMAP, so OAuth2 uses the
    // provider's own endpoints entered in the form.
    let (auth, secret) = if form.protocol == ProtocolKind::Jmap {
        match form.auth {
            AuthKind::Password => (
                AuthKind::Password,
                AccountSetupSecret::Password(required_text("Password", &form.password)?),
            ),
            AuthKind::OAuth2 => {
                let client = custom_oauth2_client_from_form(form)?;
                (AuthKind::OAuth2, AccountSetupSecret::OAuth2(client))
            }
        }
    } else {
        match form.auth {
            AuthKind::Password => (
                AuthKind::Password,
                AccountSetupSecret::Password(required_text("Password", &form.password)?),
            ),
            AuthKind::OAuth2 => {
                let preset = form
                    .oauth_preset
                    .context("Choose an OAuth2 provider (Gmail or Microsoft 365)")?;
                let client_id = required_text("OAuth2 client id", &form.oauth_client_id)?;
                let client_secret = trimmed_or_none(&form.oauth_client_secret);
                let client = preset.client(client_id, client_secret, OAUTH2_REDIRECT_PORT);
                (AuthKind::OAuth2, AccountSetupSecret::OAuth2(client))
            }
        }
    };
    let identity_name = if form.identity_name.trim().is_empty() {
        display_name.clone()
    } else {
        form.identity_name.trim().to_owned()
    };
    let identity_email = if form.identity_email.trim().is_empty() {
        username.clone()
    } else {
        form.identity_email.trim().to_owned()
    };
    let mut identities = vec![NewIdentity {
        display_name: identity_name.clone(),
        email_address: identity_email,
        signature: form.signature.clone(),
        is_primary: true,
    }];
    for (index, alias) in split_addresses(&form.alias_emails).into_iter().enumerate() {
        identities.push(NewIdentity {
            display_name: format!("{identity_name} Alias {}", index + 1),
            email_address: alias,
            signature: String::new(),
            is_primary: false,
        });
    }

    let outgoing = match form.protocol {
        ProtocolKind::Imap => {
            let mut outgoing = OutgoingConfig::smtp_default(derive_host(&endpoint));
            outgoing.host = Some(required_text("SMTP host", &form.smtp_host)?);
            outgoing.port = Some(
                required_text("SMTP port", &form.smtp_port)?
                    .parse::<u16>()
                    .context("SMTP port must be an integer")?,
            );
            outgoing.security = Some(parse_submission_security(&form.smtp_security)?);
            outgoing.username = Some(if form.smtp_username.trim().is_empty() {
                username.clone()
            } else {
                form.smtp_username.trim().to_owned()
            });
            outgoing
        }
        ProtocolKind::Jmap => OutgoingConfig::jmap_default(),
    };

    Ok((
        NewAccount {
            display_name,
            protocol: form.protocol,
            auth,
            endpoint,
            username,
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(outgoing),
            identities,
        },
        secret,
    ))
}

/// Loopback port the UI OAuth2 flow listens on for the provider redirect,
/// matching the CLI default.
const OAUTH2_REDIRECT_PORT: u16 = 8357;

/// Credentials the account setup form collected for a new account.
pub(crate) enum AccountSetupSecret {
    /// Password or app password, stored directly.
    Password(String),
    /// The OAuth2 client whose browser flow runs after the account is stored.
    OAuth2(zmail_auth::OAuth2Client),
}

/// Parses the account setup form's disk budget: empty means unlimited.
fn parse_disk_budget_mb(value: &str) -> Result<Option<u64>> {
    match value.trim() {
        "" => Ok(None),
        trimmed => trimmed
            .parse::<u64>()
            .map(Some)
            .context("the disk budget must be a whole number of MiB"),
    }
}

/// Returns the trimmed text, or `None` when it is empty or whitespace-only.
fn trimmed_or_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Builds the provider-agnostic OAuth2 client for JMAP accounts from the
/// form's custom endpoint fields.
fn custom_oauth2_client_from_form(form: &AccountFormValues) -> Result<zmail_auth::OAuth2Client> {
    let client_id = required_text("OAuth2 client id", &form.oauth_client_id)?;
    let client_secret = trimmed_or_none(&form.oauth_client_secret);
    let authorization_endpoint = required_text(
        "OAuth2 authorization endpoint",
        &form.oauth_authorization_endpoint,
    )?;
    let token_endpoint = required_text("OAuth2 token endpoint", &form.oauth_token_endpoint)?;
    let scopes = form
        .oauth_scopes
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(zmail_auth::OAuth2Client {
        client_id,
        client_secret,
        authorization_endpoint,
        token_endpoint,
        scopes,
        redirect_port: OAUTH2_REDIRECT_PORT,
        extra_authorize_params: Vec::new(),
    })
}

/// Checks the account setup form and returns an error message for each
/// field that needs fixing, so the form can show them beside the fields.
/// It mirrors the checks in [`build_account_from_form`].
pub(crate) fn validate_account_form(form: &AccountFormValues) -> Vec<(AccountField, String)> {
    let mut errors = Vec::new();
    let mut require = |field: AccountField, value: &str, message: &str| {
        if value.trim().is_empty() {
            errors.push((field, message.to_owned()));
            false
        } else {
            true
        }
    };

    require(
        AccountField::DisplayName,
        &form.display_name,
        "Enter a name for this account.",
    );
    let has_endpoint = require(
        AccountField::Endpoint,
        &form.endpoint,
        match form.protocol {
            ProtocolKind::Imap => {
                "Enter the IMAP server, for example imaps://mail.example.com:993."
            }
            ProtocolKind::Jmap => "Enter the JMAP session URL.",
        },
    );
    require(
        AccountField::Username,
        &form.username,
        "Enter the username you sign in with.",
    );
    match form.auth {
        AuthKind::Password => {
            require(
                AccountField::Password,
                &form.password,
                "Enter the password or app password.",
            );
        }
        AuthKind::OAuth2 => {
            require(
                AccountField::OauthClientId,
                &form.oauth_client_id,
                "Enter the OAuth2 client id.",
            );
            if form.protocol == ProtocolKind::Jmap {
                require(
                    AccountField::OauthAuthorizationEndpoint,
                    &form.oauth_authorization_endpoint,
                    "Enter the authorization endpoint.",
                );
                require(
                    AccountField::OauthTokenEndpoint,
                    &form.oauth_token_endpoint,
                    "Enter the token endpoint.",
                );
            }
        }
    }
    if form.protocol == ProtocolKind::Imap {
        require(
            AccountField::SmtpHost,
            &form.smtp_host,
            "Enter the SMTP server.",
        );
    }

    if has_endpoint && let Err(error) = validate_endpoint(form.protocol, form.endpoint.trim()) {
        errors.push((AccountField::Endpoint, error.to_string()));
    }
    if form.auth == AuthKind::OAuth2
        && form.protocol == ProtocolKind::Imap
        && form.oauth_preset.is_none()
    {
        errors.push((
            AccountField::OauthClientId,
            "Choose Gmail or Microsoft 365 above.".to_owned(),
        ));
    }
    if parse_disk_budget_mb(&form.disk_budget_mb).is_err() {
        errors.push((
            AccountField::DiskBudget,
            "Enter a whole number of MiB, or leave it empty.".to_owned(),
        ));
    }
    if form.protocol == ProtocolKind::Imap {
        if form.smtp_port.trim().parse::<u16>().is_err() {
            errors.push((
                AccountField::SmtpPort,
                "Enter a port number, for example 587.".to_owned(),
            ));
        }
        if parse_submission_security(&form.smtp_security).is_err() {
            errors.push((
                AccountField::SmtpSecurity,
                "Use plain, starttls or implicit_tls.".to_owned(),
            ));
        }
    }
    errors
}

/// Return `Ok(trimmed)` or an error if `value` is empty or whitespace-only.
fn required_text(label: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(trimmed.to_owned())
}

/// Validate that `endpoint` is a well-formed URL with the correct scheme for
/// `protocol`.
fn validate_endpoint(protocol: ProtocolKind, endpoint: &str) -> Result<()> {
    let parsed =
        Url::parse(endpoint).with_context(|| format!("failed to parse endpoint {endpoint}"))?;
    match protocol {
        ProtocolKind::Imap => {
            if !matches!(parsed.scheme(), "imap" | "imaps") {
                bail!("IMAP endpoints must use imap:// or imaps://");
            }
        }
        ProtocolKind::Jmap => {
            if !matches!(parsed.scheme(), "http" | "https") {
                bail!("JMAP endpoints must use http:// or https://");
            }
        }
    }
    Ok(())
}

/// Parse a submission security mode string (case-insensitive).
fn parse_submission_security(value: &str) -> Result<SubmissionSecurity> {
    SubmissionSecurity::parse(&value.trim().to_ascii_lowercase())
        .with_context(|| format!("unknown submission security mode {value}"))
}

/// Extract the host component from an endpoint URL, defaulting to
/// `"localhost"` if parsing fails.
fn derive_host(endpoint: &str) -> String {
    Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "localhost".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        AccountField, AccountFormValues, custom_oauth2_client_from_form, parse_disk_budget_mb,
        validate_account_form,
    };

    #[test]
    fn validate_account_form_reports_errors_per_field() {
        let form = AccountFormValues::default();
        let errors = validate_account_form(&form);
        let fields = errors.iter().map(|(field, _)| *field).collect::<Vec<_>>();
        assert!(fields.contains(&AccountField::DisplayName));
        assert!(fields.contains(&AccountField::Endpoint));
        assert!(fields.contains(&AccountField::Username));
        assert!(fields.contains(&AccountField::Password));
        assert!(fields.contains(&AccountField::SmtpHost));

        let mut form = AccountFormValues {
            display_name: "Work".to_owned(),
            endpoint: "https://wrong.example.com".to_owned(),
            username: "me@example.com".to_owned(),
            password: "secret".to_owned(),
            smtp_host: "smtp.example.com".to_owned(),
            smtp_port: "not-a-port".to_owned(),
            ..AccountFormValues::default()
        };
        let errors = validate_account_form(&form);
        assert_eq!(
            errors.iter().map(|(field, _)| *field).collect::<Vec<_>>(),
            vec![AccountField::Endpoint, AccountField::SmtpPort],
            "only the wrong scheme and the bad port are reported: {errors:?}"
        );

        form.endpoint = "imaps://mail.example.com:993".to_owned();
        form.smtp_port = "587".to_owned();
        assert!(validate_account_form(&form).is_empty());

        // JMAP accounts need no SMTP settings.
        let form = AccountFormValues {
            protocol: zmail_core::ProtocolKind::Jmap,
            display_name: "Fastmail".to_owned(),
            endpoint: "https://api.example.com/.well-known/jmap".to_owned(),
            username: "me@example.com".to_owned(),
            password: "secret".to_owned(),
            smtp_port: String::new(),
            ..AccountFormValues::default()
        };
        assert!(validate_account_form(&form).is_empty());
    }

    #[test]
    fn disk_budget_parses_empty_as_unlimited() {
        assert_eq!(parse_disk_budget_mb("").expect("empty parses"), None);
        assert_eq!(parse_disk_budget_mb("  ").expect("blank parses"), None);
        assert_eq!(
            parse_disk_budget_mb(" 512 ").expect("a number parses"),
            Some(512)
        );
        assert!(parse_disk_budget_mb("512x").is_err(), "garbage is rejected");
        assert!(
            parse_disk_budget_mb("-1").is_err(),
            "negatives are rejected"
        );
    }

    #[test]
    fn custom_oauth2_client_from_form_validates_and_builds() {
        let mut form = AccountFormValues {
            oauth_client_id: "client-1".to_owned(),
            ..AccountFormValues::default()
        };

        // Missing provider endpoints are rejected with a clear label.
        let error = custom_oauth2_client_from_form(&form)
            .expect_err("the authorization endpoint should be required");
        assert!(
            format!("{error}").contains("OAuth2 authorization endpoint"),
            "the error should name the missing field: {error}"
        );

        form.oauth_authorization_endpoint = "https://provider.test/authorize".to_owned();
        let error = custom_oauth2_client_from_form(&form)
            .expect_err("the token endpoint should be required");
        assert!(
            format!("{error}").contains("OAuth2 token endpoint"),
            "the error should name the missing field: {error}"
        );

        form.oauth_token_endpoint = "https://provider.test/token".to_owned();
        form.oauth_scopes = "email profile".to_owned();
        let client = custom_oauth2_client_from_form(&form).expect("the custom client should build");
        assert_eq!(client.client_id, "client-1");
        assert_eq!(
            client.authorization_endpoint,
            "https://provider.test/authorize"
        );
        assert_eq!(client.token_endpoint, "https://provider.test/token");
        assert_eq!(
            client.scopes,
            vec!["email".to_owned(), "profile".to_owned()]
        );
        assert!(client.client_secret.is_none());
    }
}
