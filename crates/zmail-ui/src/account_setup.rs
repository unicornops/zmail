//! Account setup form state, the account setup render function, and the
//! protocol-selector action buttons.
//!
//! [`AccountSetupState`] holds the in-memory form backing the new-account
//! wizard. [`render_account_setup_detail`] produces the grouped form content,
//! and [`account_protocol_buttons`] produces the toggle buttons used for
//! selecting IMAP+SMTP vs JMAP.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, Subscription, div, prelude::*,
    rgb,
};
use zmail_auth::OAuth2Preset;
use zmail_core::{AuthKind, ProtocolKind};

use super::WorkspaceView;
use crate::components::TextInput;
use crate::styles::{ICON_SIZE_SM, UiColors};
use crate::widgets::{EllipsisExt, icon, input_card, section_card, toolbar_button};

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
    pub(crate) notice: Option<&'static str>,
    /// The workspace's subscriptions to the inputs' events.
    pub(crate) subscriptions: Vec<Subscription>,
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
            subscriptions: Vec::new(),
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
// render_account_setup_detail
// ---------------------------------------------------------------------------

/// Render the account setup form: connection fields, identity fields, and
/// (for IMAP) SMTP submission fields.
///
/// Returns a [`gpui::Div`] ready to be placed inside the detail pane's
/// scrollable content area.
pub(crate) fn render_account_setup_detail(
    form: &AccountSetupState,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .w_full()
        .min_w_0()
        .when_some(form.notice, |this, notice| {
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
                    .child(div().flex_1().min_w_0().ellipsis().child(notice)),
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
pub(crate) fn account_protocol_buttons(
    protocol: ProtocolKind,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
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
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.account_setup.set_protocol(ProtocolKind::Imap, cx);
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
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.account_setup.set_protocol(ProtocolKind::Jmap, cx);
                // Provider presets carry IMAP endpoints and SMTP submission
                // defaults; they do not apply to JMAP accounts.
                this.account_setup.oauth_preset = None;
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
pub(crate) fn account_auth_buttons(
    form: &AccountSetupState,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
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
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.account_setup.auth = AuthKind::Password;
                this.account_setup.oauth_preset = None;
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
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.account_setup.auth = AuthKind::OAuth2;
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
pub(crate) fn account_oauth_provider_buttons(
    form: &AccountSetupState,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
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
                cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                    this.account_setup.oauth_preset = Some(preset);
                    let form = &this.account_setup;
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
