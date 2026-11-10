//! Minimal SMTP submission client used by the Phase 2b harness.

use std::{sync::Once, time::Duration};

use anyhow::{Context, Result, bail};
use mail_send::{
    Credentials, SmtpClientBuilder,
    mail_builder::{MessageBuilder, headers::address::Address},
    smtp::message::Message,
};
use zmail_core::{
    AuthKind, OutgoingMessage,
    participants::{Participant, envelope_address},
    reauth::{ReauthReason, ReauthRequired},
    threading::bare_message_id,
};

pub use zmail_core::SubmissionSecurity;

#[cfg(test)]
mod timeout_tests;

/// Delivery model for SMTP-native accounts.
pub const DELIVERY_MODEL: &str =
    "Queued submission with offline retry and explicit identity binding.";

/// How long connecting, the TLS handshake, the greeting and signing in may
/// take in all. mail-send's own default is an hour.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// The base for each command of a send. RFC 5321 section 4.5.3.2.6 gives
/// the server ten minutes to answer the end of the message (it may be
/// scanning it), and a send given up while the server was still deciding
/// may go out twice when retried.
const SEND_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The slowest upload a send allows for on top of [`SEND_TIMEOUT`]:
/// 32 KiB/s (256 kbit/s), so a large attachment on a slow link still goes.
const SLOWEST_UPLOAD_BYTES_PER_SECOND: u64 = 32 * 1024;

/// The time limits of one submission; tests shorten them.
#[derive(Debug, Clone, Copy)]
struct Timeouts {
    connect: Duration,
    send: Duration,
}

const TIMEOUTS: Timeouts = Timeouts {
    connect: CONNECT_TIMEOUT,
    send: SEND_TIMEOUT,
};

impl Timeouts {
    /// The limit for each command of sending a message of `bytes`:
    /// mail-send times the whole upload and the server's answer to it as
    /// one command.
    fn for_message(self, bytes: usize) -> Duration {
        let upload = (bytes as u64).div_ceil(SLOWEST_UPLOAD_BYTES_PER_SECOND);
        self.send + Duration::from_secs(upload)
    }
}

/// Connection settings for an SMTP submission attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionConfig {
    /// SMTP hostname.
    pub host: String,
    /// SMTP port.
    pub port: u16,
    /// Optional username for SMTP AUTH.
    pub username: Option<String>,
    /// Optional password or app password for SMTP AUTH; for OAuth2 accounts
    /// this is the current access token.
    pub secret: Option<String>,
    /// Authentication method used for SMTP AUTH.
    pub auth: AuthKind,
    /// Transport security mode.
    pub security: SubmissionSecurity,
    /// EHLO hostname sent by the client.
    pub hello_name: String,
}

/// Plain-text message submitted through SMTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedMessage {
    /// SMTP envelope sender.
    pub envelope_from: String,
    /// Optional display name for the From header.
    pub from_name: Option<String>,
    /// One or more recipient addresses.
    pub recipients: Vec<String>,
    /// ASCII-focused subject line used by the harness.
    pub subject: String,
    /// Plain text body content.
    pub text_body: String,
}

impl SubmissionConfig {
    /// Returns a localhost-friendly cleartext config for the Phase 2b harness.
    pub fn plain_local(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            username: None,
            secret: None,
            auth: AuthKind::Password,
            security: SubmissionSecurity::Plain,
            hello_name: "localhost".to_owned(),
        }
    }

    /// Builds the SASL credentials the SMTP AUTH exchange uses, if any.
    ///
    /// Password accounts authenticate with `AUTH PLAIN`; OAuth2 accounts
    /// authenticate with `AUTH XOAUTH2` using the current access token as
    /// the bearer secret.
    fn sasl_credentials(&self) -> Result<Option<Credentials<String>>> {
        match (&self.username, &self.secret) {
            (Some(username), Some(secret)) => Ok(Some(match self.auth {
                AuthKind::Password => Credentials::Plain {
                    username: username.clone(),
                    secret: secret.clone(),
                },
                AuthKind::OAuth2 => Credentials::XOauth2 {
                    username: username.clone(),
                    secret: secret.clone(),
                },
                AuthKind::ProtonSession => bail!("Proton accounts do not send over SMTP"),
            })),
            (None, None) => Ok(None),
            _ => bail!("SMTP credentials must include both username and secret"),
        }
    }
}

/// Submits a plain text message to an SMTP server.
pub async fn submit_text_message(
    config: &SubmissionConfig,
    message: &SubmittedMessage,
) -> Result<()> {
    let message = OutgoingMessage {
        from_email: message.envelope_from.clone(),
        from_name: message.from_name.clone(),
        to: message.recipients.clone(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: message.subject.clone(),
        text_body: message.text_body.clone(),
        attachments: Vec::new(),
        message_id: None,
        in_reply_to: None,
        references: Vec::new(),
        reply_action: None,
    };
    submit_outgoing_message(config, &message).await
}

/// Submits a composed message, including attachments, to an SMTP server.
pub async fn submit_outgoing_message(
    config: &SubmissionConfig,
    message: &OutgoingMessage,
) -> Result<()> {
    submit_within(config, message, TIMEOUTS).await
}

async fn submit_within(
    config: &SubmissionConfig,
    message: &OutgoingMessage,
    timeouts: Timeouts,
) -> Result<()> {
    ensure_rustls_provider();

    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        bail!("SMTP submission requires at least one recipient");
    }
    // mail-send writes `MAIL FROM` and `RCPT TO` as they are, so an
    // address that could break out of them never reaches the server.
    message.check_addresses()?;

    let builder = client_builder(config, timeouts)?;
    let recipients = all_recipients(message);
    let body = render_rfc822_message(message)?;
    let send_timeout = timeouts.for_message(body.len());
    let smtp_message = Message::empty()
        .from(message.from_email.as_str())
        .body(body);
    let smtp_message = recipients
        .iter()
        .fold(smtp_message, |smtp_message, recipient| {
            smtp_message.to(recipient.as_str())
        });

    // mail-send 0.7 returns the server's final reply (with the queue ID);
    // an error status is already an `Err`, and nothing here uses the text.
    match config.security {
        SubmissionSecurity::Plain => {
            let mut client = builder.connect_plain().await.map_err(|error| {
                connect_error(
                    error,
                    "failed to connect to the SMTP server over plain text",
                )
            })?;
            client.timeout = send_timeout;
            client.send(smtp_message).await
        }
        SubmissionSecurity::StartTls => {
            let mut client = builder
                .implicit_tls(false)
                .connect()
                .await
                .map_err(|error| {
                    connect_error(error, "failed to connect to the SMTP server using STARTTLS")
                })?;
            client.timeout = send_timeout;
            client.send(smtp_message).await
        }
        SubmissionSecurity::ImplicitTls => {
            let mut client = builder.connect().await.map_err(|error| {
                connect_error(
                    error,
                    "failed to connect to the SMTP server using implicit TLS",
                )
            })?;
            client.timeout = send_timeout;
            client.send(smtp_message).await
        }
    }
    .context("failed to submit the SMTP message")?;

    Ok(())
}

/// The client for `config`, with its sign-in and connect timeout.
fn client_builder(
    config: &SubmissionConfig,
    timeouts: Timeouts,
) -> Result<SmtpClientBuilder<String>> {
    let mut builder = SmtpClientBuilder::new(config.host.clone(), config.port)
        .map_err(anyhow::Error::msg)?
        .helo_host(config.hello_name.clone())
        .timeout(timeouts.connect);
    if let Some(credentials) = config.sasl_credentials()? {
        builder = builder.credentials(credentials);
    }
    Ok(builder)
}

/// Connects, signs in (`AUTH`, when the config has credentials) and says
/// goodbye, sending nothing: the add-account flow's connection test. A
/// refused sign-in is a [`ReauthRequired`] error, as when sending.
pub async fn check_sign_in(config: &SubmissionConfig) -> Result<()> {
    check_sign_in_within(config, TIMEOUTS).await
}

async fn check_sign_in_within(config: &SubmissionConfig, timeouts: Timeouts) -> Result<()> {
    ensure_rustls_provider();
    let builder = client_builder(config, timeouts)?;
    // QUIT is a courtesy: the sign-in has already succeeded.
    match config.security {
        SubmissionSecurity::Plain => {
            let client = builder.connect_plain().await.map_err(|error| {
                connect_error(
                    error,
                    "failed to connect to the SMTP server over plain text",
                )
            })?;
            let _ = client.quit().await;
        }
        SubmissionSecurity::StartTls => {
            let client = builder
                .implicit_tls(false)
                .connect()
                .await
                .map_err(|error| {
                    connect_error(error, "failed to connect to the SMTP server using STARTTLS")
                })?;
            let _ = client.quit().await;
        }
        SubmissionSecurity::ImplicitTls => {
            let client = builder.connect().await.map_err(|error| {
                connect_error(
                    error,
                    "failed to connect to the SMTP server using implicit TLS",
                )
            })?;
            let _ = client.quit().await;
        }
    }
    Ok(())
}

/// Wraps a connection failure. A refused sign-in (`AUTH` answered with
/// 535 or another failure) is a [`ReauthRequired`] error, since retrying
/// the same credentials cannot work; the user has to sign in again.
fn connect_error(error: mail_send::Error, context: &'static str) -> anyhow::Error {
    match error {
        mail_send::Error::AuthenticationFailed(reply) => anyhow::Error::new(ReauthRequired::new(
            ReauthReason::SendRefused,
            format!(
                "{} {}.{}.{} {}",
                reply.code, reply.esc[0], reply.esc[1], reply.esc[2], reply.message
            ),
        ))
        .context(context),
        error => anyhow::Error::new(error).context(context),
    }
}

/// Renders a composed outgoing message into RFC-822 bytes.
pub fn render_rfc822_message(message: &OutgoingMessage) -> Result<Vec<u8>> {
    if all_recipients(message).is_empty() {
        bail!("message rendering requires at least one recipient");
    }

    let from_display = message
        .from_name
        .clone()
        .unwrap_or_else(|| message.from_email.clone());
    let mut builder = MessageBuilder::new()
        .from((from_display.as_str(), message.from_email.as_str()))
        .subject(message.subject.as_str())
        .text_body(normalize_body(&message.text_body));

    // The threading headers (RFC 5322 section 3.6.4). Without a Message-ID
    // of the outbox's, mail-builder makes one up on the device's host name.
    if let Some(message_id) = message.message_id.as_deref().and_then(bare_message_id) {
        builder = builder.message_id(message_id);
    }
    if let Some(in_reply_to) = message.in_reply_to.as_deref().and_then(bare_message_id) {
        builder = builder.in_reply_to(in_reply_to);
    }
    let references = message
        .references
        .iter()
        .filter_map(|id| bare_message_id(id))
        .collect::<Vec<_>>();
    if !references.is_empty() {
        builder = builder.references(references);
    }

    if !message.to.is_empty() {
        builder = builder.to(address_list(&message.to));
    }
    if !message.cc.is_empty() {
        builder = builder.cc(address_list(&message.cc));
    }
    // The sending identity's reply-to (RFC 5322 section 3.6.2); none when
    // it has none, so replies go to From.
    if !message.reply_to.is_empty() {
        builder = builder.reply_to(address_list(&message.reply_to));
    }
    // Bcc recipients never appear in the message itself, or every
    // recipient would see them: they are only in the envelope (SMTP
    // `RCPT TO` here, the JMAP EmailSubmission envelope in zmail-jmap).

    for attachment in &message.attachments {
        builder = builder.attachment(
            attachment_content_type(&attachment.content_type),
            attachment.filename.as_str(),
            attachment.bytes.as_slice(),
        );
    }

    builder
        .write_to_vec()
        .context("failed to render the RFC-822 message")
}

/// An attachment's media type as its `Content-Type` header gets it: a
/// `type/subtype` of RFC 2045 tokens as it is, anything else
/// `application/octet-stream`. mail-builder writes the type as it is, and
/// a forward's comes from the received mail or the JMAP server.
fn attachment_content_type(content_type: &str) -> &str {
    let is_token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !br#"()<>@,;:\"/[]?="#.contains(&byte))
    };
    match content_type.split_once('/') {
        Some((kind, subtype)) if is_token(kind) && is_token(subtype) => content_type,
        _ => "application/octet-stream",
    }
}

fn normalize_body(body: &str) -> String {
    body.replace("\r\n", "\n")
}

/// Recipients (or Reply-To addresses) as an address header: each `Name
/// <address>` (as drafts keep them, a reply's included) with its name and
/// address apart, so the header names the address, not the whole entry.
fn address_list(recipients: &[String]) -> Address<'_> {
    Address::new_list(
        recipients
            .iter()
            .map(|recipient| {
                let participant = Participant::parse(recipient);
                match participant.email {
                    Some(email) => Address::new_address(participant.name, email),
                    None => Address::new_address(None::<String>, recipient.trim()),
                }
            })
            .collect(),
    )
}

/// Every recipient's bare address, for the SMTP envelope (`RCPT TO`).
fn all_recipients(message: &OutgoingMessage) -> Vec<String> {
    message
        .to
        .iter()
        .chain(message.cc.iter())
        .chain(message.bcc.iter())
        .map(|recipient| envelope_address(recipient))
        .collect()
}

fn ensure_rustls_provider() {
    static RUSTLS_PROVIDER: Once = Once::new();

    RUSTLS_PROVIDER.call_once(|| {
        let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::{SubmittedMessage, render_rfc822_message};
    use mail_send::Credentials;
    use zmail_core::{AuthKind, OutgoingAttachment, OutgoingMessage, SubmissionSecurity};

    use super::SubmissionConfig;

    fn config(auth: AuthKind) -> SubmissionConfig {
        SubmissionConfig {
            host: "smtp.example.test".to_owned(),
            port: 587,
            username: Some("user@example.test".to_owned()),
            secret: Some("the-secret".to_owned()),
            auth,
            security: SubmissionSecurity::StartTls,
            hello_name: "localhost".to_owned(),
        }
    }

    #[test]
    fn sasl_credentials_pick_the_mechanism_from_the_auth_kind() {
        // Password accounts authenticate with AUTH PLAIN.
        let credentials = config(AuthKind::Password)
            .sasl_credentials()
            .expect("password credentials should build")
            .expect("credentials should be present");
        assert!(matches!(
            credentials,
            Credentials::Plain { username, secret }
                if username == "user@example.test" && secret == "the-secret"
        ));

        // OAuth2 accounts authenticate with AUTH XOAUTH2, using the current
        // access token as the bearer secret.
        let credentials = config(AuthKind::OAuth2)
            .sasl_credentials()
            .expect("OAuth2 credentials should build")
            .expect("credentials should be present");
        assert!(matches!(
            credentials,
            Credentials::XOauth2 { username, secret }
                if username == "user@example.test" && secret == "the-secret"
        ));
    }

    #[test]
    fn sasl_credentials_require_both_username_and_secret() {
        let mut partial = config(AuthKind::Password);
        partial.secret = None;
        assert!(
            partial.sasl_credentials().is_err(),
            "a username without a secret must be rejected"
        );

        let anonymous = SubmissionConfig {
            username: None,
            secret: None,
            ..config(AuthKind::OAuth2)
        };
        assert!(
            anonymous
                .sasl_credentials()
                .expect("no credentials is a valid state")
                .is_none(),
            "an anonymous submission carries no credentials"
        );
    }

    #[test]
    fn renders_basic_rfc822_headers() {
        let message = OutgoingMessage {
            from_email: "sender@localhost".to_owned(),
            from_name: Some("Sender".to_owned()),
            to: vec!["recipient@localhost".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Phase 2b".to_owned(),
            text_body: "Line one\nLine two".to_owned(),
            attachments: Vec::new(),
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            reply_action: None,
        };

        let rendered =
            String::from_utf8(render_rfc822_message(&message).expect("message should render"))
                .expect("message should be utf-8");
        assert!(
            rendered.contains("From: \"Sender\" <sender@localhost>")
                || rendered.contains("From: Sender <sender@localhost>")
        );
        assert!(rendered.contains("recipient@localhost"));
        assert!(rendered.contains("Subject: Phase 2b"));
        assert!(rendered.contains("Line one"));
        assert!(rendered.contains("Line two"));
    }

    #[test]
    fn renders_multipart_messages_with_attachments() {
        let message = OutgoingMessage {
            from_email: "sender@localhost".to_owned(),
            from_name: Some("Sender".to_owned()),
            to: vec!["recipient@localhost".to_owned()],
            cc: vec!["copy@localhost".to_owned()],
            bcc: vec!["blind@localhost".to_owned()],
            reply_to: Vec::new(),
            subject: "Attachment".to_owned(),
            text_body: "Attachment body".to_owned(),
            attachments: vec![OutgoingAttachment {
                filename: "note.txt".to_owned(),
                content_type: "text/plain".to_owned(),
                bytes: b"payload".to_vec(),
            }],
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            reply_action: None,
        };

        let rendered =
            String::from_utf8(render_rfc822_message(&message).expect("message should render"))
                .expect("message should be utf-8");
        assert!(rendered.contains("multipart/mixed"));
        assert!(rendered.contains("note.txt"));
        assert!(rendered.contains("copy@localhost"));
        // Bcc goes in the envelope only, never in the message.
        assert!(!rendered.contains("blind@localhost"), "{rendered}");
        assert!(
            !rendered.to_ascii_lowercase().contains("bcc:"),
            "{rendered}"
        );
        assert!(super::all_recipients(&message).contains(&"blind@localhost".to_owned()));
    }

    #[test]
    fn attachment_types_that_are_not_type_and_subtype_are_sent_as_octet_stream() {
        let attachment = |content_type: &str| OutgoingAttachment {
            filename: "file.bin".to_owned(),
            content_type: content_type.to_owned(),
            bytes: b"payload".to_vec(),
        };
        let message = OutgoingMessage {
            from_email: "sender@localhost".to_owned(),
            from_name: None,
            to: vec!["recipient@localhost".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Types".to_owned(),
            text_body: "Body".to_owned(),
            attachments: vec![
                attachment("image/svg+xml"),
                attachment("text/plain\r\nX-Injected: yes"),
                attachment("text"),
                attachment("text/plain; charset=utf-8"),
                attachment("text/pl ain"),
            ],
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            reply_action: None,
        };
        let rendered =
            String::from_utf8(render_rfc822_message(&message).expect("renders")).expect("utf-8");
        assert!(
            rendered.contains("Content-Type: image/svg+xml"),
            "{rendered}"
        );
        assert!(!rendered.contains("X-Injected"), "{rendered}");
        assert!(!rendered.contains("charset=utf-8;"), "{rendered}");
        assert_eq!(
            rendered
                .matches("Content-Type: application/octet-stream")
                .count(),
            4,
            "{rendered}"
        );
    }

    #[test]
    fn bcc_only_messages_render_and_keep_the_recipient_in_the_envelope() {
        let message = OutgoingMessage {
            from_email: "sender@localhost".to_owned(),
            from_name: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: vec!["Hidden Person <hidden@localhost>".to_owned()],
            reply_to: Vec::new(),
            subject: "Only Bcc".to_owned(),
            text_body: "Body".to_owned(),
            attachments: Vec::new(),
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            reply_action: None,
        };
        let rendered =
            String::from_utf8(render_rfc822_message(&message).expect("renders")).expect("utf-8");
        assert!(!rendered.contains("hidden@localhost"), "{rendered}");
        assert_eq!(
            super::all_recipients(&message),
            vec!["hidden@localhost".to_owned()]
        );
    }

    #[test]
    fn renders_the_threading_headers_of_a_reply() {
        let message = OutgoingMessage {
            from_email: "me@work.example".to_owned(),
            from_name: Some("Me".to_owned()),
            to: vec!["kai@example.com".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Re: Plan".to_owned(),
            text_body: "Sounds good.".to_owned(),
            attachments: Vec::new(),
            message_id: Some("a1b2.c3d4@work.example".to_owned()),
            in_reply_to: Some("<parent@example.com>".to_owned()),
            references: vec![
                "root@example.com".to_owned(),
                "parent@example.com".to_owned(),
            ],
            reply_action: None,
        };
        let rendered =
            String::from_utf8(render_rfc822_message(&message).expect("message should render"))
                .expect("message should be utf-8");
        assert!(rendered.contains("Message-ID: <a1b2.c3d4@work.example>\r\n"));
        assert!(rendered.contains("In-Reply-To: <parent@example.com>\r\n"));
        assert!(rendered.contains("References: <root@example.com> <parent@example.com>\r\n"));

        // A new message: only a Message-ID.
        let new = OutgoingMessage {
            in_reply_to: None,
            references: Vec::new(),
            ..message
        };
        let rendered = String::from_utf8(render_rfc822_message(&new).expect("renders"))
            .expect("message should be utf-8");
        assert!(rendered.contains("Message-ID: <a1b2.c3d4@work.example>"));
        assert!(!rendered.contains("In-Reply-To") && !rendered.contains("References"));
    }

    #[test]
    fn recipients_with_display_names_keep_the_address_apart() {
        let message = OutgoingMessage {
            from_email: "me@work.example".to_owned(),
            from_name: Some("Me".to_owned()),
            to: vec!["Kai Santos <kai@example.com>".to_owned()],
            cc: vec![
                "\"Doe, Jane\" <jane@example.com>".to_owned(),
                "bo@example.com".to_owned(),
            ],
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Re: Plan".to_owned(),
            text_body: "Yes.".to_owned(),
            attachments: Vec::new(),
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            reply_action: None,
        };
        assert_eq!(
            super::all_recipients(&message),
            ["kai@example.com", "jane@example.com", "bo@example.com"],
            "the envelope has bare addresses"
        );
        let rendered = String::from_utf8(render_rfc822_message(&message).expect("renders"))
            .expect("message should be utf-8");
        assert!(
            rendered.contains("To: \"Kai Santos\" <kai@example.com>\r\n"),
            "{rendered}"
        );
        assert!(rendered.contains("<jane@example.com>"), "{rendered}");
        assert!(!rendered.contains("<Kai Santos"), "{rendered}");
    }

    fn message_with_reply_to(reply_to: &[&str]) -> OutgoingMessage {
        OutgoingMessage {
            from_email: "me@work.example".to_owned(),
            from_name: Some("Me".to_owned()),
            to: vec!["kai@example.com".to_owned()],
            cc: Vec::new(),
            bcc: vec!["hidden@example.com".to_owned()],
            reply_to: reply_to.iter().map(|&address| address.to_owned()).collect(),
            subject: "Plan".to_owned(),
            text_body: "Yes.".to_owned(),
            attachments: Vec::new(),
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            reply_action: None,
        }
    }

    fn rendered(message: &OutgoingMessage) -> String {
        String::from_utf8(render_rfc822_message(message).expect("renders")).expect("utf-8")
    }

    #[test]
    fn writes_the_identity_reply_to_with_the_address_apart() {
        let rendered = rendered(&message_with_reply_to(&["Help Desk <desk@work.example>"]));
        assert!(
            rendered.contains("Reply-To: \"Help Desk\" <desk@work.example>\r\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("<Help Desk"), "{rendered}");
        // The envelope is unchanged: Reply-To is not a recipient.
        assert_eq!(
            super::all_recipients(&message_with_reply_to(&["desk@work.example"])),
            ["kai@example.com", "hidden@example.com"]
        );
        // Bcc still never appears in the message.
        assert!(!rendered.contains("hidden@example.com"), "{rendered}");
    }

    #[test]
    fn writes_every_reply_to_address() {
        let rendered = rendered(&message_with_reply_to(&[
            "\"Tickets, Inc\" <tickets@example.com>",
            "ops@example.com",
        ]));
        assert!(
            rendered.contains(
                "Reply-To: \"Tickets, Inc\" <tickets@example.com>, <ops@example.com>\r\n"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn writes_no_reply_to_when_the_identity_has_none() {
        let rendered = rendered(&message_with_reply_to(&[]));
        assert!(
            !rendered.to_ascii_lowercase().contains("reply-to:"),
            "{rendered}"
        );
    }

    #[test]
    fn legacy_submitted_message_shape_remains_usable() {
        let message = SubmittedMessage {
            envelope_from: "sender@localhost".to_owned(),
            from_name: Some("Sender".to_owned()),
            recipients: vec!["recipient@localhost".to_owned()],
            subject: "Legacy".to_owned(),
            text_body: "Body".to_owned(),
        };

        let converted = OutgoingMessage {
            from_email: message.envelope_from,
            from_name: message.from_name,
            to: message.recipients,
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: message.subject,
            text_body: message.text_body,
            attachments: Vec::new(),
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            reply_action: None,
        };
        assert!(render_rfc822_message(&converted).is_ok());
    }
}

#[cfg(test)]
mod envelope_tests;
