//! The add-account form's fields, their validation, and building the
//! account from them.
//!
//! Every way through the flow ends in the same fields: a provider preset
//! or an autodiscovery result fills the server fields, and the manual form
//! shows them for editing. [`build_account_from_form`] then turns one
//! [`AccountFormValues`] snapshot into the stored account, whichever way
//! the values got there. Each frontend keeps the fields in its own
//! controls and hands a snapshot here.

use anyhow::{Context as _, Result, bail};
use url::Url;
use zmail_auth::OAuth2Preset;
use zmail_autodiscover::{EmailAddress, MailSettings, Provider, Security, ServerSettings};
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutgoingConfig, ProtocolKind,
    SubmissionSecurity,
};

use crate::google;

// ---------------------------------------------------------------------------
// AccountField
// ---------------------------------------------------------------------------

/// One field of the add-account flow, for validation errors and focus.
/// The text fields each have a text control in the frontend; the security
/// choices and the cleartext opt-in are controls with errors of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccountField {
    /// The name mail is sent under.
    Name,
    /// The email address.
    Email,
    /// The password or app password.
    Password,
    /// The OAuth client ID.
    OauthClientId,
    /// The OAuth client secret.
    OauthClientSecret,
    /// A custom OAuth service's authorization endpoint.
    OauthAuthorizationEndpoint,
    /// A custom OAuth service's token endpoint.
    OauthTokenEndpoint,
    /// A custom OAuth service's scopes.
    OauthScopes,
    /// The JMAP session URL.
    JmapUrl,
    /// The IMAP server's host name.
    ImapHost,
    /// The IMAP server's port.
    ImapPort,
    /// The IMAP server's security choice.
    ImapSecurity,
    /// The IMAP user name.
    ImapUsername,
    /// The SMTP server's host name.
    SmtpHost,
    /// The SMTP server's port.
    SmtpPort,
    /// The SMTP server's security choice.
    SmtpSecurity,
    /// The SMTP user name.
    SmtpUsername,
    /// The opt-in to an unencrypted connection.
    AllowCleartext,
    /// Proton sign-in: the two-factor code.
    ProtonTotp,
    /// Proton sign-in: the mailbox password (two-password accounts).
    ProtonMailboxPassword,
    /// The address the primary identity sends from (the terminal UI's
    /// identity step; not in [`AccountField::TEXT`], which the GUI shows).
    IdentityEmail,
    /// The primary identity's signature (the terminal UI's identity step).
    Signature,
}

impl AccountField {
    /// The text fields, in form order.
    pub const TEXT: [AccountField; 17] = [
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
        AccountField::ProtonTotp,
        AccountField::ProtonMailboxPassword,
    ];

    /// Whether the field holds a secret: never shown, never logged.
    pub fn is_secret(self) -> bool {
        matches!(
            self,
            AccountField::Password
                | AccountField::OauthClientSecret
                | AccountField::ProtonMailboxPassword
        )
    }

    /// The visible label, also the field's accessible name.
    pub fn label(self) -> &'static str {
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
            AccountField::ProtonTotp => "Two-factor code",
            AccountField::ProtonMailboxPassword => "Mailbox password",
            AccountField::IdentityEmail => "Send as",
            AccountField::Signature => "Signature",
        }
    }

    /// Placeholder text.
    pub fn placeholder(self) -> &'static str {
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
            AccountField::ProtonTotp => "6-digit code from your authenticator app",
            AccountField::ProtonMailboxPassword => "Your Proton mailbox password",
            AccountField::IdentityEmail => "Your email address",
            AccountField::Signature => "Optional",
            AccountField::ImapSecurity
            | AccountField::SmtpSecurity
            | AccountField::AllowCleartext => "",
        }
    }
}

/// Whether the provider's OAuth clients need their secret at the token
/// exchange. Google's desktop clients do, though the secret is not
/// confidential there; Microsoft's public clients have none.
pub fn oauth_secret_required(provider: Option<Provider>) -> bool {
    provider == Some(Provider::Google)
}

/// The client secret field's visible label for the provider.
pub fn oauth_secret_label(provider: Option<Provider>) -> &'static str {
    if oauth_secret_required(provider) {
        "OAuth client secret"
    } else {
        "OAuth client secret (optional)"
    }
}

/// The client secret field's placeholder for the provider.
pub fn oauth_secret_placeholder(provider: Option<Provider>) -> &'static str {
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
pub const IMAP_SECURITY: [Security; 2] = [Security::Tls, Security::None];

/// The security choices the SMTP Select offers.
pub const SMTP_SECURITY: [Security; 3] = [Security::Tls, Security::StartTls, Security::None];

/// The usual IMAP port for a security choice.
pub fn imap_default_port(security: Security) -> u16 {
    match security {
        Security::Tls => 993,
        Security::StartTls | Security::None => 143,
    }
}

/// The usual submission port for a security choice (RFC 8314: 465 for
/// implicit TLS, 587 for STARTTLS).
pub fn smtp_default_port(security: Security) -> u16 {
    match security {
        Security::Tls => 465,
        Security::StartTls | Security::None => 587,
    }
}

/// The label a security Select shows for a choice.
pub fn security_label(security: Security) -> &'static str {
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
///
/// Its `Debug` leaves the password and the client secret out, so a value
/// logged by mistake carries no secret.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountFormValues {
    /// The provider button picked; `None` is "Other".
    pub provider: Option<Provider>,
    /// The name mail is sent under.
    pub name: String,
    /// The email address.
    pub email: String,
    /// The password or app password.
    pub password: String,
    /// Sign in with OAuth2 instead of a password.
    pub use_oauth: bool,
    /// The OAuth client ID.
    pub oauth_client_id: String,
    /// The OAuth client secret.
    pub oauth_client_secret: String,
    /// A custom OAuth service's authorization endpoint.
    pub oauth_authorization_endpoint: String,
    /// A custom OAuth service's token endpoint.
    pub oauth_token_endpoint: String,
    /// A custom OAuth service's scopes, space-separated.
    pub oauth_scopes: String,
    /// IMAP and SMTP, or JMAP.
    pub protocol: ProtocolKind,
    /// The JMAP session URL.
    pub jmap_url: String,
    /// The IMAP server's host name.
    pub imap_host: String,
    /// The IMAP server's port, as typed.
    pub imap_port: String,
    /// The IMAP server's security.
    pub imap_security: Security,
    /// The IMAP user name; empty is the address.
    pub imap_username: String,
    /// The SMTP server's host name.
    pub smtp_host: String,
    /// The SMTP server's port, as typed.
    pub smtp_port: String,
    /// The SMTP server's security.
    pub smtp_security: Security,
    /// The SMTP user name; empty is the IMAP user name.
    pub smtp_username: String,
    /// The user confirmed an unencrypted connection.
    pub allow_cleartext: bool,
    /// The address the primary identity sends from; empty is the account's
    /// address. The GUI leaves it empty.
    pub identity_email: String,
    /// The primary identity's signature. The GUI leaves it empty.
    pub signature: String,
}

impl std::fmt::Debug for AccountFormValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hidden = |value: &str| if value.is_empty() { "" } else { "<hidden>" };
        f.debug_struct("AccountFormValues")
            .field("provider", &self.provider)
            .field("name", &self.name)
            .field("email", &self.email)
            .field("password", &hidden(&self.password))
            .field("use_oauth", &self.use_oauth)
            .field("oauth_client_id", &self.oauth_client_id)
            .field("oauth_client_secret", &hidden(&self.oauth_client_secret))
            .field(
                "oauth_authorization_endpoint",
                &self.oauth_authorization_endpoint,
            )
            .field("oauth_token_endpoint", &self.oauth_token_endpoint)
            .field("oauth_scopes", &self.oauth_scopes)
            .field("protocol", &self.protocol)
            .field("jmap_url", &self.jmap_url)
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("imap_security", &self.imap_security)
            .field("imap_username", &self.imap_username)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_security", &self.smtp_security)
            .field("smtp_username", &self.smtp_username)
            .field("allow_cleartext", &self.allow_cleartext)
            .field("identity_email", &self.identity_email)
            .field("signature", &self.signature)
            .finish()
    }
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
            identity_email: String::new(),
            signature: String::new(),
        }
    }
}

impl AccountFormValues {
    /// The value of one text field (empty for the Proton sign-in fields,
    /// which are read when submitted, and for the controls).
    pub fn value(&self, field: AccountField) -> &str {
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
            AccountField::IdentityEmail => &self.identity_email,
            AccountField::Signature => &self.signature,
            // Proton sign-in fields are read when submitted, not stored.
            AccountField::ProtonTotp | AccountField::ProtonMailboxPassword => "",
            AccountField::ImapSecurity
            | AccountField::SmtpSecurity
            | AccountField::AllowCleartext => "",
        }
    }

    /// Whether any connection these values describe is unencrypted.
    pub fn uses_cleartext(&self) -> bool {
        match self.protocol {
            ProtocolKind::Imap => {
                self.imap_security == Security::None || self.smtp_security == Security::None
            }
            ProtocolKind::Jmap => self.jmap_url.trim().starts_with("http://"),
            // Proton's API is HTTPS only, and its accounts never use this form.
            ProtocolKind::Proton => false,
        }
    }

    /// Replaces one text field's value. The Proton sign-in fields and the
    /// controls have none, so setting them does nothing.
    pub fn set_value(&mut self, field: AccountField, value: &str) {
        let slot = match field {
            AccountField::Name => &mut self.name,
            AccountField::Email => &mut self.email,
            AccountField::Password => &mut self.password,
            AccountField::OauthClientId => &mut self.oauth_client_id,
            AccountField::OauthClientSecret => &mut self.oauth_client_secret,
            AccountField::OauthAuthorizationEndpoint => &mut self.oauth_authorization_endpoint,
            AccountField::OauthTokenEndpoint => &mut self.oauth_token_endpoint,
            AccountField::OauthScopes => &mut self.oauth_scopes,
            AccountField::JmapUrl => &mut self.jmap_url,
            AccountField::ImapHost => &mut self.imap_host,
            AccountField::ImapPort => &mut self.imap_port,
            AccountField::ImapUsername => &mut self.imap_username,
            AccountField::SmtpHost => &mut self.smtp_host,
            AccountField::SmtpPort => &mut self.smtp_port,
            AccountField::SmtpUsername => &mut self.smtp_username,
            AccountField::IdentityEmail => &mut self.identity_email,
            AccountField::Signature => &mut self.signature,
            AccountField::ProtonTotp
            | AccountField::ProtonMailboxPassword
            | AccountField::ImapSecurity
            | AccountField::SmtpSecurity
            | AccountField::AllowCleartext => return,
        };
        value.clone_into(slot);
    }

    /// Fills the server fields from discovered (or preset) settings.
    pub fn apply_settings(&mut self, settings: &MailSettings) {
        match settings {
            MailSettings::Jmap(jmap) => {
                self.protocol = ProtocolKind::Jmap;
                self.jmap_url.clone_from(&jmap.value.session_url);
            }
            MailSettings::ImapSmtp { imap, smtp } => {
                self.protocol = ProtocolKind::Imap;
                self.apply_imap(&imap.value);
                self.apply_smtp(&smtp.value);
            }
        }
    }

    /// Fills the IMAP fields.
    pub fn apply_imap(&mut self, server: &ServerSettings) {
        self.imap_security = server.security;
        self.imap_host.clone_from(&server.host);
        self.imap_port = server.port.to_string();
        self.imap_username.clone_from(&server.username);
    }

    /// Fills the SMTP fields.
    pub fn apply_smtp(&mut self, server: &ServerSettings) {
        self.smtp_security = server.security;
        self.smtp_host.clone_from(&server.host);
        self.smtp_port = server.port.to_string();
        self.smtp_username.clone_from(&server.username);
    }

    /// Fills the manual form with the usual guesses for the address's
    /// domain, when nothing better was found.
    pub fn prefill_guesses(&mut self, email: &EmailAddress) {
        let domain = email.domain();
        if self.imap_host.trim().is_empty() {
            self.imap_host = format!("imap.{domain}");
            self.imap_username = email.to_string();
        }
        if self.smtp_host.trim().is_empty() {
            self.smtp_host = format!("smtp.{domain}");
            self.smtp_username = email.to_string();
        }
    }

    /// Changes the IMAP security, moving the port along when it is still
    /// the previous choice's usual port.
    pub fn set_imap_security(&mut self, security: Security) {
        if self.imap_port.trim() == imap_default_port(self.imap_security).to_string() {
            self.imap_port = imap_default_port(security).to_string();
        }
        self.imap_security = security;
    }

    /// Changes the SMTP security, moving the port along when it is still
    /// the previous choice's usual port.
    pub fn set_smtp_security(&mut self, security: Security) {
        if self.smtp_port.trim() == smtp_default_port(self.smtp_security).to_string() {
            self.smtp_port = smtp_default_port(security).to_string();
        }
        self.smtp_security = security;
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Which part of the flow is being checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The sign-in step: name, address and password (or OAuth2).
    Credentials,
    /// Everything, before the account is stored.
    All,
}

/// Checks the form and returns an error message for each field that needs
/// fixing, so the form can show them beside the fields. It mirrors the
/// checks in [`build_account_from_form`].
pub fn validate_account_form(
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
        if form.provider == Some(Provider::Google) {
            // A pasted client is checked for its shape (a Google client
            // ID, a secret that is not the ID) before the browser opens.
            if !form.oauth_client_id.trim().is_empty()
                && let Some(error) = google::check_client_id(&form.oauth_client_id)
            {
                errors.push((AccountField::OauthClientId, error.to_owned()));
            }
            if let Some(error) = google::check_client_secret(&form.oauth_client_secret) {
                errors.push((AccountField::OauthClientSecret, error.to_owned()));
            }
        }
        if oauth_preset(form).is_none() {
            // The browser signs in at the first and the client secret and
            // tokens go to the second: both must be https (http only on
            // this computer, for a test server).
            for (field, value, missing, cleartext) in [
                (
                    AccountField::OauthAuthorizationEndpoint,
                    &form.oauth_authorization_endpoint,
                    "Enter the authorization endpoint.",
                    "Enter the authorization endpoint as an https:// address.",
                ),
                (
                    AccountField::OauthTokenEndpoint,
                    &form.oauth_token_endpoint,
                    "Enter the token endpoint.",
                    "Enter the token endpoint as an https:// address.",
                ),
            ] {
                if require(&mut errors, field, value, missing)
                    && zmail_auth::check_endpoint(value).is_err()
                {
                    errors.push((field, cleartext.to_owned()));
                }
            }
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
                    "Zmail can't use STARTTLS for IMAP yet. Choose TLS (usually port 993)."
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
pub const OAUTH2_REDIRECT_PORT: u16 = 8357;

/// Credentials the form collected for a new account. Its `Debug` leaves
/// the password and the client secret out.
pub enum AccountSetupSecret {
    /// Password or app password, stored directly.
    Password(String),
    /// The OAuth2 client whose browser flow runs after the account is stored.
    OAuth2(zmail_auth::OAuth2Client),
}

impl std::fmt::Debug for AccountSetupSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password(_) => f.write_str("Password(<hidden>)"),
            Self::OAuth2(client) => f
                .debug_struct("OAuth2")
                .field("client_id", &client.client_id)
                .field("token_endpoint", &client.token_endpoint)
                .finish_non_exhaustive(),
        }
    }
}

/// Builds a [`NewAccount`] plus the form's credential plan.
pub fn build_account_from_form(
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
                Security::StartTls => bail!("Zmail can't use STARTTLS for IMAP yet"),
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
                email_address: or_default(&form.identity_email, &address),
                signature: form.signature.trim_end().to_owned(),
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
            oauth_client_id: "client-1.apps.googleusercontent.com".to_owned(),
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
        // They must be https, except on this computer (a test server).
        let cleartext = AccountFormValues {
            oauth_authorization_endpoint: "http://provider.test/authorize".to_owned(),
            oauth_token_endpoint: "http://provider.test/token".to_owned(),
            ..form.clone()
        };
        let errors = validate_account_form(&cleartext, Check::Credentials);
        assert_eq!(
            fields(&errors),
            [
                AccountField::OauthAuthorizationEndpoint,
                AccountField::OauthTokenEndpoint
            ]
        );
        assert!(errors[1].1.contains("https://"), "{errors:?}");
        assert!(build_account_from_form(&cleartext).is_err());
        let loopback = AccountFormValues {
            oauth_authorization_endpoint: "http://127.0.0.1:8080/authorize".to_owned(),
            oauth_token_endpoint: "http://localhost:8080/token".to_owned(),
            ..form.clone()
        };
        assert!(validate_account_form(&loopback, Check::Credentials).is_empty());
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
            oauth_client_id: "client-1.apps.googleusercontent.com".to_owned(),
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

    #[test]
    fn a_pasted_google_client_is_checked_for_its_shape() {
        let google = |client_id: &str, client_secret: &str| AccountFormValues {
            provider: Some(Provider::Google),
            use_oauth: true,
            password: String::new(),
            oauth_client_id: client_id.to_owned(),
            oauth_client_secret: client_secret.to_owned(),
            ..imap_form()
        };
        let check =
            |form: &AccountFormValues| fields(&validate_account_form(form, Check::Credentials));

        assert!(check(&google("42-abc.apps.googleusercontent.com", "GOCSPX-x")).is_empty());
        // Not a Google client ID.
        assert_eq!(
            check(&google("42-abc", "GOCSPX-x")),
            [AccountField::OauthClientId]
        );
        // The two pasted the wrong way round.
        assert_eq!(
            check(&google("GOCSPX-x", "42-abc.apps.googleusercontent.com")),
            [AccountField::OauthClientId, AccountField::OauthClientSecret]
        );
        // An empty ID is reported as missing, once.
        let errors = validate_account_form(&google("", "GOCSPX-x"), Check::Credentials);
        assert_eq!(fields(&errors), [AccountField::OauthClientId]);
        assert!(errors[0].1.starts_with("Enter the OAuth client ID"));
        // Microsoft's client IDs are GUIDs, and are not checked this way.
        let microsoft = AccountFormValues {
            provider: Some(Provider::Microsoft365),
            ..google("00000000-0000-0000-0000-000000000000", "")
        };
        assert!(check(&microsoft).is_empty());
    }
    #[test]
    fn settings_fill_the_form_and_security_moves_the_usual_port() {
        let email = zmail_autodiscover::EmailAddress::parse("jane@icloud.com").expect("valid");
        let mut form = AccountFormValues::default();
        form.apply_settings(&Provider::ICloud.settings(&email));
        assert_eq!(form.imap_host, "imap.mail.me.com");
        assert_eq!(form.smtp_port, "587");
        assert_eq!(form.smtp_security, Security::StartTls);

        // The port follows the security while it is the usual one.
        form.set_smtp_security(Security::Tls);
        assert_eq!(form.smtp_port, "465");
        form.smtp_port = "2525".to_owned();
        form.set_smtp_security(Security::StartTls);
        assert_eq!(form.smtp_port, "2525", "a port typed by hand stays");
        form.set_imap_security(Security::None);
        assert_eq!(form.imap_port, "143");

        let fastmail = zmail_autodiscover::EmailAddress::parse("jane@fastmail.com").expect("valid");
        form.apply_settings(&Provider::Fastmail.settings(&fastmail));
        assert_eq!(form.protocol, ProtocolKind::Jmap);
        assert_eq!(form.jmap_url, "https://api.fastmail.com/jmap/session");
    }

    #[test]
    fn fields_read_and_write_by_name() {
        let mut form = AccountFormValues::default();
        for field in AccountField::TEXT {
            form.set_value(field, "x");
        }
        assert_eq!(form.value(AccountField::ImapHost), "x");
        assert_eq!(form.value(AccountField::Password), "x");
        assert_eq!(form.value(AccountField::ProtonTotp), "", "never kept");
        assert!(AccountField::Password.is_secret());
        assert!(AccountField::OauthClientSecret.is_secret());
        assert!(!AccountField::OauthClientId.is_secret());
    }

    #[test]
    fn debug_output_leaves_secrets_out() {
        let form = AccountFormValues {
            password: "hunter2-app-password".to_owned(),
            oauth_client_secret: "GOCSPX-very-secret".to_owned(),
            ..imap_form()
        };
        let debug = format!("{form:?}");
        assert!(
            !debug.contains("hunter2") && !debug.contains("very-secret"),
            "{debug}"
        );
        assert!(debug.contains("imap.example.com"));
        let (_, secret) = build_account_from_form(&form).expect("valid form");
        assert!(!format!("{secret:?}").contains("hunter2"));
    }

    #[test]
    fn the_identity_can_be_changed_before_saving() {
        let form = AccountFormValues {
            identity_email: "jane.doe@example.com".to_owned(),
            signature: "Jane\n".to_owned(),
            ..imap_form()
        };
        let (account, _) = build_account_from_form(&form).expect("valid form");
        assert_eq!(account.identities[0].email_address, "jane.doe@example.com");
        assert_eq!(account.identities[0].signature, "Jane");
        assert_eq!(
            account.username, "jane@example.com",
            "the sign-in is unchanged"
        );
    }
}
