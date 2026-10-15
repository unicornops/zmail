//! Account setup form state, the account setup render function, and the
//! protocol-selector action buttons.
//!
//! [`AccountSetupState`] holds the in-memory form backing the new-account
//! wizard. [`render_account_setup_detail`] produces the grouped form content,
//! and [`account_protocol_buttons`] produces the toggle buttons used for
//! selecting IMAP+SMTP vs JMAP.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, Context, IntoElement, Stateful, StatefulInteractiveElement, div, prelude::*, rgb,
};
use zmail_auth::OAuth2Preset;
use zmail_core::{AuthKind, ProtocolKind};

use super::WorkspaceView;
use crate::input::{EditorField, UiBuffer};
use crate::styles::{ICON_SIZE_SM, UiColors};
use crate::widgets::{EllipsisExt, icon, input_card, section_card, toolbar_button};

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
    pub(crate) disk_budget_mb: UiBuffer,
    pub(crate) identity_name: UiBuffer,
    pub(crate) identity_email: UiBuffer,
    pub(crate) signature: UiBuffer,
    pub(crate) alias_emails: UiBuffer,
    pub(crate) smtp_host: UiBuffer,
    pub(crate) smtp_port: UiBuffer,
    pub(crate) smtp_security: UiBuffer,
    pub(crate) smtp_username: UiBuffer,
    /// Validation errors shown under their fields; editing a field clears
    /// its error.
    pub(crate) errors: BTreeMap<EditorField, String>,
    /// Why the form opened when the user did not ask for it, for example
    /// Compose with no account.
    pub(crate) notice: Option<&'static str>,
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

impl Default for AccountSetupState {
    fn default() -> Self {
        Self {
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            oauth_preset: None,
            display_name: UiBuffer::single_line(String::new()),
            // Empty, so the protocol-specific placeholder shows instead of
            // an IMAP URL that JMAP accounts would have to delete.
            endpoint: UiBuffer::single_line(String::new()),
            username: UiBuffer::single_line(String::new()),
            password: UiBuffer::secret(String::new()),
            oauth_client_id: UiBuffer::single_line(String::new()),
            oauth_client_secret: UiBuffer::secret(String::new()),
            // Custom provider endpoints, used by JMAP accounts: no consumer
            // provider presets offer JMAP.
            oauth_authorization_endpoint: UiBuffer::single_line(String::new()),
            oauth_token_endpoint: UiBuffer::single_line(String::new()),
            oauth_scopes: UiBuffer::single_line("email"),
            // Empty means unlimited; a number is the budget in MiB that
            // the post-sync eviction pass enforces.
            disk_budget_mb: UiBuffer::single_line(String::new()),
            identity_name: UiBuffer::single_line(String::new()),
            identity_email: UiBuffer::single_line(String::new()),
            signature: UiBuffer::multiline("Regards,"),
            alias_emails: UiBuffer::single_line(String::new()),
            smtp_host: UiBuffer::single_line(String::new()),
            smtp_port: UiBuffer::single_line("587"),
            smtp_security: UiBuffer::single_line("starttls"),
            smtp_username: UiBuffer::single_line(String::new()),
            errors: BTreeMap::new(),
            notice: None,
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
                    oauth_client_fields(form, active_field, colors, cx),
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
                    oauth_client_fields(form, active_field, colors, cx),
                    div()
                        .flex()
                        .gap_3()
                        .child(
                            div().flex_1().min_w_0().child(
                                account_input(
 form,
 active_field,
 EditorField::AccountOauthAuthorizationEndpoint,
 "account-oauth-authorization-endpoint-field",
 "Authorization Endpoint",
 "https://provider.example.test/authorize",
 &form.oauth_authorization_endpoint,
 false,
 colors,
 cx,
),
                            ),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                account_input(
 form,
 active_field,
 EditorField::AccountOauthTokenEndpoint,
 "account-oauth-token-endpoint-field",
 "Token Endpoint",
 "https://provider.example.test/token",
 &form.oauth_token_endpoint,
 false,
 colors,
 cx,
),
                            ),
                        ),
                    div().child(
                        account_input(
 form,
 active_field,
 EditorField::AccountOauthScopes,
 "account-oauth-scopes-field",
 "Scopes",
 "Space-separated, for example: email",
 &form.oauth_scopes,
 false,
 colors,
 cx,
),
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
                .child(account_input(
                    form,
                    active_field,
                    EditorField::AccountDisplayName,
                    "account-display-name-field",
                    "Display Name",
                    "Work Mail",
                    &form.display_name,
                    false,
                    colors,
                    cx,
                ))
                .child(account_input(
                    form,
                    active_field,
                    EditorField::AccountEndpoint,
                    "account-endpoint-field",
                    "Endpoint",
                    endpoint_placeholder(form.protocol),
                    &form.endpoint,
                    false,
                    colors,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(account_input(
                            form,
                            active_field,
                            EditorField::AccountUsername,
                            "account-username-field",
                            "Username",
                            "name@example.com",
                            &form.username,
                            false,
                            colors,
                            cx,
                        )))
                        .children(if form.auth == AuthKind::Password {
                            vec![div().flex_1().min_w_0().child(account_input(
                                form,
                                active_field,
                                EditorField::AccountPassword,
                                "account-password-field",
                                "Password",
                                "App password or account secret",
                                &form.password,
                                false,
                                colors,
                                cx,
                            ))]
                        } else {
                            Vec::new()
                        }),
                ),
        )
        .child(account_input(
            form,
            active_field,
            EditorField::AccountDiskBudget,
            "account-disk-budget-field",
            "Disk Budget (MiB)",
            "Empty for unlimited",
            &form.disk_budget_mb,
            false,
            colors,
            cx,
        ))
        .child(
            section_card("Identity", colors)
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(account_input(
                            form,
                            active_field,
                            EditorField::AccountIdentityName,
                            "account-identity-name-field",
                            "Primary Identity Name",
                            "Jane Doe",
                            &form.identity_name,
                            false,
                            colors,
                            cx,
                        )))
                        .child(div().flex_1().min_w_0().child(account_input(
                            form,
                            active_field,
                            EditorField::AccountIdentityEmail,
                            "account-identity-email-field",
                            "Primary Identity Email",
                            "jane@example.com",
                            &form.identity_email,
                            false,
                            colors,
                            cx,
                        ))),
                )
                .child(account_input(
                    form,
                    active_field,
                    EditorField::AccountAliasEmails,
                    "account-alias-emails-field",
                    "Additional Identity Emails",
                    "alias@example.com, billing@example.com",
                    &form.alias_emails,
                    false,
                    colors,
                    cx,
                ))
                .child(account_input(
                    form,
                    active_field,
                    EditorField::AccountSignature,
                    "account-signature-field",
                    "Signature",
                    "Regards,\nJane",
                    &form.signature,
                    true,
                    colors,
                    cx,
                )),
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
                        .child(account_input(
                            form,
                            active_field,
                            EditorField::AccountSmtpHost,
                            "account-smtp-host-field",
                            "SMTP Host",
                            "smtp.example.com",
                            &form.smtp_host,
                            false,
                            colors,
                            cx,
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child(div().flex_1().min_w_0().child(account_input(
                                    form,
                                    active_field,
                                    EditorField::AccountSmtpPort,
                                    "account-smtp-port-field",
                                    "SMTP Port",
                                    "587",
                                    &form.smtp_port,
                                    false,
                                    colors,
                                    cx,
                                )))
                                .child(div().flex_1().min_w_0().child(account_input(
                                    form,
                                    active_field,
                                    EditorField::AccountSmtpSecurity,
                                    "account-smtp-security-field",
                                    "SMTP Security",
                                    "plain, starttls, or implicit_tls",
                                    &form.smtp_security,
                                    false,
                                    colors,
                                    cx,
                                ))),
                        )
                        .child(account_input(
                            form,
                            active_field,
                            EditorField::AccountSmtpUsername,
                            "account-smtp-username-field",
                            "SMTP Username",
                            "Optional override for SMTP auth",
                            &form.smtp_username,
                            false,
                            colors,
                            cx,
                        )),
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
            if protocol == ProtocolKind::Jmap {
                colors.accent
            } else {
                colors.border
            },
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
        .child(div().flex_1().min_w_0().child(account_input(
            form,
            active_field,
            EditorField::AccountOauthClientId,
            "account-oauth-client-id-field",
            "OAuth2 Client Id",
            "Provider application client id",
            &form.oauth_client_id,
            false,
            colors,
            cx,
        )))
        .child(div().flex_1().min_w_0().child(account_input(
            form,
            active_field,
            EditorField::AccountOauthClientSecret,
            "account-oauth-client-secret-field",
            "OAuth2 Client Secret",
            "Optional for public clients",
            &form.oauth_client_secret,
            false,
            colors,
            cx,
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

/// An account-setup input with its click-to-focus handler and, when the
/// last submit found a problem with it, the error underneath.
// The label, placeholder and buffer are what make each field distinct.
#[allow(clippy::too_many_arguments)]
fn account_input(
    form: &AccountSetupState,
    active_field: Option<EditorField>,
    field: EditorField,
    id: &'static str,
    label: &str,
    placeholder: &str,
    buffer: &UiBuffer,
    multiline: bool,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> Stateful<gpui::Div> {
    let danger = colors.danger;
    input_card(
        id,
        label,
        placeholder,
        buffer,
        active_field == Some(field),
        multiline,
        colors,
    )
    .on_click(cx.listener(move |this, _, window, cx| {
        this.set_active_field(field, window, cx);
    }))
    .when_some(form.errors.get(&field).cloned(), move |this, error| {
        this.child(
            div()
                .w_full()
                .text_sm()
                .text_color(rgb(danger))
                .ellipsis()
                .child(error),
        )
    })
}
