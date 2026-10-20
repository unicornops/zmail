//! Built-in settings for the providers the onboarding flow offers as
//! buttons, also used when a domain's MX record points at one of them.

use std::fmt;

use crate::{
    AuthMethod, EmailAddress, Found, JmapSettings, MailSettings, Security, ServerSettings, Source,
    StepFindings,
};

/// A hosted mail provider zmail has settings for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Gmail and Google Workspace.
    Google,
    /// Microsoft 365 and Outlook.com.
    Microsoft365,
    /// Fastmail, over JMAP.
    Fastmail,
    /// iCloud Mail.
    ICloud,
}

impl Provider {
    /// Every provider, in the order the onboarding flow lists them.
    pub const ALL: [Provider; 4] = [
        Provider::Google,
        Provider::Microsoft365,
        Provider::Fastmail,
        Provider::ICloud,
    ];

    /// The provider's name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Microsoft365 => "Microsoft 365",
            Self::Fastmail => "Fastmail",
            Self::ICloud => "iCloud",
        }
    }

    /// Whether the provider only accepts an app-specific password from mail
    /// clients (the account password is refused).
    pub fn needs_app_password(self) -> bool {
        matches!(self, Self::Google | Self::Fastmail | Self::ICloud)
    }

    /// Whether the provider has turned off password sign-in for IMAP and
    /// SMTP, so only OAuth2 works.
    pub fn needs_oauth2(self) -> bool {
        matches!(self, Self::Microsoft365)
    }

    /// The provider's settings for `email`.
    pub fn settings(self, email: &EmailAddress) -> MailSettings {
        self.settings_from(email, Source::Provider(self))
    }

    /// The provider's settings for `email`, credited to `source`.
    fn settings_from(self, email: &EmailAddress, source: Source) -> MailSettings {
        let auth = if self.needs_oauth2() {
            AuthMethod::OAuth2
        } else {
            AuthMethod::Password
        };
        let server = |(host, port, security): (&str, u16, Security)| {
            Found::new(
                ServerSettings {
                    host: host.to_owned(),
                    port,
                    security,
                    username: email.to_string(),
                    auth,
                },
                source.clone(),
            )
        };
        match self.servers() {
            Servers::Jmap(session_url) => MailSettings::Jmap(Found::new(
                JmapSettings {
                    session_url: session_url.to_owned(),
                    username: email.to_string(),
                },
                source.clone(),
            )),
            Servers::ImapSmtp { imap, smtp } => MailSettings::ImapSmtp {
                imap: server(imap),
                smtp: server(smtp),
            },
        }
    }

    /// The provider's settings as step findings from `source`.
    pub(crate) fn findings(self, email: &EmailAddress, source: Source) -> StepFindings {
        match self.settings_from(email, source) {
            MailSettings::Jmap(jmap) => StepFindings {
                jmap: Some(jmap),
                ..StepFindings::default()
            },
            MailSettings::ImapSmtp { imap, smtp } => StepFindings {
                imap: Some(imap),
                smtp: Some(smtp),
                ..StepFindings::default()
            },
        }
    }

    /// The provider's published servers.
    fn servers(self) -> Servers {
        match self {
            Self::Google => Servers::ImapSmtp {
                imap: ("imap.gmail.com", 993, Security::Tls),
                smtp: ("smtp.gmail.com", 465, Security::Tls),
            },
            Self::Microsoft365 => Servers::ImapSmtp {
                imap: ("outlook.office365.com", 993, Security::Tls),
                smtp: ("smtp.office365.com", 587, Security::StartTls),
            },
            Self::Fastmail => Servers::Jmap("https://api.fastmail.com/jmap/session"),
            Self::ICloud => Servers::ImapSmtp {
                imap: ("imap.mail.me.com", 993, Security::Tls),
                smtp: ("smtp.mail.me.com", 587, Security::StartTls),
            },
        }
    }

    /// The provider whose mail servers `mx_host` belongs to, if any.
    pub fn from_mx_host(mx_host: &str) -> Option<Self> {
        let mx_host = mx_host.trim_end_matches('.').to_ascii_lowercase();
        let under = |suffix: &str| {
            mx_host
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with('.'))
        };
        if under("google.com") || under("googlemail.com") {
            Some(Self::Google)
        } else if under("mail.protection.outlook.com") {
            Some(Self::Microsoft365)
        } else if under("messagingengine.com") {
            Some(Self::Fastmail)
        } else if under("mail.icloud.com") {
            Some(Self::ICloud)
        } else {
            None
        }
    }
}

/// A provider's servers: host, port and security.
enum Servers {
    Jmap(&'static str),
    ImapSmtp {
        imap: (&'static str, u16, Security),
        smtp: (&'static str, u16, Security),
    },
}

impl fmt::Display for Provider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::Provider;
    use crate::{EmailAddress, MailSettings, Security, Source};

    #[test]
    fn every_provider_has_complete_tls_settings() {
        let email = EmailAddress::parse("me@example.com").expect("valid address");
        for provider in Provider::ALL {
            match provider.settings(&email) {
                MailSettings::Jmap(jmap) => {
                    assert!(jmap.value.session_url.starts_with("https://"));
                    assert_eq!(jmap.source, Source::Provider(provider));
                }
                MailSettings::ImapSmtp { imap, smtp } => {
                    assert_eq!(imap.value.security, Security::Tls);
                    assert_ne!(smtp.value.security, Security::None);
                    assert_eq!(imap.value.username, "me@example.com");
                }
            }
        }
    }

    #[test]
    fn mx_hosts_match_on_label_boundaries() {
        assert_eq!(
            Provider::from_mx_host("aspmx.l.google.com."),
            Some(Provider::Google)
        );
        assert_eq!(
            Provider::from_mx_host("smtp.google.com"),
            Some(Provider::Google)
        );
        assert_eq!(
            Provider::from_mx_host("example-com.mail.protection.outlook.com"),
            Some(Provider::Microsoft365)
        );
        assert_eq!(
            Provider::from_mx_host("in1-smtp.messagingengine.com"),
            Some(Provider::Fastmail)
        );
        assert_eq!(
            Provider::from_mx_host("mx01.mail.icloud.com"),
            Some(Provider::ICloud)
        );
        assert_eq!(Provider::from_mx_host("notgoogle.com"), None);
        assert_eq!(Provider::from_mx_host("google.com.evil.example"), None);
        assert_eq!(Provider::from_mx_host("google.com"), None);
    }
}
