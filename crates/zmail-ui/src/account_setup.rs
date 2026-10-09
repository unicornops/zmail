//! Account setup form state, the account setup render function, and the
//! protocol-selector action buttons.
//!
//! [`AccountSetupState`] holds the in-memory form backing the new-account
//! wizard. [`render_account_setup_detail`] produces the grouped form content,
//! and [`account_protocol_buttons`] produces the toggle buttons used for
//! selecting IMAP+SMTP vs JMAP.

use gpui::{AnyElement, Context, IntoElement, StatefulInteractiveElement, div, prelude::*, rgb};
use zmail_auth::OAuth2Preset;
use zmail_core::{AuthKind, ProtocolKind};

use super::WorkspaceView;
use crate::input::{EditorField, UiBuffer};
use crate::styles::UiColors;
use crate::widgets::{input_card, section_card, toolbar_button};

// ---------------------------------------------------------------------------
// AccountSetupState
// ---------------------------------------------------------------------------

/// In-memory form state for the new-account setup wizard.
///
/// All text fields are [`UiBuffer`]s so the cursor position is preserved
/// across refreshes.  The `protocol` field drives which additional SMTP fields
/// are shown, and `auth` switches between password entry and the OAuth2
/// provider flow.
#[derive(Debug, Clone)]
pub(crate) struct AccountSetupState {
    pub(crate) protocol: ProtocolKind,
    pub(crate) auth: AuthKind,
    /// Chosen OAuth2 provider preset; `None` until a provider button is
    /// picked, which also fills the endpoint and SMTP defaults.
    pub(crate) oauth_preset: Option<OAuth2Preset>,
    pub(crate) display_name: UiBuffer,
    pub(crate) endpoint: UiBuffer,
    pub(crate) username: UiBuffer,
    pub(crate) password: UiBuffer,
    pub(crate) oauth_client_id: UiBuffer,
    pub(crate) oauth_client_secret: UiBuffer,
    pub(crate) oauth_authorization_endpoint: UiBuffer,
    pub(crate) oauth_token_endpoint: UiBuffer,
    pub(crate) oauth_scopes: UiBuffer,
    pub(crate) identity_name: UiBuffer,
    pub(crate) identity_email: UiBuffer,
    pub(crate) signature: UiBuffer,
    pub(crate) alias_emails: UiBuffer,
    pub(crate) smtp_host: UiBuffer,
    pub(crate) smtp_port: UiBuffer,
    pub(crate) smtp_security: UiBuffer,
    pub(crate) smtp_username: UiBuffer,
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

impl Default for AccountSetupState {
    fn default() -> Self {
        Self {
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            oauth_preset: None,
            display_name: UiBuffer::single_line(String::new()),
            endpoint: UiBuffer::single_line("imaps://mail.example.com:993"),
            username: UiBuffer::single_line(String::new()),
            password: UiBuffer::secret(String::new()),
            oauth_client_id: UiBuffer::single_line(String::new()),
            oauth_client_secret: UiBuffer::secret(String::new()),
            // Custom provider endpoints, used by JMAP accounts: no consumer
            // provider presets offer JMAP.
            oauth_authorization_endpoint: UiBuffer::single_line(String::new()),
            oauth_token_endpoint: UiBuffer::single_line(String::new()),
            oauth_scopes: UiBuffer::single_line("email"),
            identity_name: UiBuffer::single_line(String::new()),
            identity_email: UiBuffer::single_line(String::new()),
            signature: UiBuffer::multiline("Regards,"),
            alias_emails: UiBuffer::single_line(String::new()),
            smtp_host: UiBuffer::single_line("smtp.example.com"),
            smtp_port: UiBuffer::single_line("587"),
            smtp_security: UiBuffer::single_line("starttls"),
            smtp_username: UiBuffer::single_line(String::new()),
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
    active_field: Option<EditorField>,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            section_card(
                "Connection",
                "Select the mail protocol, then enter the endpoint and authentication used to build the local mirror.",
                colors,
            )
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
            .children(if form.auth == AuthKind::OAuth2 && form.protocol == ProtocolKind::Imap {
                vec![
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(account_oauth_provider_buttons(form, colors, cx)),
                    oauth_client_fields(form, active_field, colors, cx),
                    div()
                        .rounded_md()
                        .bg(rgb(colors.background))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .px_3()
                        .py_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .child(
                                    "Submitting opens the provider's authorization page in the browser; the account syncs once the grant returns. Mail sync and SMTP submission use the OAuth2 access token (XOAUTH2), refreshed in the background.",
                                ),
                        ),
                ]
            } else if form.auth == AuthKind::OAuth2 && form.protocol == ProtocolKind::Jmap {
                vec![
                    oauth_client_fields(form, active_field, colors, cx),
                    div()
                        .flex()
                        .gap_3()
                        .child(
                            div().flex_1().child(
                                input_card(
                                    "account-oauth-authorization-endpoint-field",
                                    "Authorization Endpoint",
                                    "https://provider.example.test/authorize",
                                    &form.oauth_authorization_endpoint,
                                    active_field == Some(EditorField::AccountOauthAuthorizationEndpoint),
                                    false,
                                    colors,
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_active_field(
                                        EditorField::AccountOauthAuthorizationEndpoint,
                                        window,
                                        cx,
                                    );
                                })),
                            ),
                        )
                        .child(
                            div().flex_1().child(
                                input_card(
                                    "account-oauth-token-endpoint-field",
                                    "Token Endpoint",
                                    "https://provider.example.test/token",
                                    &form.oauth_token_endpoint,
                                    active_field == Some(EditorField::AccountOauthTokenEndpoint),
                                    false,
                                    colors,
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_active_field(
                                        EditorField::AccountOauthTokenEndpoint,
                                        window,
                                        cx,
                                    );
                                })),
                            ),
                        ),
                    div().child(
                        input_card(
                            "account-oauth-scopes-field",
                            "Scopes",
                            "Space-separated, for example: email",
                            &form.oauth_scopes,
                            active_field == Some(EditorField::AccountOauthScopes),
                            false,
                            colors,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_active_field(EditorField::AccountOauthScopes, window, cx);
                        })),
                    ),
                    div()
                        .rounded_md()
                        .bg(rgb(colors.background))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .px_3()
                        .py_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .child(
                                    "No consumer presets offer JMAP, so the provider's own OAuth2 endpoints are entered here. The account authenticates with the bearer access token, refreshed in the background.",
                                ),
                        ),
                ]
            } else {
                Vec::new()
            })
            .child(
                input_card(
                    "account-display-name-field",
                    "Display Name",
                    "Work Mail",
                    &form.display_name,
                    active_field == Some(EditorField::AccountDisplayName),
                    false,
                    colors,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_active_field(EditorField::AccountDisplayName, window, cx);
                })),
            )
            .child(
                input_card(
                    "account-endpoint-field",
                    "Endpoint",
                    "imaps://mail.example.com:993 or https://jmap.example.com/.well-known/jmap",
                    &form.endpoint,
                    active_field == Some(EditorField::AccountEndpoint),
                    false,
                    colors,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_active_field(EditorField::AccountEndpoint, window, cx);
                })),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div().flex_1().child(
                            input_card(
                                "account-username-field",
                                "Username",
                                "name@example.com",
                                &form.username,
                                active_field == Some(EditorField::AccountUsername),
                                false,
                                colors,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_active_field(EditorField::AccountUsername, window, cx);
                            })),
                        ),
                    )
                    .children(if form.auth == AuthKind::Password {
                        vec![div().flex_1().child(
                            input_card(
                                "account-password-field",
                                "Password",
                                "App password or account secret",
                                &form.password,
                                active_field == Some(EditorField::AccountPassword),
                                false,
                                colors,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_active_field(EditorField::AccountPassword, window, cx);
                            })),
                        )]
                    } else {
                        Vec::new()
                    }),
            ),
        )
        .child(
            section_card(
                "Identity",
                "Set the default sender name, address, aliases, and signature used for new drafts.",
                colors,
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div().flex_1().child(
                            input_card(
                                "account-identity-name-field",
                                "Primary Identity Name",
                                "Jane Doe",
                                &form.identity_name,
                                active_field == Some(EditorField::AccountIdentityName),
                                false,
                                colors,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_active_field(EditorField::AccountIdentityName, window, cx);
                            })),
                        ),
                    )
                    .child(
                        div().flex_1().child(
                            input_card(
                                "account-identity-email-field",
                                "Primary Identity Email",
                                "jane@example.com",
                                &form.identity_email,
                                active_field == Some(EditorField::AccountIdentityEmail),
                                false,
                                colors,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_active_field(EditorField::AccountIdentityEmail, window, cx);
                            })),
                        ),
                    ),
            )
            .child(
                input_card(
                    "account-alias-emails-field",
                    "Additional Identity Emails",
                    "alias@example.com, billing@example.com",
                    &form.alias_emails,
                    active_field == Some(EditorField::AccountAliasEmails),
                    false,
                    colors,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_active_field(EditorField::AccountAliasEmails, window, cx);
                })),
            )
            .child(
                input_card(
                    "account-signature-field",
                    "Signature",
                    "Regards,\nJane",
                    &form.signature,
                    active_field == Some(EditorField::AccountSignature),
                    true,
                    colors,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_active_field(EditorField::AccountSignature, window, cx);
                })),
            ),
        )
        .child(
            section_card(
                "Outgoing Mail (SMTP)",
                "Configure submission for IMAP accounts. JMAP accounts submit through the server endpoint instead.",
                colors,
            )
            .child(if form.protocol == ProtocolKind::Imap {
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        input_card(
                            "account-smtp-host-field",
                            "SMTP Host",
                            "smtp.example.com",
                            &form.smtp_host,
                            active_field == Some(EditorField::AccountSmtpHost),
                            false,
                            colors,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_active_field(EditorField::AccountSmtpHost, window, cx);
                        })),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div().flex_1().child(
                                    input_card(
                                        "account-smtp-port-field",
                                        "SMTP Port",
                                        "587",
                                        &form.smtp_port,
                                        active_field == Some(EditorField::AccountSmtpPort),
                                        false,
                                        colors,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_active_field(EditorField::AccountSmtpPort, window, cx);
                                    })),
                                ),
                            )
                            .child(
                                div().flex_1().child(
                                    input_card(
                                        "account-smtp-security-field",
                                        "SMTP Security",
                                        "plain, starttls, or implicit_tls",
                                        &form.smtp_security,
                                        active_field == Some(EditorField::AccountSmtpSecurity),
                                        false,
                                        colors,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_active_field(EditorField::AccountSmtpSecurity, window, cx);
                                    })),
                                ),
                            ),
                    )
                    .child(
                        input_card(
                            "account-smtp-username-field",
                            "SMTP Username",
                            "Optional override for SMTP auth",
                            &form.smtp_username,
                            active_field == Some(EditorField::AccountSmtpUsername),
                            false,
                            colors,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_active_field(EditorField::AccountSmtpUsername, window, cx);
                        })),
                    )
            } else {
                div()
                    .rounded_md()
                    .bg(rgb(colors.background))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .px_3()
                    .py_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(colors.muted))
                            .child(
                                "JMAP handles message submission directly, so no separate SMTP settings are required here.",
                            ),
                    )
            }),
        )
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
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.account_setup.protocol = ProtocolKind::Imap;
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
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.account_setup.protocol = ProtocolKind::Jmap;
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
fn oauth_client_fields(
    form: &AccountSetupState,
    active_field: Option<EditorField>,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    div()
        .flex()
        .gap_3()
        .child(
            div().flex_1().child(
                input_card(
                    "account-oauth-client-id-field",
                    "OAuth2 Client Id",
                    "Provider application client id",
                    &form.oauth_client_id,
                    active_field == Some(EditorField::AccountOauthClientId),
                    false,
                    colors,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_active_field(EditorField::AccountOauthClientId, window, cx);
                })),
            ),
        )
        .child(
            div().flex_1().child(
                input_card(
                    "account-oauth-client-secret-field",
                    "OAuth2 Client Secret",
                    "Optional for public clients",
                    &form.oauth_client_secret,
                    active_field == Some(EditorField::AccountOauthClientSecret),
                    false,
                    colors,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_active_field(EditorField::AccountOauthClientSecret, window, cx);
                })),
            ),
        )
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
            colors.border,
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
            colors.border,
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
                colors.border,
                colors.foreground,
                cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                    this.account_setup.oauth_preset = Some(preset);
                    this.account_setup.endpoint =
                        UiBuffer::single_line(preset_endpoint(preset).to_owned());
                    let (host, port, security) = preset_smtp(preset);
                    this.account_setup.smtp_host = UiBuffer::single_line(host.to_owned());
                    this.account_setup.smtp_port = UiBuffer::single_line(port.to_owned());
                    this.account_setup.smtp_security = UiBuffer::single_line(security.to_owned());
                    cx.notify();
                }),
            )
            .into_any_element()
        })
        .collect()
}
