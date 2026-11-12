//! `add-imap` and `add-jmap`: an account set up at prompts.

use std::{net::IpAddr, sync::Arc};

use anyhow::{Context, Result, bail};
use zmail_core::{
    AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutgoingConfig, ProtocolKind,
    SubmissionSecurity,
};
use zmail_security::{KeyringSecretStore, SecretSlot, SecretStore};
use zmail_setup::check::CheckPart;
use zmail_store::LocalStore;
use zmail_sync::BackgroundRuntime;

use crate::output::line;
use crate::prompts::{
    prompt, prompt_optional, prompt_secret, prompt_secret_optional, prompt_with_default,
    prompt_yes_no,
};

pub(crate) fn add_account(
    protocol: ProtocolKind,
    allow_cleartext: bool,
    force: bool,
) -> Result<()> {
    let store = Arc::new(crate::open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    // Every answer is checked as it is given, and the connection before
    // anything is stored: a mistake leaves nothing behind.
    let NewAccountAnswers {
        account,
        secret,
        budget_mb,
    } = prompt_account(protocol, allow_cleartext)?;
    let runtime = BackgroundRuntime::start(4)?;
    let account_id = store_checked_account(
        &runtime,
        &store,
        secrets.as_ref(),
        &account,
        &secret,
        budget_mb,
        &|label| println!("{}...", line(label)),
    )?;

    let sync_service = crate::command_sync_service(&runtime, store.clone(), secrets, force)?;
    if !sync_service.runs_sync() {
        println!(
            "Account {account_id} added: {} ({}). {}, and starts syncing it within seconds.",
            line(&account.display_name),
            account.protocol,
            crate::SYNCED_ELSEWHERE
        );
        return Ok(());
    }
    match sync_service.sync_account_and_wait(account_id) {
        Ok(()) => {
            println!(
                "Account {account_id} added and synced: {} ({})",
                line(&account.display_name),
                account.protocol
            );
            Ok(())
        }
        Err(error) => {
            eprintln!(
                "Account {account_id} was added, but its first sync failed: {}",
                line(&format!("{error:#}"))
            );
            Err(error)
        }
    }
}

/// Tests the account's sign-in on both halves (`zmail_setup::check`, as
/// the terminal UI and the window do), then stores it with its secret
/// and disk budget. Nothing is stored when the test fails, and an account
/// whose secret can't be kept is removed again. `progress` hears each
/// step. Returns the new account's id.
fn store_checked_account(
    runtime: &BackgroundRuntime,
    store: &LocalStore,
    secrets: &dyn SecretStore,
    account: &NewAccount,
    secret: &AccountSecret,
    budget_mb: Option<u64>,
    progress: &dyn Fn(&str),
) -> Result<i64> {
    for part in [CheckPart::Incoming, CheckPart::Outgoing] {
        progress(&zmail_setup::check::check_label(account, part));
        runtime
            .handle()
            .block_on(zmail_setup::check::check_part(
                account,
                secret.sign_in_secret(),
                part,
            ))
            .map_err(|failure| {
                anyhow::anyhow!("{}. Nothing was added.", line(&failure.summary()))
            })?;
    }
    let account_id = store.insert_account(account)?;
    let kept = secret
        .store(store, secrets, account_id)
        .and_then(|()| match budget_mb {
            Some(budget_mb) => store.set_account_disk_budget(account_id, Some(budget_mb)),
            None => Ok(()),
        });
    if let Err(error) = kept {
        let _ = zmail_setup::accounts::remove_account(store, secrets, None, account_id);
        return Err(error.context("the account was not added"));
    }
    Ok(account_id)
}

/// Everything `add-imap` and `add-jmap` ask, checked.
struct NewAccountAnswers {
    account: NewAccount,
    secret: AccountSecret,
    budget_mb: Option<u64>,
}

/// Credentials collected while setting up a new account.
enum AccountSecret {
    /// Password or app password.
    Password(String),
    /// An OAuth2 access token pasted by the user (no refresh grant).
    PastedAccessToken(String),
    /// Tokens granted by the browser flow, with the client that ran it.
    OAuth2Flow {
        client: zmail_auth::OAuth2Client,
        tokens: zmail_auth::TokenSet,
    },
}

impl AccountSecret {
    /// What the connection test signs in with: the password, or the
    /// access token.
    fn sign_in_secret(&self) -> &str {
        match self {
            Self::Password(secret) | Self::PastedAccessToken(secret) => secret,
            Self::OAuth2Flow { tokens, .. } => &tokens.access_token,
        }
    }

    /// Persists the credentials for a freshly inserted account.
    fn store(&self, store: &LocalStore, secrets: &dyn SecretStore, account_id: i64) -> Result<()> {
        match self {
            Self::Password(secret) => secrets
                .set_account_secret(account_id, secret)
                .with_context(|| format!("failed to store credentials for account {account_id}")),
            Self::PastedAccessToken(token) => secrets
                .set_account_secret_in_slot(account_id, SecretSlot::AccessToken, token)
                .with_context(|| {
                    format!("failed to store the access token for account {account_id}")
                }),
            Self::OAuth2Flow { client, tokens } => {
                // The full grant is persisted so background sync can refresh
                // the access token without user interaction.
                zmail_sync::persist_oauth2_grant(store, secrets, account_id, client, tokens)
            }
        }
    }
}

fn prompt_account(protocol: ProtocolKind, allow_cleartext: bool) -> Result<NewAccountAnswers> {
    if protocol == ProtocolKind::Proton {
        // Proton accounts sign in to Proton step by step, which the
        // window and the terminal UI do (docs/adr/0004-proton-native.md).
        bail!(
            "add Proton Mail accounts in the Zmail window, or in `zmail-app tui` (Setup, then \
             Accounts)"
        );
    }
    println!("Setting up a new {protocol} account.");
    let display_name = prompt("Display name")?;
    let endpoint = match protocol {
        ProtocolKind::Imap => prompt_with_default("IMAP endpoint", "imaps://mail.example.com:993")?,
        ProtocolKind::Jmap => prompt_with_default("JMAP base URL", "https://mail.example.com")?,
        ProtocolKind::Proton => unreachable!("Proton accounts are refused above"),
    };
    validate_endpoint(protocol, &endpoint, allow_cleartext)?;
    let username = prompt("Username or account email")?;
    let auth = match protocol {
        ProtocolKind::Imap => {
            if prompt_yes_no("Authenticate with an OAuth2 access token (XOAUTH2)?", false)? {
                AuthKind::OAuth2
            } else {
                AuthKind::Password
            }
        }
        ProtocolKind::Jmap => {
            if prompt_yes_no("Authenticate with an OAuth2 bearer token?", false)? {
                AuthKind::OAuth2
            } else {
                AuthKind::Password
            }
        }
        ProtocolKind::Proton => unreachable!("Proton accounts are refused above"),
    };
    let secret = match auth {
        AuthKind::Password => AccountSecret::Password(
            rpassword::prompt_password("Password or app password: ")
                .context("failed to read the account password")?,
        ),
        AuthKind::OAuth2 => {
            if prompt_yes_no("Run the browser authorization flow (PKCE)?", true)? {
                let client = prompt_oauth2_client(protocol)?;
                let tokens = client.authorize_interactively()?;
                println!(
                    "Received an access token{}.",
                    if tokens.refresh_token.is_some() {
                        " and a refresh token"
                    } else {
                        ""
                    }
                );
                AccountSecret::OAuth2Flow { client, tokens }
            } else {
                AccountSecret::PastedAccessToken(
                    rpassword::prompt_password("OAuth2 access token: ")
                        .context("failed to read the OAuth2 access token")?,
                )
            }
        }
        AuthKind::ProtonSession => unreachable!("Proton accounts are refused above"),
    };
    let identity_name = prompt_with_default("Primary identity name", &display_name)?;
    let identity_email = prompt_with_default("Primary identity email", &username)?;
    check_address(&identity_email)?;
    let signature =
        prompt_with_default("Primary signature", &format!("Regards,\n{identity_name}"))?;
    let alias_input = prompt_optional("Additional identity emails (comma-separated, optional)")?;
    let outgoing = match protocol {
        ProtocolKind::Imap => prompt_smtp_outgoing(&endpoint, &username, allow_cleartext)?,
        ProtocolKind::Jmap => OutgoingConfig::jmap_default(),
        ProtocolKind::Proton => unreachable!("Proton accounts are refused above"),
    };

    let mut identities = vec![NewIdentity {
        display_name: identity_name.clone(),
        email_address: identity_email.clone(),
        signature,
        is_primary: true,
    }];

    if let Some(alias_input) = alias_input {
        for (index, alias) in alias_input
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .enumerate()
        {
            check_address(alias)?;
            identities.push(NewIdentity {
                display_name: format!("{identity_name} Alias {}", index + 1),
                email_address: alias.to_owned(),
                signature: String::new(),
                is_primary: false,
            });
        }
    }

    let budget_mb = prompt_optional("Disk budget in MiB (empty for unlimited)")?
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                anyhow::anyhow!("the disk budget must be a number of MiB, not {value:?}")
            })
        })
        .transpose()?;

    Ok(NewAccountAnswers {
        account: NewAccount {
            display_name,
            protocol,
            auth,
            endpoint,
            username,
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(outgoing),
            identities,
        },
        secret,
        budget_mb,
    })
}

/// Refuses an identity address Zmail could not send from, by the rule the
/// SMTP and JMAP envelopes are checked with
/// ([`zmail_core::participants::is_valid_address`]).
fn check_address(address: &str) -> Result<()> {
    if !zmail_core::participants::is_valid_address(address) {
        bail!("{} is not an email address", line(address));
    }
    Ok(())
}

/// Prompts for the OAuth2 client configuration used by the browser flow,
/// with Gmail and Microsoft 365 presets pre-filling the endpoints. Client
/// secrets are read without echo.
fn prompt_oauth2_client(protocol: ProtocolKind) -> Result<zmail_auth::OAuth2Client> {
    // No consumer provider presets offer JMAP; JMAP accounts always use
    // custom provider endpoints.
    if protocol == ProtocolKind::Jmap {
        return prompt_custom_oauth2_client();
    }

    let preset = prompt_with_default(
        "Provider (1 = Gmail, 2 = Microsoft 365, 3 = custom endpoints)",
        "3",
    )?;
    let redirect_port: u16 = prompt_with_default("Local redirect port", "8357")?
        .parse()
        .context("the redirect port must be a number")?;

    match preset.trim() {
        "1" => {
            let client_id = prompt("Google OAuth2 client id")?;
            // Google's desktop clients need their secret at the token
            // exchange, so it is required here.
            let client_secret = prompt_secret("Google OAuth2 client secret")?;
            Ok(zmail_auth::OAuth2Preset::Gmail.client(
                client_id,
                Some(client_secret),
                redirect_port,
            ))
        }
        "2" => {
            let client_id = prompt("Microsoft application (client) id")?;
            let client_secret =
                prompt_secret_optional("Client secret (public clients leave this empty)")?;
            Ok(zmail_auth::OAuth2Preset::Microsoft365.client(
                client_id,
                client_secret,
                redirect_port,
            ))
        }
        _ => {
            let client_id = prompt("OAuth2 client id")?;
            let client_secret =
                prompt_secret_optional("Client secret (public clients leave this empty)")?;
            let client = prompt_custom_oauth2_client()?;
            Ok(zmail_auth::OAuth2Client {
                client_id,
                client_secret,
                ..client
            })
        }
    }
}

/// Prompts for a provider-agnostic OAuth2 client configuration, used by JMAP
/// accounts (no consumer presets offer JMAP) and the custom-endpoint choice.
fn prompt_custom_oauth2_client() -> Result<zmail_auth::OAuth2Client> {
    let redirect_port: u16 = prompt_with_default("Local redirect port", "8357")?
        .parse()
        .context("the redirect port must be a number")?;
    let client_id = prompt("OAuth2 client id")?;
    let client_secret = prompt_secret_optional("Client secret (public clients leave this empty)")?;
    let authorization_endpoint = prompt("Authorization endpoint URL")?;
    let token_endpoint = prompt("Token endpoint URL")?;
    let scopes = prompt_with_default("Scopes (space-separated)", "email")?
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(zmail_auth::OAuth2Client {
        client_id,
        client_secret,
        authorization_endpoint,
        token_endpoint,
        scopes,
        redirect_port,
        extra_authorize_params: Vec::new(),
    })
}

/// Checks an account's endpoint: the protocol's schemes, and no
/// cleartext (`imap://`, `http://`) to another machine unless
/// `allow_cleartext`.
pub(crate) fn validate_endpoint(
    protocol: ProtocolKind,
    endpoint: &str,
    allow_cleartext: bool,
) -> Result<()> {
    let parsed = url::Url::parse(endpoint)
        .with_context(|| format!("failed to parse endpoint {endpoint}"))?;
    let cleartext = match protocol {
        ProtocolKind::Imap => match parsed.scheme() {
            "imaps" => false,
            "imap" => true,
            _ => bail!("IMAP endpoints must use imap:// or imaps://"),
        },
        ProtocolKind::Jmap => match parsed.scheme() {
            "https" => false,
            "http" => true,
            _ => bail!("JMAP endpoints must use http:// or https://"),
        },
        ProtocolKind::Proton => {
            if parsed.scheme() != "https" {
                bail!("the Proton API is only reached over https://");
            }
            false
        }
    };
    if cleartext {
        refuse_cleartext(
            &format!("{endpoint} has no TLS"),
            parsed.host_str().unwrap_or_default(),
            allow_cleartext,
        )?;
    }
    Ok(())
}

/// Refuses a connection without TLS to `host`, unless `host` is this
/// machine or the user passed `--allow-cleartext`. `what` says which
/// connection.
fn refuse_cleartext(what: &str, host: &str, allow_cleartext: bool) -> Result<()> {
    if is_loopback_host(host) {
        return Ok(());
    }
    if allow_cleartext {
        eprintln!(
            "warning: {what}, so the password and mail cross the network unencrypted \
             (--allow-cleartext)."
        );
        return Ok(());
    }
    bail!(
        "{what}, so the password and mail would cross the network unencrypted. Use TLS, or \
         pass --allow-cleartext if that is really what you want"
    )
}

/// Whether `host` is this machine: `localhost` or a loopback address.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn prompt_smtp_outgoing(
    endpoint: &str,
    username: &str,
    allow_cleartext: bool,
) -> Result<OutgoingConfig> {
    let mut outgoing = OutgoingConfig::smtp_default(derive_host(endpoint));
    let host = prompt_with_default("SMTP host", outgoing.host.as_deref().unwrap_or("localhost"))?;
    outgoing.port = Some(
        prompt_with_default("SMTP port", &outgoing.port.unwrap_or(587).to_string())?
            .parse::<u16>()
            .context("SMTP port must be an integer between 0 and 65535")?,
    );
    let security = prompt_with_default("SMTP security [starttls|implicit_tls|plain]", "starttls")?;
    let security = parse_submission_security(&security)?;
    if security == SubmissionSecurity::Plain {
        refuse_cleartext(
            &format!("SMTP security `plain` to {host} has no TLS"),
            &host,
            allow_cleartext,
        )?;
    }
    outgoing.host = Some(host);
    outgoing.security = Some(security);
    outgoing.username = Some(prompt_with_default("SMTP username", username)?);
    outgoing.hello_name = prompt_with_default("SMTP EHLO hostname", "localhost")?;
    Ok(outgoing)
}

fn parse_submission_security(value: &str) -> Result<SubmissionSecurity> {
    SubmissionSecurity::parse(&value.trim().to_ascii_lowercase())
        .with_context(|| format!("unknown submission security mode {value}"))
}

pub(crate) fn derive_host(endpoint: &str) -> String {
    url::Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "localhost".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleartext_endpoints_need_loopback_or_an_explicit_opt_in() {
        for endpoint in [
            "imap://127.0.0.1:3143",
            "imap://localhost:143",
            "imap://[::1]:143",
            "imaps://mail.example.com:993",
        ] {
            validate_endpoint(ProtocolKind::Imap, endpoint, false)
                .unwrap_or_else(|error| panic!("{endpoint}: {error:#}"));
        }
        for endpoint in ["http://127.0.0.1:4080", "https://mail.example.com"] {
            validate_endpoint(ProtocolKind::Jmap, endpoint, false)
                .unwrap_or_else(|error| panic!("{endpoint}: {error:#}"));
        }
        for (protocol, endpoint) in [
            (ProtocolKind::Imap, "imap://mail.example.com:143"),
            (ProtocolKind::Imap, "imap://192.168.1.10:143"),
            (ProtocolKind::Jmap, "http://mail.example.com"),
            // Not loopback, whatever the name looks like.
            (ProtocolKind::Jmap, "http://localhost.example.com"),
        ] {
            let error = validate_endpoint(protocol, endpoint, false)
                .err()
                .unwrap_or_else(|| panic!("{endpoint} is refused"));
            assert!(
                format!("{error:#}").contains("--allow-cleartext"),
                "{error:#}"
            );
            validate_endpoint(protocol, endpoint, true)
                .unwrap_or_else(|error| panic!("{endpoint} with the opt-in: {error:#}"));
        }
        assert!(validate_endpoint(ProtocolKind::Imap, "https://mail.example.com", true).is_err());
    }

    #[test]
    fn nothing_is_stored_when_the_connection_test_fails() {
        // A port nothing listens on: the incoming sign-in fails at once.
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .expect("a port")
            .local_addr()
            .expect("its address")
            .port();
        let account = NewAccount {
            display_name: "Work".to_owned(),
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            endpoint: format!("imap://127.0.0.1:{port}"),
            username: "user@example.test".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(OutgoingConfig::smtp_default("127.0.0.1".to_owned())),
            identities: vec![NewIdentity {
                display_name: "Work".to_owned(),
                email_address: "user@example.test".to_owned(),
                signature: String::new(),
                is_primary: true,
            }],
        };
        let store = LocalStore::open_ephemeral().expect("the store opens");
        let secrets = zmail_security::InMemorySecretStore::shared();
        let runtime = BackgroundRuntime::start(1).expect("a runtime");
        let steps = std::sync::Mutex::new(Vec::new());
        let error = store_checked_account(
            &runtime,
            &store,
            secrets.as_ref(),
            &account,
            &AccountSecret::Password("password".to_owned()),
            Some(100),
            &|label| steps.lock().expect("the steps").push(label.to_owned()),
        )
        .expect_err("the test fails");
        let message = format!("{error:#}");
        assert!(
            message.contains("couldn't connect to the incoming server"),
            "{message}"
        );
        assert!(message.contains("Nothing was added"), "{message}");
        assert!(store.list_accounts().expect("accounts").is_empty());
        assert_eq!(
            steps.into_inner().expect("the steps"),
            vec![format!("Signing in to 127.0.0.1:{port}")]
        );
    }

    #[test]
    fn identity_addresses_are_checked() {
        for address in ["ada@example.com", "ops@localhost"] {
            check_address(address).unwrap_or_else(|error| panic!("{address}: {error:#}"));
        }
        for address in [
            "ada",
            "@example.com",
            "ada@",
            "a da@example.com",
            "a@b>\r\n",
            "ada@example..com",
        ] {
            assert!(check_address(address).is_err(), "{address}");
        }
    }

    #[test]
    fn plain_smtp_needs_loopback_or_an_explicit_opt_in() {
        assert!(refuse_cleartext("SMTP", "smtp.example.com", false).is_err());
        assert!(refuse_cleartext("SMTP", "smtp.example.com", true).is_ok());
        assert!(refuse_cleartext("SMTP", "127.0.0.1", false).is_ok());
    }
}
