//! The add-account form's fields, their validation, and building the
//! account from them.
//!
//! Every way through the flow ends in the same fields: a provider preset
//! or an autodiscovery result fills the server fields, and the manual form
//! shows them for editing. [`build_account_from_form`] then turns one
//! [`AccountFormValues`] snapshot into the stored account, whichever way
//! the values got there.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use gpui::{App, AppContext as _, Entity};
use url::Url;
use zmail_auth::OAuth2Preset;
use zmail_autodiscover::{EmailAddress, MailSettings, Provider, Security, ServerSettings};
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutgoingConfig, ProtocolKind,
    SubmissionSecurity,
};

use crate::components::TextInput;

// ---------------------------------------------------------------------------
// AccountField
// ---------------------------------------------------------------------------

/// One field of the add-account flow, for validation errors and focus.
/// The text fields each have a [`TextInput`]; the security choices and the
/// cleartext opt-in are controls with errors of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum AccountField {
    Name,
    Email,
    Password,
    OauthClientId,
    OauthClientSecret,
    OauthAuthorizationEndpoint,
    OauthTokenEndpoint,
    OauthScopes,
    JmapUrl,
    ImapHost,
    ImapPort,
    ImapSecurity,
    ImapUsername,
    SmtpHost,
    SmtpPort,
    SmtpSecurity,
    SmtpUsername,
    AllowCleartext,
}

impl AccountField {
    /// The text fields, in form order.
    pub(crate) const TEXT: [AccountField; 15] = [
        AccountField::Name,
        AccountField::Email,
        AccountField::Password,
        AccountField::OauthClientId,
        AccountField::OauthClientSecret,
        AccountField::OauthAuthorizationEndpoint,
        AccountField::OauthTokenEndpoint,
        AccountField::OauthScopes,
        AccountField::JmapUrl,
        AccountField::ImapHost,
        AccountField::ImapPort,
        AccountField::ImapUsername,
        AccountField::SmtpHost,
        AccountField::SmtpPort,
        AccountField::SmtpUsername,
    ];

    /// The visible label, also the field's accessible name.
    pub(crate) fn label(self) -> &'static str {
        match self {
            AccountField::Name => "Your name",
            AccountField::Email => "Email address",
            AccountField::Password => "Password",
            AccountField::OauthClientId => "OAuth client ID",
            AccountField::OauthClientSecret => "OAuth client secret",
            AccountField::OauthAuthorizationEndpoint => "Authorization endpoint",
            AccountField::OauthTokenEndpoint => "Token endpoint",
            AccountField::OauthScopes => "Scopes",
            AccountField::JmapUrl => "JMAP session URL",
            AccountField::ImapHost => "IMAP server",
            AccountField::ImapPort => "Port",
            AccountField::ImapSecurity => "Security",
            AccountField::ImapUsername => "User name",
            AccountField::SmtpHost => "SMTP server",
            AccountField::SmtpPort => "Port",
            AccountField::SmtpSecurity => "Security",
            AccountField::SmtpUsername => "User name",
            AccountField::AllowCleartext => "Connect without encryption",
        }
    }

    /// Placeholder text.
    fn placeholder(self) -> &'static str {
        match self {
            AccountField::Name => "Jane Doe",
            AccountField::Email => "jane@example.com",
            AccountField::Password => "Password or app password",
            AccountField::OauthClientId => "From your provider's developer console",
            AccountField::OauthClientSecret => oauth_secret_placeholder(None),
            AccountField::OauthAuthorizationEndpoint => "https://provider.example.com/authorize",
            AccountField::OauthTokenEndpoint => "https://provider.example.com/token",
            AccountField::OauthScopes => "Space-separated, for example: email",
            AccountField::JmapUrl => "https://mail.example.com/.well-known/jmap",
            AccountField::ImapHost => "imap.example.com",
            AccountField::ImapPort => "993",
            AccountField::ImapUsername | AccountField::SmtpUsername => "Usually your email address",
            AccountField::SmtpHost => "smtp.example.com",
            AccountField::SmtpPort => "465",
            AccountField::ImapSecurity
            | AccountField::SmtpSecurity
            | AccountField::AllowCleartext => "",
        }
    }
}

/// Whether the provider's OAuth clients need their secret at the token
/// exchange. Google's desktop clients do, though the secret is not
/// confidential there; Microsoft's public clients have none.
pub(crate) fn oauth_secret_required(provider: Option<Provider>) -> bool {
    provider == Some(Provider::Google)
}

/// The client secret field's visible label for the provider.
pub(crate) fn oauth_secret_label(provider: Option<Provider>) -> &'static str {
    if oauth_secret_required(provider) {
        "OAuth client secret"
    } else {
        "OAuth client secret (optional)"
    }
}

/// The client secret field's placeholder for the provider.
pub(crate) fn oauth_secret_placeholder(provider: Option<Provider>) -> &'static str {
    if oauth_secret_required(provider) {
        "Required: shown beside the client ID in Google Cloud"
    } else {
        "Leave empty for a public client"
    }
}

// ---------------------------------------------------------------------------
// Security choices
// ---------------------------------------------------------------------------

/// The security choices the IMAP Select offers. zmail's IMAP client speaks
/// implicit TLS or cleartext, not STARTTLS.
pub(crate) const IMAP_SECURITY: [Security; 2] = [Security::Tls, Security::None];

/// The security choices the SMTP Select offers.
pub(crate) const SMTP_SECURITY: [Security; 3] = [Security::Tls, Security::StartTls, Security::None];

/// The usual IMAP port for a security choice.
pub(crate) fn imap_default_port(security: Security) -> u16 {
    match security {
        Security::Tls => 993,
        Security::StartTls | Security::None => 143,
    }
}

/// The usual submission port for a security choice (RFC 8314: 465 for
/// implicit TLS, 587 for STARTTLS).
pub(crate) fn smtp_default_port(security: Security) -> u16 {
    match security {
        Security::Tls => 465,
        Security::StartTls | Security::None => 587,
    }
}

/// The label a security Select shows for a choice.
pub(crate) fn security_label(security: Security) -> &'static str {
    match security {
        Security::Tls => "TLS",
        Security::StartTls => "STARTTLS",
        Security::None => "None (unencrypted)",
    }
}

// ---------------------------------------------------------------------------
// AccountFormValues
// ---------------------------------------------------------------------------

/// A snapshot of the form, which validation and account building work on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountFormValues {
    /// The provider button picked; `None` is "Other".
    pub(crate) provider: Option<Provider>,
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) password: String,
    /// Sign in with OAuth2 instead of a password.
    pub(crate) use_oauth: bool,
    pub(crate) oauth_client_id: String,
    pub(crate) oauth_client_secret: String,
    pub(crate) oauth_authorization_endpoint: String,
    pub(crate) oauth_token_endpoint: String,
    pub(crate) oauth_scopes: String,
    pub(crate) protocol: ProtocolKind,
    pub(crate) jmap_url: String,
    pub(crate) imap_host: String,
    pub(crate) imap_port: String,
    pub(crate) imap_security: Security,
    pub(crate) imap_username: String,
    pub(crate) smtp_host: String,
    pub(crate) smtp_port: String,
    pub(crate) smtp_security: Security,
    pub(crate) smtp_username: String,
    /// The user confirmed an unencrypted connection.
    pub(crate) allow_cleartext: bool,
}

impl Default for AccountFormValues {
    /// The values a new form starts with: TLS everywhere.
    fn default() -> Self {
        Self {
            provider: None,
            name: String::new(),
            email: String::new(),
            password: String::new(),
            use_oauth: false,
            oauth_client_id: String::new(),
            oauth_client_secret: String::new(),
            oauth_authorization_endpoint: String::new(),
            oauth_token_endpoint: String::new(),
            oauth_scopes: "email".to_owned(),
            protocol: ProtocolKind::Imap,
            jmap_url: String::new(),
            imap_host: String::new(),
            imap_port: imap_default_port(Security::Tls).to_string(),
            imap_security: Security::Tls,
            imap_username: String::new(),
            smtp_host: String::new(),
            smtp_port: smtp_default_port(Security::Tls).to_string(),
            smtp_security: Security::Tls,
            smtp_username: String::new(),
            allow_cleartext: false,
        }
    }
}

impl AccountFormValues {
    /// The value of one text field.
    fn value(&self, field: AccountField) -> &str {
        match field {
            AccountField::Name => &self.name,
            AccountField::Email => &self.email,
            AccountField::Password => &self.password,
            AccountField::OauthClientId => &self.oauth_client_id,
            AccountField::OauthClientSecret => &self.oauth_client_secret,
            AccountField::OauthAuthorizationEndpoint => &self.oauth_authorization_endpoint,
            AccountField::OauthTokenEndpoint => &self.oauth_token_endpoint,
            AccountField::OauthScopes => &self.oauth_scopes,
            AccountField::JmapUrl => &self.jmap_url,
            AccountField::ImapHost => &self.imap_host,
            AccountField::ImapPort => &self.imap_port,
            AccountField::ImapUsername => &self.imap_username,
            AccountField::SmtpHost => &self.smtp_host,
            AccountField::SmtpPort => &self.smtp_port,
            AccountField::SmtpUsername => &self.smtp_username,
            AccountField::ImapSecurity
            | AccountField::SmtpSecurity
            | AccountField::AllowCleartext => "",
        }
    }

    /// Whether any connection these values describe is unencrypted.
    pub(crate) fn uses_cleartext(&self) -> bool {
        match self.protocol {
            ProtocolKind::Imap => {
                self.imap_security == Security::None || self.smtp_security == Security::None
            }
            ProtocolKind::Jmap => self.jmap_url.trim().starts_with("http://"),
            // Proton's API is HTTPS only, and its accounts never use this form.
            ProtocolKind::Proton => false,
        }
    }
}

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
                        AccountField::Password | AccountField::OauthClientSecret => input.masked(),
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

    /// Fills the server fields from discovered (or preset) settings.
    pub(crate) fn apply_settings(&mut self, settings: &MailSettings, cx: &mut App) {
        match settings {
            MailSettings::Jmap(jmap) => {
                self.protocol = ProtocolKind::Jmap;
                self.set_text(AccountField::JmapUrl, &jmap.value.session_url, cx);
            }
            MailSettings::ImapSmtp { imap, smtp } => {
                self.protocol = ProtocolKind::Imap;
                self.apply_imap(&imap.value, cx);
                self.apply_smtp(&smtp.value, cx);
            }
        }
    }

    /// Fills the IMAP fields.
    pub(crate) fn apply_imap(&mut self, server: &ServerSettings, cx: &mut App) {
        self.imap_security = server.security;
        self.set_text(AccountField::ImapHost, &server.host, cx);
        self.set_text(AccountField::ImapPort, &server.port.to_string(), cx);
        self.set_text(AccountField::ImapUsername, &server.username, cx);
    }

    /// Fills the SMTP fields.
    pub(crate) fn apply_smtp(&mut self, server: &ServerSettings, cx: &mut App) {
        self.smtp_security = server.security;
        self.set_text(AccountField::SmtpHost, &server.host, cx);
        self.set_text(AccountField::SmtpPort, &server.port.to_string(), cx);
        self.set_text(AccountField::SmtpUsername, &server.username, cx);
    }

    /// Fills the manual form with the usual guesses for the address's
    /// domain, when nothing better was found.
    pub(crate) fn prefill_guesses(&mut self, email: &EmailAddress, cx: &mut App) {
        let domain = email.domain();
        if self.text(AccountField::ImapHost, cx).trim().is_empty() {
            self.set_text(AccountField::ImapHost, &format!("imap.{domain}"), cx);
            self.set_text(AccountField::ImapUsername, &email.to_string(), cx);
        }
        if self.text(AccountField::SmtpHost, cx).trim().is_empty() {
            self.set_text(AccountField::SmtpHost, &format!("smtp.{domain}"), cx);
            self.set_text(AccountField::SmtpUsername, &email.to_string(), cx);
        }
    }

    /// Changes the IMAP security, moving the port along when it is still
    /// the previous choice's usual port.
    pub(crate) fn set_imap_security(&mut self, security: Security, cx: &mut App) {
        let port = self.text(AccountField::ImapPort, cx);
        if port.trim() == imap_default_port(self.imap_security).to_string() {
            self.set_text(
                AccountField::ImapPort,
                &imap_default_port(security).to_string(),
                cx,
            );
        }
        self.imap_security = security;
        self.errors.remove(&AccountField::ImapSecurity);
    }

    /// Changes the SMTP security, moving the port along when it is still
    /// the previous choice's usual port.
    pub(crate) fn set_smtp_security(&mut self, security: Security, cx: &mut App) {
        let port = self.text(AccountField::SmtpPort, cx);
        if port.trim() == smtp_default_port(self.smtp_security).to_string() {
            self.set_text(
                AccountField::SmtpPort,
                &smtp_default_port(security).to_string(),
                cx,
            );
        }
        self.smtp_security = security;
        self.errors.remove(&AccountField::SmtpSecurity);
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Which part of the flow is being checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    /// The sign-in step: name, address and password (or OAuth2).
    Credentials,
    /// Everything, before the account is stored.
    All,
}

/// Checks the form and returns an error message for each field that needs
/// fixing, so the form can show them beside the fields. It mirrors the
/// checks in [`build_account_from_form`].
pub(crate) fn validate_account_form(
    form: &AccountFormValues,
    check: Check,
) -> Vec<(AccountField, String)> {
    let mut errors = Vec::new();
    /// Records `message` when `value` is empty; returns whether it isn't.
    fn require(
        errors: &mut Vec<(AccountField, String)>,
        field: AccountField,
        value: &str,
        message: &str,
    ) -> bool {
        let present = !value.trim().is_empty();
        if !present {
            errors.push((field, message.to_owned()));
        }
        present
    }

    if require(
        &mut errors,
        AccountField::Email,
        &form.email,
        "Enter your email address.",
    ) && let Err(error) = EmailAddress::parse(&form.email)
    {
        errors.push((AccountField::Email, error.to_string()));
    }
    if form.use_oauth {
        require(
            &mut errors,
            AccountField::OauthClientId,
            &form.oauth_client_id,
            "Enter the OAuth client ID (under Advanced).",
        );
        if oauth_secret_required(form.provider) {
            require(
                &mut errors,
                AccountField::OauthClientSecret,
                &form.oauth_client_secret,
                "Google needs the client secret as well as the ID. Copy it from the client in Google Cloud.",
            );
        }
        if oauth_preset(form).is_none() {
            require(
                &mut errors,
                AccountField::OauthAuthorizationEndpoint,
                &form.oauth_authorization_endpoint,
                "Enter the authorization endpoint.",
            );
            require(
                &mut errors,
                AccountField::OauthTokenEndpoint,
                &form.oauth_token_endpoint,
                "Enter the token endpoint.",
            );
        }
    } else {
        require(
            &mut errors,
            AccountField::Password,
            &form.password,
            "Enter the password or app password.",
        );
    }
    if check == Check::Credentials {
        return errors;
    }

    match form.protocol {
        // Proton accounts are added by signing in to Proton, not here.
        ProtocolKind::Proton => {}
        ProtocolKind::Jmap => {
            if require(
                &mut errors,
                AccountField::JmapUrl,
                &form.jmap_url,
                "Enter the JMAP session URL.",
            ) && let Err(error) = validate_jmap_url(&form.jmap_url)
            {
                errors.push((AccountField::JmapUrl, error.to_string()));
            }
        }
        ProtocolKind::Imap => {
            if require(
                &mut errors,
                AccountField::ImapHost,
                &form.imap_host,
                "Enter the IMAP server.",
            ) && let Err(error) = validate_host(&form.imap_host)
            {
                errors.push((AccountField::ImapHost, error.to_string()));
            }
            if parse_port(&form.imap_port).is_err() {
                errors.push((
                    AccountField::ImapPort,
                    "Enter a port number, for example 993.".to_owned(),
                ));
            }
            if !IMAP_SECURITY.contains(&form.imap_security) {
                errors.push((
                    AccountField::ImapSecurity,
                    "zmail can't use STARTTLS for IMAP yet. Choose TLS (usually port 993)."
                        .to_owned(),
                ));
            }
            if require(
                &mut errors,
                AccountField::SmtpHost,
                &form.smtp_host,
                "Enter the SMTP server.",
            ) && let Err(error) = validate_host(&form.smtp_host)
            {
                errors.push((AccountField::SmtpHost, error.to_string()));
            }
            if parse_port(&form.smtp_port).is_err() {
                errors.push((
                    AccountField::SmtpPort,
                    "Enter a port number, for example 465.".to_owned(),
                ));
            }
        }
    }
    if form.uses_cleartext() && !form.allow_cleartext {
        errors.push((
            AccountField::AllowCleartext,
            "Confirm that you want to connect without encryption, or choose TLS.".to_owned(),
        ));
    }
    errors
}

/// The OAuth2 provider preset the form's provider has, if any.
fn oauth_preset(form: &AccountFormValues) -> Option<OAuth2Preset> {
    match (form.provider, form.protocol) {
        (Some(Provider::Google), ProtocolKind::Imap) => Some(OAuth2Preset::Gmail),
        (Some(Provider::Microsoft365), ProtocolKind::Imap) => Some(OAuth2Preset::Microsoft365),
        _ => None,
    }
}

/// Checks a host name typed into the manual form: no scheme, path, port or
/// spaces.
fn validate_host(host: &str) -> Result<()> {
    let host = host.trim();
    if host.contains("://") {
        bail!("Enter just the server name, without imaps:// or https://.");
    }
    if host.contains(['/', ':', ' ', '@']) {
        bail!("Enter just the server name, such as mail.example.com.");
    }
    Ok(())
}

/// Parses a port field.
fn parse_port(port: &str) -> Result<u16> {
    port.trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .context("the port must be a number from 1 to 65535")
}

/// Validates a JMAP session URL: http or https with a host.
fn validate_jmap_url(url: &str) -> Result<()> {
    let parsed = Url::parse(url.trim()).with_context(|| format!("{} is not a URL.", url.trim()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("The JMAP URL must start with https://.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Building the account
// ---------------------------------------------------------------------------

/// Loopback port the UI OAuth2 flow listens on for the provider redirect,
/// matching the CLI default.
const OAUTH2_REDIRECT_PORT: u16 = 8357;

/// Credentials the form collected for a new account.
pub(crate) enum AccountSetupSecret {
    /// Password or app password, stored directly.
    Password(String),
    /// The OAuth2 client whose browser flow runs after the account is stored.
    OAuth2(zmail_auth::OAuth2Client),
}

/// Builds a [`NewAccount`] plus the form's credential plan.
pub(crate) fn build_account_from_form(
    form: &AccountFormValues,
) -> Result<(NewAccount, AccountSetupSecret)> {
    if let Some((_, error)) = validate_account_form(form, Check::All).into_iter().next() {
        bail!(error);
    }
    let email = EmailAddress::parse(&form.email)?;
    let address = email.to_string();
    let secret = if form.use_oauth {
        let client_id = form.oauth_client_id.trim().to_owned();
        let client_secret = trimmed_or_none(&form.oauth_client_secret);
        let client = match oauth_preset(form) {
            Some(preset) => preset.client(client_id, client_secret, OAUTH2_REDIRECT_PORT),
            None => custom_oauth2_client(form, client_id, client_secret),
        };
        AccountSetupSecret::OAuth2(client)
    } else {
        AccountSetupSecret::Password(form.password.clone())
    };
    let identity_name = match form.name.trim() {
        "" => email.local_part().to_owned(),
        name => name.to_owned(),
    };

    let (endpoint, username, outgoing) = match form.protocol {
        ProtocolKind::Proton => bail!("Proton Mail accounts are added by signing in to Proton"),
        ProtocolKind::Jmap => (
            form.jmap_url.trim().to_owned(),
            address.clone(),
            OutgoingConfig::jmap_default(),
        ),
        ProtocolKind::Imap => {
            let host = form.imap_host.trim();
            let scheme = match form.imap_security {
                Security::Tls => "imaps",
                Security::None => "imap",
                Security::StartTls => bail!("zmail can't use STARTTLS for IMAP yet"),
            };
            let endpoint = format!("{scheme}://{host}:{}", parse_port(&form.imap_port)?);
            let username = or_default(&form.imap_username, &address);
            let mut outgoing = OutgoingConfig::smtp_default(form.smtp_host.trim());
            outgoing.port = Some(parse_port(&form.smtp_port)?);
            outgoing.security = Some(submission_security(form.smtp_security));
            outgoing.username = Some(or_default(&form.smtp_username, &username));
            (endpoint, username, outgoing)
        }
    };

    Ok((
        NewAccount {
            // The address names the account until it is renamed in
            // Settings → Accounts.
            display_name: address.clone(),
            protocol: form.protocol,
            auth: if form.use_oauth {
                AuthKind::OAuth2
            } else {
                AuthKind::Password
            },
            endpoint,
            username,
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(outgoing),
            identities: vec![NewIdentity {
                display_name: identity_name,
                email_address: address,
                signature: String::new(),
                is_primary: true,
            }],
        },
        secret,
    ))
}

/// The provider-agnostic OAuth2 client, from the Advanced fields.
fn custom_oauth2_client(
    form: &AccountFormValues,
    client_id: String,
    client_secret: Option<String>,
) -> zmail_auth::OAuth2Client {
    zmail_auth::OAuth2Client {
        client_id,
        client_secret,
        authorization_endpoint: form.oauth_authorization_endpoint.trim().to_owned(),
        token_endpoint: form.oauth_token_endpoint.trim().to_owned(),
        scopes: form
            .oauth_scopes
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        redirect_port: OAUTH2_REDIRECT_PORT,
        extra_authorize_params: Vec::new(),
    }
}

/// The store's name for a submission security choice.
fn submission_security(security: Security) -> SubmissionSecurity {
    match security {
        Security::Tls => SubmissionSecurity::ImplicitTls,
        Security::StartTls => SubmissionSecurity::StartTls,
        Security::None => SubmissionSecurity::Plain,
    }
}

/// The trimmed text, or `fallback` when it is empty.
fn or_default(value: &str, fallback: &str) -> String {
    match value.trim() {
        "" => fallback.to_owned(),
        value => value.to_owned(),
    }
}

/// Returns the trimmed text, or `None` when it is empty or whitespace-only.
fn trimmed_or_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use zmail_autodiscover::{Provider, Security};
    use zmail_core::{AuthKind, ProtocolKind, SubmissionSecurity};

    use super::{
        AccountField, AccountFormValues, AccountSetupSecret, Check, build_account_from_form,
        oauth_secret_label, oauth_secret_placeholder, oauth_secret_required, validate_account_form,
    };

    fn fields(errors: &[(AccountField, String)]) -> Vec<AccountField> {
        errors.iter().map(|(field, _)| *field).collect()
    }

    fn imap_form() -> AccountFormValues {
        AccountFormValues {
            name: "Jane Doe".to_owned(),
            email: "jane@example.com".to_owned(),
            password: "app-password".to_owned(),
            imap_host: "imap.example.com".to_owned(),
            smtp_host: "smtp.example.com".to_owned(),
            ..AccountFormValues::default()
        }
    }

    #[test]
    fn the_sign_in_step_checks_only_its_own_fields() {
        let form = AccountFormValues::default();
        assert_eq!(
            fields(&validate_account_form(&form, Check::Credentials)),
            [AccountField::Email, AccountField::Password]
        );
        let form = AccountFormValues {
            email: "not-an-address".to_owned(),
            password: "secret".to_owned(),
            ..AccountFormValues::default()
        };
        assert_eq!(
            fields(&validate_account_form(&form, Check::Credentials)),
            [AccountField::Email]
        );
    }

    #[test]
    fn server_fields_take_names_and_ports_not_urls() {
        let form = AccountFormValues {
            imap_host: "imaps://imap.example.com".to_owned(),
            imap_port: "99999".to_owned(),
            smtp_port: "smtp".to_owned(),
            ..imap_form()
        };
        assert_eq!(
            fields(&validate_account_form(&form, Check::All)),
            [
                AccountField::ImapHost,
                AccountField::ImapPort,
                AccountField::SmtpPort
            ]
        );
        assert!(validate_account_form(&imap_form(), Check::All).is_empty());
    }

    #[test]
    fn cleartext_needs_an_explicit_opt_in() {
        let mut form = AccountFormValues {
            smtp_security: Security::None,
            ..imap_form()
        };
        assert_eq!(
            fields(&validate_account_form(&form, Check::All)),
            [AccountField::AllowCleartext]
        );
        form.allow_cleartext = true;
        assert!(validate_account_form(&form, Check::All).is_empty());

        let form = AccountFormValues {
            protocol: ProtocolKind::Jmap,
            jmap_url: "http://mail.example.com/.well-known/jmap".to_owned(),
            ..imap_form()
        };
        assert_eq!(
            fields(&validate_account_form(&form, Check::All)),
            [AccountField::AllowCleartext]
        );
    }

    #[test]
    fn imap_starttls_is_refused() {
        let form = AccountFormValues {
            imap_security: Security::StartTls,
            ..imap_form()
        };
        assert_eq!(
            fields(&validate_account_form(&form, Check::All)),
            [AccountField::ImapSecurity]
        );
        assert!(build_account_from_form(&form).is_err());
    }

    #[test]
    fn builds_an_imap_account_with_tls_defaults() {
        let (account, secret) = build_account_from_form(&imap_form()).expect("valid form");
        assert_eq!(account.endpoint, "imaps://imap.example.com:993");
        assert_eq!(account.username, "jane@example.com");
        assert_eq!(account.display_name, "jane@example.com");
        assert_eq!(account.auth, AuthKind::Password);
        let outgoing = account.outgoing.expect("SMTP settings");
        assert_eq!(outgoing.host.as_deref(), Some("smtp.example.com"));
        assert_eq!(outgoing.port, Some(465));
        assert_eq!(outgoing.security, Some(SubmissionSecurity::ImplicitTls));
        assert_eq!(outgoing.username.as_deref(), Some("jane@example.com"));
        assert_eq!(account.identities[0].display_name, "Jane Doe");
        assert!(
            matches!(secret, AccountSetupSecret::Password(password) if password == "app-password")
        );

        // A cleartext IMAP server, confirmed, uses imap://.
        let form = AccountFormValues {
            imap_security: Security::None,
            imap_port: "143".to_owned(),
            allow_cleartext: true,
            name: String::new(),
            ..imap_form()
        };
        let (account, _) = build_account_from_form(&form).expect("valid form");
        assert_eq!(account.endpoint, "imap://imap.example.com:143");
        assert_eq!(
            account.identities[0].display_name, "jane",
            "the local part names you"
        );
    }

    #[test]
    fn oauth_uses_the_provider_preset_or_the_advanced_endpoints() {
        let form = AccountFormValues {
            provider: Some(Provider::Google),
            use_oauth: true,
            password: String::new(),
            oauth_client_id: "client-1".to_owned(),
            oauth_client_secret: "secret-1".to_owned(),
            ..imap_form()
        };
        let (account, secret) = build_account_from_form(&form).expect("valid form");
        assert_eq!(account.auth, AuthKind::OAuth2);
        let AccountSetupSecret::OAuth2(client) = secret else {
            panic!("expected an OAuth2 client");
        };
        assert_eq!(client.token_endpoint, "https://oauth2.googleapis.com/token");
        assert_eq!(client.client_secret.as_deref(), Some("secret-1"));

        // Without a preset the endpoints are required.
        let form = AccountFormValues {
            provider: None,
            oauth_client_secret: String::new(),
            protocol: ProtocolKind::Jmap,
            jmap_url: "https://api.example.com/jmap/session".to_owned(),
            ..form
        };
        assert_eq!(
            fields(&validate_account_form(&form, Check::Credentials)),
            [
                AccountField::OauthAuthorizationEndpoint,
                AccountField::OauthTokenEndpoint
            ]
        );
        let form = AccountFormValues {
            oauth_authorization_endpoint: "https://provider.test/authorize".to_owned(),
            oauth_token_endpoint: "https://provider.test/token".to_owned(),
            oauth_scopes: "email profile".to_owned(),
            ..form
        };
        let (account, secret) = build_account_from_form(&form).expect("valid form");
        assert_eq!(account.endpoint, "https://api.example.com/jmap/session");
        let AccountSetupSecret::OAuth2(client) = secret else {
            panic!("expected an OAuth2 client");
        };
        assert_eq!(client.scopes, ["email", "profile"]);
        assert!(client.client_secret.is_none());
    }

    #[test]
    fn the_client_secret_is_required_for_google_only() {
        let oauth = |provider| AccountFormValues {
            provider,
            use_oauth: true,
            password: String::new(),
            oauth_client_id: "client-1".to_owned(),
            oauth_authorization_endpoint: "https://provider.test/authorize".to_owned(),
            oauth_token_endpoint: "https://provider.test/token".to_owned(),
            ..imap_form()
        };

        // Google's desktop clients need the secret at the token exchange.
        let google = oauth(Some(Provider::Google));
        assert_eq!(
            fields(&validate_account_form(&google, Check::Credentials)),
            [AccountField::OauthClientSecret]
        );
        assert!(build_account_from_form(&google).is_err());
        let blank = AccountFormValues {
            oauth_client_secret: "   ".to_owned(),
            ..google.clone()
        };
        assert_eq!(
            fields(&validate_account_form(&blank, Check::Credentials)),
            [AccountField::OauthClientSecret],
            "whitespace is not a secret"
        );
        assert!(oauth_secret_required(Some(Provider::Google)));
        assert_eq!(
            oauth_secret_label(Some(Provider::Google)),
            "OAuth client secret"
        );
        assert!(oauth_secret_placeholder(Some(Provider::Google)).starts_with("Required"));

        // Microsoft's public clients, and other providers, have none.
        for provider in [Some(Provider::Microsoft365), None] {
            let form = oauth(provider);
            assert!(
                validate_account_form(&form, Check::Credentials).is_empty(),
                "{provider:?} needs no secret"
            );
            let (_, secret) = build_account_from_form(&form).expect("valid form");
            let AccountSetupSecret::OAuth2(client) = secret else {
                panic!("expected an OAuth2 client");
            };
            assert!(client.client_secret.is_none());
            assert!(!oauth_secret_required(provider));
            assert_eq!(
                oauth_secret_label(provider),
                "OAuth client secret (optional)"
            );
            assert!(!oauth_secret_placeholder(provider).contains("Required"));
        }

        // A password sign-in never asks for it.
        let password = AccountFormValues {
            provider: Some(Provider::Google),
            ..imap_form()
        };
        assert!(validate_account_form(&password, Check::All).is_empty());
    }
}
