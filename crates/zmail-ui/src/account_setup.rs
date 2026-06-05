//! Account setup form state, the account setup render function, and the
//! protocol-selector action buttons.
//!
//! [`AccountSetupState`] holds the in-memory form backing the new-account
//! wizard. [`render_account_setup_detail`] produces the grouped form content,
//! and [`account_protocol_buttons`] produces the toggle buttons used for
//! selecting IMAP+SMTP vs JMAP.

use gpui::{AnyElement, Context, IntoElement, StatefulInteractiveElement, div, prelude::*, rgb};
use zmail_core::ProtocolKind;

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
/// are shown.
#[derive(Debug, Clone)]
pub(crate) struct AccountSetupState {
    pub(crate) protocol: ProtocolKind,
    pub(crate) display_name: UiBuffer,
    pub(crate) endpoint: UiBuffer,
    pub(crate) username: UiBuffer,
    pub(crate) password: UiBuffer,
    pub(crate) identity_name: UiBuffer,
    pub(crate) identity_email: UiBuffer,
    pub(crate) signature: UiBuffer,
    pub(crate) alias_emails: UiBuffer,
    pub(crate) smtp_host: UiBuffer,
    pub(crate) smtp_port: UiBuffer,
    pub(crate) smtp_security: UiBuffer,
    pub(crate) smtp_username: UiBuffer,
}

impl Default for AccountSetupState {
    fn default() -> Self {
        Self {
            protocol: ProtocolKind::Imap,
            display_name: UiBuffer::single_line(String::new()),
            endpoint: UiBuffer::single_line("imaps://mail.example.com:993"),
            username: UiBuffer::single_line(String::new()),
            password: UiBuffer::secret(String::new()),
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
                    .child(
                        div().flex_1().child(
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
                        ),
                    ),
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
                cx.notify();
            }),
        )
        .into_any_element(),
    ]
}
