//! Testing a new account's connection before it is stored: sign in to the
//! incoming server (IMAP, or the JMAP session), then the outgoing one
//! (SMTP), and say plainly which one failed and why.
//!
//! The test sends no mail and changes nothing on the server: IMAP logs in
//! and out, SMTP signs in and quits, JMAP fetches the session. The secret
//! is the password, or an OAuth2 access token from a finished browser
//! sign-in. Server replies quoted in the failure are untrusted text.

use anyhow::Result;
use zmail_core::{NewAccount, OutgoingConfig, ProtocolKind, SubmissionKind};

use crate::form::AccountField;

/// Which half of the account a test step signs in to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckPart {
    /// IMAP, or JMAP (which sends through the same session).
    Incoming,
    /// SMTP submission.
    Outgoing,
}

/// A failed connection test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckFailure {
    /// The half that failed.
    pub part: CheckPart,
    /// The server it tried, such as `imap.example.com:993`.
    pub server: String,
    /// Why, the whole error chain (untrusted: it quotes the server).
    pub message: String,
    /// Whether the server refused the sign-in itself (the password or
    /// token), rather than the connection failing.
    pub refused: bool,
}

impl CheckFailure {
    /// The field to fix first: the password for a refused sign-in, else
    /// the server's host.
    pub fn field(&self) -> AccountField {
        match (self.refused, self.part) {
            (true, _) => AccountField::Password,
            (false, CheckPart::Incoming) => AccountField::ImapHost,
            (false, CheckPart::Outgoing) => AccountField::SmtpHost,
        }
    }

    /// One line for the user: what failed, where, and why.
    pub fn summary(&self) -> String {
        let what = match (self.part, self.refused) {
            (CheckPart::Incoming, true) => "The incoming server refused the sign-in",
            (CheckPart::Incoming, false) => "Zmail couldn't connect to the incoming server",
            (CheckPart::Outgoing, true) => "The outgoing server refused the sign-in",
            (CheckPart::Outgoing, false) => "Zmail couldn't connect to the outgoing server",
        };
        format!("{what} ({}): {}", self.server, self.message)
    }
}

/// What a test step is doing, for progress.
pub fn check_label(account: &NewAccount, part: CheckPart) -> String {
    match part {
        CheckPart::Incoming => format!("Signing in to {}", incoming_server(account)),
        CheckPart::Outgoing => format!(
            "Signing in to {}",
            account
                .outgoing
                .as_ref()
                .map(outgoing_server)
                .unwrap_or_default()
        ),
    }
}

/// Tests the account's sign-in on both halves, the incoming one first.
/// Proton accounts are tested by signing in to Proton, never here.
pub async fn check_connection(account: &NewAccount, secret: &str) -> Result<(), CheckFailure> {
    check_part(account, secret, CheckPart::Incoming).await?;
    check_part(account, secret, CheckPart::Outgoing).await
}

/// Tests one half of the account's sign-in.
pub async fn check_part(
    account: &NewAccount,
    secret: &str,
    part: CheckPart,
) -> Result<(), CheckFailure> {
    let failure = |server: String, error: anyhow::Error| CheckFailure {
        part,
        server,
        refused: refused(&error),
        message: format!("{error:#}"),
    };
    match (part, account.protocol) {
        (_, ProtocolKind::Proton) => Err(CheckFailure {
            part,
            server: "Proton".to_owned(),
            message: "Proton accounts are tested by signing in to Proton".to_owned(),
            refused: false,
        }),
        (CheckPart::Incoming, ProtocolKind::Imap) => {
            zmail_imap::check_sign_in(&account.endpoint, &account.username, secret, account.auth)
                .await
                .map_err(|error| failure(incoming_server(account), error))
        }
        (CheckPart::Incoming, ProtocolKind::Jmap) => {
            zmail_jmap::check_sign_in(&account.endpoint, &account.username, secret, account.auth)
                .await
                .map_err(|error| failure(incoming_server(account), error))
        }
        (CheckPart::Outgoing, _) => {
            let Some(outgoing) = &account.outgoing else {
                return Ok(());
            };
            if outgoing.kind != SubmissionKind::Smtp {
                // JMAP sends through the session tested above.
                return Ok(());
            }
            let config = match submission_config(outgoing, account, secret) {
                Ok(config) => config,
                Err(error) => return Err(failure(outgoing_server(outgoing), error)),
            };
            zmail_smtp::check_sign_in(&config)
                .await
                .map_err(|error| failure(outgoing_server(outgoing), error))
        }
    }
}

/// The SMTP settings to sign in with, as the outbox builds them.
fn submission_config(
    outgoing: &OutgoingConfig,
    account: &NewAccount,
    secret: &str,
) -> Result<zmail_smtp::SubmissionConfig> {
    let host = outgoing
        .host
        .clone()
        .ok_or_else(|| anyhow::anyhow!("there is no SMTP server"))?;
    let port = outgoing
        .port
        .ok_or_else(|| anyhow::anyhow!("there is no SMTP port"))?;
    let security = outgoing
        .security
        .ok_or_else(|| anyhow::anyhow!("there is no SMTP security choice"))?;
    let username = outgoing.username.clone();
    Ok(zmail_smtp::SubmissionConfig {
        host,
        port,
        secret: username.as_ref().map(|_| secret.to_owned()),
        username,
        auth: account.auth,
        security,
        hello_name: outgoing.hello_name.clone(),
    })
}

/// Whether an error is the server refusing the credentials.
fn refused(error: &anyhow::Error) -> bool {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<zmail_core::reauth::ReauthRequired>()
            .is_some()
    }) {
        return true;
    }
    let text = format!("{error:#}").to_ascii_lowercase();
    [
        "authenticationfailed",
        "authentication failed",
        "invalid credentials",
        "login failed",
        "[auth",
        "unauthorized",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// The incoming server as `host:port`, or the JMAP URL.
fn incoming_server(account: &NewAccount) -> String {
    match account.protocol {
        ProtocolKind::Imap => url::Url::parse(&account.endpoint)
            .ok()
            .and_then(|url| {
                url.host_str()
                    .map(|host| format!("{host}:{}", url.port().unwrap_or(993)))
            })
            .unwrap_or_else(|| account.endpoint.clone()),
        ProtocolKind::Jmap | ProtocolKind::Proton => account.endpoint.clone(),
    }
}

/// The outgoing server as `host:port`.
fn outgoing_server(outgoing: &OutgoingConfig) -> String {
    format!(
        "{}:{}",
        outgoing.host.as_deref().unwrap_or_default(),
        outgoing.port.unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use zmail_core::{AuthKind, CachePolicySummary, NewAccount, OutgoingConfig, ProtocolKind};

    use super::{CheckPart, check_connection, check_label};
    use crate::form::AccountField;

    /// An IMAP account whose servers are a closed port on this machine.
    fn closed_account() -> NewAccount {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map(|address| address.port())
            .expect("a free port");
        let mut outgoing = OutgoingConfig::smtp_default("127.0.0.1");
        outgoing.port = Some(port);
        NewAccount {
            display_name: "jane@example.test".to_owned(),
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            endpoint: format!("imap://127.0.0.1:{port}"),
            username: "jane@example.test".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(outgoing),
            identities: Vec::new(),
        }
    }

    #[test]
    fn a_closed_port_fails_the_incoming_half_by_name() {
        let account = closed_account();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let failure = runtime
            .block_on(check_connection(&account, "secret"))
            .expect_err("nothing listens there");
        assert_eq!(failure.part, CheckPart::Incoming);
        assert!(!failure.refused);
        assert_eq!(failure.field(), AccountField::ImapHost);
        assert!(
            failure
                .summary()
                .starts_with("Zmail couldn't connect to the incoming server (127.0.0.1:")
        );
        assert!(
            !failure.summary().contains("secret"),
            "the secret never shows"
        );
        assert!(check_label(&account, CheckPart::Incoming).starts_with("Signing in to 127.0.0.1:"));
    }
}
