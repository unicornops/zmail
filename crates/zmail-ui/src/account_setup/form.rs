//! The add-account form's live state: a [`TextInput`] per text field and
//! the choices, kept in GPUI entities.
//!
//! The fields, their validation and building the account from them are
//! shared with the terminal UI in `zmail_setup::form`; [`AccountForm`]
//! hands them an [`AccountFormValues`] snapshot and writes back what they
//! change.

use std::collections::BTreeMap;

use gpui::{App, AppContext as _, Entity};
use zmail_autodiscover::{MailSettings, Provider, Security};
use zmail_core::ProtocolKind;
pub(crate) use zmail_setup::form::{
    AccountField, AccountFormValues, Check, IMAP_SECURITY, SMTP_SECURITY, oauth_secret_label,
    oauth_secret_placeholder, security_label, validate_account_form,
};

use crate::components::TextInput;

// ---------------------------------------------------------------------------
// AccountForm
// ---------------------------------------------------------------------------

/// The form's live state: a [`TextInput`] per text field, the choices, and
/// the errors from the last check.
pub(crate) struct AccountForm {
    inputs: BTreeMap<AccountField, Entity<TextInput>>,
    pub(crate) provider: Option<Provider>,
    pub(crate) use_oauth: bool,
    pub(crate) protocol: ProtocolKind,
    pub(crate) imap_security: Security,
    pub(crate) smtp_security: Security,
    pub(crate) allow_cleartext: bool,
    /// Errors shown under their fields; editing a field clears its error.
    pub(crate) errors: BTreeMap<AccountField, String>,
}

impl AccountForm {
    /// A new, empty form with its default values.
    pub(crate) fn new(cx: &mut App) -> Self {
        let defaults = AccountFormValues::default();
        let inputs = AccountField::TEXT
            .into_iter()
            .map(|field| {
                let value = defaults.value(field).to_owned();
                let input = cx.new(|cx| {
                    let input = TextInput::new(cx)
                        .label(field.label())
                        .placeholder(field.placeholder());
                    let input = match field {
                        AccountField::Password
                        | AccountField::OauthClientSecret
                        | AccountField::ProtonMailboxPassword => input.masked(),
                        _ => input,
                    };
                    input.with_text(value)
                });
                (field, input)
            })
            .collect();
        Self {
            inputs,
            provider: defaults.provider,
            use_oauth: defaults.use_oauth,
            protocol: defaults.protocol,
            imap_security: defaults.imap_security,
            smtp_security: defaults.smtp_security,
            allow_cleartext: defaults.allow_cleartext,
            errors: BTreeMap::new(),
        }
    }

    /// The input entity for a text field.
    pub(crate) fn input(&self, field: AccountField) -> &Entity<TextInput> {
        &self.inputs[&field]
    }

    /// The current text of a field.
    pub(crate) fn text(&self, field: AccountField, cx: &App) -> String {
        self.input(field).read(cx).text().to_owned()
    }

    /// Replace a field's text.
    pub(crate) fn set_text(&self, field: AccountField, text: &str, cx: &mut App) {
        self.input(field)
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    /// Snapshot the form's current values.
    pub(crate) fn values(&self, cx: &App) -> AccountFormValues {
        let text = |field: AccountField| self.text(field, cx);
        AccountFormValues {
            provider: self.provider,
            name: text(AccountField::Name),
            email: text(AccountField::Email),
            password: text(AccountField::Password),
            use_oauth: self.use_oauth,
            oauth_client_id: text(AccountField::OauthClientId),
            oauth_client_secret: text(AccountField::OauthClientSecret),
            oauth_authorization_endpoint: text(AccountField::OauthAuthorizationEndpoint),
            oauth_token_endpoint: text(AccountField::OauthTokenEndpoint),
            oauth_scopes: text(AccountField::OauthScopes),
            protocol: self.protocol,
            jmap_url: text(AccountField::JmapUrl),
            imap_host: text(AccountField::ImapHost),
            imap_port: text(AccountField::ImapPort),
            imap_security: self.imap_security,
            imap_username: text(AccountField::ImapUsername),
            smtp_host: text(AccountField::SmtpHost),
            smtp_port: text(AccountField::SmtpPort),
            smtp_security: self.smtp_security,
            smtp_username: text(AccountField::SmtpUsername),
            allow_cleartext: self.allow_cleartext,
            // The GUI's identity is the address and the name above.
            identity_email: String::new(),
            signature: String::new(),
        }
    }

    /// Sets the provider, and with it what the client secret field asks
    /// for.
    pub(crate) fn set_provider(&mut self, provider: Option<Provider>, cx: &mut App) {
        self.provider = provider;
        self.input(AccountField::OauthClientSecret)
            .update(cx, |input, cx| {
                input.set_placeholder(oauth_secret_placeholder(provider), cx)
            });
    }

    /// A field's visible label, which for the client secret depends on the
    /// provider.
    pub(crate) fn label(&self, field: AccountField) -> &'static str {
        match field {
            AccountField::OauthClientSecret => oauth_secret_label(self.provider),
            field => field.label(),
        }
    }

    /// Applies a change from `zmail_setup` to a snapshot of the form, then
    /// writes back the fields it changed (only those, so the others keep
    /// their cursor and undo history).
    pub(crate) fn update(&mut self, cx: &mut App, change: impl FnOnce(&mut AccountFormValues)) {
        let before = self.values(cx);
        let mut after = before.clone();
        change(&mut after);
        for field in AccountField::TEXT {
            if after.value(field) != before.value(field) {
                self.set_text(field, after.value(field), cx);
            }
        }
        self.protocol = after.protocol;
        self.imap_security = after.imap_security;
        self.smtp_security = after.smtp_security;
    }

    /// Fills the server fields from discovered (or preset) settings.
    pub(crate) fn apply_settings(&mut self, settings: &MailSettings, cx: &mut App) {
        self.update(cx, |values| values.apply_settings(settings));
    }

    /// Changes the IMAP security, moving the port along when it is still
    /// the previous choice's usual port.
    pub(crate) fn set_imap_security(&mut self, security: Security, cx: &mut App) {
        self.update(cx, |values| values.set_imap_security(security));
        self.errors.remove(&AccountField::ImapSecurity);
    }

    /// Changes the SMTP security, moving the port along when it is still
    /// the previous choice's usual port.
    pub(crate) fn set_smtp_security(&mut self, security: Security, cx: &mut App) {
        self.update(cx, |values| values.set_smtp_security(security));
        self.errors.remove(&AccountField::SmtpSecurity);
    }
}
