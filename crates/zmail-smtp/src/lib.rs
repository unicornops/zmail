//! Minimal SMTP submission client used by the Phase 2b harness.

use std::sync::Once;

use anyhow::{Context, Result, bail};
use mail_send::{
    Credentials, SmtpClientBuilder, mail_builder::MessageBuilder, smtp::message::Message,
};
use zmail_core::OutgoingMessage;

pub use zmail_core::SubmissionSecurity;

/// Delivery model for SMTP-native accounts.
pub const DELIVERY_MODEL: &str =
    "Queued submission with offline retry and explicit identity binding.";

/// Connection settings for an SMTP submission attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionConfig {
    /// SMTP hostname.
    pub host: String,
    /// SMTP port.
    pub port: u16,
    /// Optional username for SMTP AUTH.
    pub username: Option<String>,
    /// Optional password or app password for SMTP AUTH.
    pub secret: Option<String>,
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
            security: SubmissionSecurity::Plain,
            hello_name: "localhost".to_owned(),
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
        subject: message.subject.clone(),
        text_body: message.text_body.clone(),
        attachments: Vec::new(),
    };
    submit_outgoing_message(config, &message).await
}

/// Submits a composed message, including attachments, to an SMTP server.
pub async fn submit_outgoing_message(
    config: &SubmissionConfig,
    message: &OutgoingMessage,
) -> Result<()> {
    ensure_rustls_provider();

    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        bail!("SMTP submission requires at least one recipient");
    }

    let mut builder = SmtpClientBuilder::new(config.host.clone(), config.port)
        .helo_host(config.hello_name.clone());

    match (&config.username, &config.secret) {
        (Some(username), Some(secret)) => {
            builder = builder.credentials(Credentials::Plain {
                username: username.clone(),
                secret: secret.clone(),
            });
        }
        (None, None) => {}
        _ => bail!("SMTP credentials must include both username and secret"),
    }

    let recipients = all_recipients(message);
    let smtp_message = Message::empty()
        .from(message.from_email.as_str())
        .body(render_rfc822_message(message)?);
    let smtp_message = recipients
        .iter()
        .fold(smtp_message, |smtp_message, recipient| {
            smtp_message.to(recipient.as_str())
        });

    match config.security {
        SubmissionSecurity::Plain => builder
            .connect_plain()
            .await
            .context("failed to connect to the SMTP server over plain text")?
            .send(smtp_message)
            .await
            .context("failed to submit the SMTP message")?,
        SubmissionSecurity::StartTls => builder
            .implicit_tls(false)
            .connect()
            .await
            .context("failed to connect to the SMTP server using STARTTLS")?
            .send(smtp_message)
            .await
            .context("failed to submit the SMTP message")?,
        SubmissionSecurity::ImplicitTls => builder
            .connect()
            .await
            .context("failed to connect to the SMTP server using implicit TLS")?
            .send(smtp_message)
            .await
            .context("failed to submit the SMTP message")?,
    }

    Ok(())
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

    if !message.to.is_empty() {
        builder = builder.to(message.to.iter().map(String::as_str).collect::<Vec<_>>());
    }
    if !message.cc.is_empty() {
        builder = builder.cc(message.cc.iter().map(String::as_str).collect::<Vec<_>>());
    }
    if !message.bcc.is_empty() {
        builder = builder.bcc(message.bcc.iter().map(String::as_str).collect::<Vec<_>>());
    }

    for attachment in &message.attachments {
        builder = builder.attachment(
            attachment.content_type.as_str(),
            attachment.filename.as_str(),
            attachment.bytes.as_slice(),
        );
    }

    builder
        .write_to_vec()
        .context("failed to render the RFC-822 message")
}

fn normalize_body(body: &str) -> String {
    body.replace("\r\n", "\n")
}

fn all_recipients(message: &OutgoingMessage) -> Vec<String> {
    message
        .to
        .iter()
        .chain(message.cc.iter())
        .chain(message.bcc.iter())
        .cloned()
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
    use zmail_core::{OutgoingAttachment, OutgoingMessage};

    #[test]
    fn renders_basic_rfc822_headers() {
        let message = OutgoingMessage {
            from_email: "sender@localhost".to_owned(),
            from_name: Some("Sender".to_owned()),
            to: vec!["recipient@localhost".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Phase 2b".to_owned(),
            text_body: "Line one\nLine two".to_owned(),
            attachments: Vec::new(),
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
            subject: "Attachment".to_owned(),
            text_body: "Attachment body".to_owned(),
            attachments: vec![OutgoingAttachment {
                filename: "note.txt".to_owned(),
                content_type: "text/plain".to_owned(),
                bytes: b"payload".to_vec(),
            }],
        };

        let rendered =
            String::from_utf8(render_rfc822_message(&message).expect("message should render"))
                .expect("message should be utf-8");
        assert!(rendered.contains("multipart/mixed"));
        assert!(rendered.contains("note.txt"));
        assert!(rendered.contains("copy@localhost"));
        assert!(rendered.contains("blind@localhost"));
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
            subject: message.subject,
            text_body: message.text_body,
            attachments: Vec::new(),
        };
        assert!(render_rfc822_message(&converted).is_ok());
    }
}
