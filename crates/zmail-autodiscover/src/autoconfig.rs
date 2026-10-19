//! Thunderbird autoconfig files (`config-v1.1.xml`): the domain's own copy
//! and Mozilla's ISPDB.
//!
//! The XML comes from the network and is untrusted. [`parse`] reads it with
//! DTDs disabled (so no entity of any kind is expanded and nothing external
//! is ever fetched), a node limit, a size limit applied before parsing, and
//! strict checks on every host name and port it returns.

use url::Url;

use crate::address::is_dns_name;
use crate::fetch::{Fetch, MAX_BODY_BYTES};
use crate::{
    AuthMethod, EmailAddress, Found, Security, ServerSettings, Source, StepFindings, StepResult,
};

/// The most XML nodes [`parse`] accepts. Real files have well under 200.
const MAX_NODES: u32 = 5_000;

/// Mozilla's ISPDB, which is keyed by the mail domain.
const ISPDB_BASE: &str = "https://autoconfig.thunderbird.net/v1.1/";

/// The servers an autoconfig file offers that zmail can use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Autoconfig {
    /// The best IMAP server: implicit TLS with a password or OAuth2.
    pub imap: Option<ServerSettings>,
    /// The best SMTP server: implicit TLS, then STARTTLS.
    pub smtp: Option<ServerSettings>,
    /// Why servers the file listed were left out, for the progress list.
    pub skipped: Vec<String>,
}

/// Why an autoconfig file was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Larger than [`MAX_BODY_BYTES`].
    TooLarge,
    /// Not UTF-8.
    NotUtf8,
    /// Not well-formed XML, or it has a DTD or too many nodes.
    Xml(String),
    /// Well-formed, but not a `clientConfig` document.
    NotAutoconfig,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(formatter, "the file is larger than {MAX_BODY_BYTES} bytes"),
            Self::NotUtf8 => formatter.write_str("the file is not UTF-8"),
            Self::Xml(error) => write!(formatter, "the file is not valid XML: {error}"),
            Self::NotAutoconfig => formatter.write_str("the file is not an autoconfig file"),
        }
    }
}

/// Parses an autoconfig file for `email`, filling in the `%EMAILADDRESS%`,
/// `%EMAILLOCALPART%` and `%EMAILDOMAIN%` placeholders.
pub fn parse(bytes: &[u8], email: &EmailAddress) -> Result<Autoconfig, ParseError> {
    if bytes.len() > MAX_BODY_BYTES {
        return Err(ParseError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ParseError::NotUtf8)?;
    let options = roxmltree::ParsingOptions {
        // A DTD is an error: no internal or external entities at all.
        allow_dtd: false,
        nodes_limit: MAX_NODES,
        entity_resolver: None,
    };
    let document = roxmltree::Document::parse_with_options(text, options)
        .map_err(|error| ParseError::Xml(error.to_string()))?;
    let root = document.root_element();
    if !root.has_tag_name("clientConfig") {
        return Err(ParseError::NotAutoconfig);
    }
    let provider = root
        .children()
        .find(|node| node.has_tag_name("emailProvider"))
        .ok_or(ParseError::NotAutoconfig)?;

    let mut config = Autoconfig::default();
    let mut imap = Vec::new();
    let mut smtp = Vec::new();
    for server in provider.children().filter(roxmltree::Node::is_element) {
        let kind = server.attribute("type").unwrap_or_default();
        let candidates = match (server.tag_name().name(), kind) {
            ("incomingServer", "imap") => &mut imap,
            ("outgoingServer", "smtp") => &mut smtp,
            // POP3, EWS and anything newer: not protocols zmail speaks.
            _ => continue,
        };
        match read_server(server, email) {
            Ok(settings) => candidates.push(settings),
            Err(reason) => config
                .skipped
                .push(format!("{kind} server skipped: {reason}")),
        }
    }

    // IMAP: implicit TLS only; zmail's IMAP client has no STARTTLS.
    config.imap = imap
        .iter()
        .find(|server| server.security == Security::Tls)
        .cloned();
    if config.imap.is_none()
        && imap
            .iter()
            .any(|server| server.security == Security::StartTls)
    {
        config
            .skipped
            .push("the IMAP server only offers STARTTLS, which zmail doesn't support yet".into());
    }
    config.smtp = [Security::Tls, Security::StartTls]
        .into_iter()
        .find_map(|security| smtp.iter().find(|server| server.security == security))
        .cloned();
    Ok(config)
}

/// Reads one `incomingServer`/`outgoingServer` element.
fn read_server(
    server: roxmltree::Node<'_, '_>,
    email: &EmailAddress,
) -> Result<ServerSettings, String> {
    let text = |name: &str| {
        server
            .children()
            .find(|node| node.has_tag_name(name))
            .and_then(|node| node.text())
            .map(|text| substitute(text.trim(), email))
    };
    let host = text("hostname").ok_or("no host name")?.to_ascii_lowercase();
    if !is_dns_name(&host) {
        return Err(format!("{host:?} is not a valid host name"));
    }
    let port = text("port")
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or("no valid port")?;
    let security = match text("socketType").as_deref() {
        Some("SSL") => Security::Tls,
        Some("STARTTLS") => Security::StartTls,
        // "plain" and anything unknown: never offered from discovery.
        _ => return Err(format!("{host} doesn't use TLS")),
    };
    let methods = server
        .children()
        .filter(|node| node.has_tag_name("authentication"))
        .filter_map(|node| node.text())
        .map(str::trim)
        .collect::<Vec<_>>();
    let auth = if methods.is_empty()
        || methods.iter().any(|method| {
            matches!(
                *method,
                "password-cleartext" | "plain" | "password-encrypted" | "secure"
            )
        }) {
        AuthMethod::Password
    } else if methods.contains(&"OAuth2") {
        AuthMethod::OAuth2
    } else {
        return Err(format!(
            "{host} needs a sign-in method zmail doesn't support"
        ));
    };
    let username = text("username")
        .filter(|username| !username.is_empty() && username.len() <= 320)
        .unwrap_or_else(|| email.to_string());
    Ok(ServerSettings {
        host,
        port,
        security,
        username,
        auth,
    })
}

/// Fills in the address placeholders autoconfig files use.
fn substitute(text: &str, email: &EmailAddress) -> String {
    text.replace("%EMAILADDRESS%", &email.to_string())
        .replace("%EMAILLOCALPART%", email.local_part())
        .replace("%EMAILDOMAIN%", email.domain())
}

/// The two places a domain publishes its own autoconfig file, in the order
/// Thunderbird tries them. Neither carries the address: the files are per
/// domain, so the provider learns nothing it needs.
pub(crate) fn domain_urls(domain: &str) -> Vec<Url> {
    [
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml"),
        format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
    ]
    .iter()
    .filter_map(|url| Url::parse(url).ok())
    .collect()
}

/// The ISPDB URL for a domain.
pub(crate) fn ispdb_url(domain: &str) -> Option<Url> {
    Url::parse(&format!("{ISPDB_BASE}{domain}")).ok()
}

/// [`crate::Step::DomainAutoconfig`].
pub(crate) fn domain_step(email: &EmailAddress, fetch: &dyn Fetch) -> StepResult {
    let mut errors = Vec::new();
    let mut notes = Vec::new();
    for url in domain_urls(email.domain()) {
        match fetch_config(&url, email, fetch) {
            Ok(Some(config)) if config.imap.is_some() || config.smtp.is_some() => {
                let source = Source::DomainAutoconfig {
                    url: url.to_string(),
                };
                return Ok(findings(config, &source));
            }
            Ok(Some(config)) => notes.extend(config.skipped),
            Ok(None) => {}
            Err(error) => errors.push(error),
        }
    }
    step_without_findings(notes, errors)
}

/// [`crate::Step::Ispdb`]. Sends the domain to Mozilla: only run it with
/// the user's consent.
pub(crate) fn ispdb_step(email: &EmailAddress, fetch: &dyn Fetch) -> StepResult {
    let Some(url) = ispdb_url(email.domain()) else {
        return Ok(StepFindings::default());
    };
    match fetch_config(&url, email, fetch)? {
        Some(config) if config.imap.is_some() || config.smtp.is_some() => Ok(findings(
            config,
            &Source::Ispdb {
                url: url.to_string(),
            },
        )),
        Some(config) => step_without_findings(config.skipped, Vec::new()),
        None => Ok(StepFindings::default()),
    }
}

/// Fetches and parses one autoconfig URL. `Ok(None)` means there is no
/// file there (any status but 200).
fn fetch_config(
    url: &Url,
    email: &EmailAddress,
    fetch: &dyn Fetch,
) -> Result<Option<Autoconfig>, String> {
    let response = fetch.get(url)?;
    if response.status != 200 {
        return Ok(None);
    }
    parse(&response.body, email)
        .map(Some)
        .map_err(|error| format!("{url}: {error}"))
}

fn findings(config: Autoconfig, source: &Source) -> StepFindings {
    StepFindings {
        jmap: None,
        imap: config.imap.map(|imap| Found::new(imap, source.clone())),
        smtp: config.smtp.map(|smtp| Found::new(smtp, source.clone())),
        note: (!config.skipped.is_empty()).then(|| config.skipped.join("; ")),
    }
}

/// A step that found nothing: an error when every URL failed, otherwise an
/// empty result with any notes.
fn step_without_findings(notes: Vec<String>, errors: Vec<String>) -> StepResult {
    if notes.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(StepFindings {
        note: (!notes.is_empty()).then(|| notes.join("; ")),
        ..StepFindings::default()
    })
}

#[cfg(test)]
mod tests {
    use super::{ParseError, domain_urls, ispdb_url, parse};
    use crate::{AuthMethod, EmailAddress, Security, ServerSettings};

    fn email() -> EmailAddress {
        EmailAddress::parse("jane@example.com").expect("valid address")
    }

    #[test]
    fn parses_a_typical_file_and_prefers_tls() {
        let config = parse(
            include_bytes!("../fixtures/example-autoconfig.xml"),
            &email(),
        )
        .expect("fixture parses");
        assert_eq!(
            config.imap,
            Some(ServerSettings {
                host: "imap.example.com".into(),
                port: 993,
                security: Security::Tls,
                username: "jane@example.com".into(),
                auth: AuthMethod::Password,
            })
        );
        // The fixture lists STARTTLS on 587 before TLS on 465.
        assert_eq!(
            config.smtp,
            Some(ServerSettings {
                host: "smtp.example.com".into(),
                port: 465,
                security: Security::Tls,
                username: "jane".into(),
                auth: AuthMethod::Password,
            })
        );
        // The POP3 server is ignored silently; the cleartext IMAP one is
        // noted.
        assert_eq!(config.skipped.len(), 1, "{:?}", config.skipped);
        assert!(config.skipped[0].contains("doesn't use TLS"));
    }

    #[test]
    fn falls_back_to_starttls_for_smtp_but_not_for_imap() {
        let config = parse(include_bytes!("../fixtures/starttls-only.xml"), &email())
            .expect("fixture parses");
        assert_eq!(config.imap, None);
        assert_eq!(
            config.smtp.map(|smtp| (smtp.port, smtp.security)),
            Some((587, Security::StartTls))
        );
        assert!(
            config.skipped.iter().any(|note| note.contains("STARTTLS")),
            "{:?}",
            config.skipped
        );
    }

    #[test]
    fn oauth_only_servers_ask_for_oauth() {
        let config =
            parse(include_bytes!("../fixtures/oauth-only.xml"), &email()).expect("fixture parses");
        assert_eq!(config.imap.map(|imap| imap.auth), Some(AuthMethod::OAuth2));
        // NTLM alone is not something zmail can do.
        assert_eq!(config.smtp, None);
    }

    #[test]
    fn rejects_entities_and_dtds() {
        // Billion laughs and an external entity: both need a DTD, which is
        // refused before any entity is defined.
        for fixture in [
            include_bytes!("../fixtures/billion-laughs.xml").as_slice(),
            include_bytes!("../fixtures/external-entity.xml").as_slice(),
        ] {
            assert!(
                matches!(parse(fixture, &email()), Err(ParseError::Xml(_))),
                "a DTD must be rejected"
            );
        }
    }

    #[test]
    fn rejects_oversized_malformed_and_foreign_documents() {
        let huge = vec![b' '; super::MAX_BODY_BYTES + 1];
        assert_eq!(parse(&huge, &email()), Err(ParseError::TooLarge));
        assert_eq!(parse(&[0xff, 0xfe], &email()), Err(ParseError::NotUtf8));
        assert!(matches!(
            parse(b"<clientConfig>", &email()),
            Err(ParseError::Xml(_))
        ));
        assert_eq!(
            parse(b"<html><body>Not here</body></html>", &email()),
            Err(ParseError::NotAutoconfig)
        );
        // Deep or wide documents hit the node limit.
        let wide = format!("<clientConfig>{}</clientConfig>", "<a/>".repeat(6_000));
        assert!(matches!(
            parse(wide.as_bytes(), &email()),
            Err(ParseError::Xml(_))
        ));
    }

    #[test]
    fn rejects_hostile_hosts_and_ports() {
        let config = parse(include_bytes!("../fixtures/hostile-values.xml"), &email())
            .expect("fixture parses");
        assert_eq!(config.imap, None);
        assert_eq!(config.smtp, None);
        assert_eq!(config.skipped.len(), 4, "{:?}", config.skipped);
    }

    #[test]
    fn urls_never_carry_the_address() {
        let urls = domain_urls("example.com")
            .into_iter()
            .chain(ispdb_url("example.com"))
            .map(|url| url.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            urls,
            [
                "https://autoconfig.example.com/mail/config-v1.1.xml",
                "https://example.com/.well-known/autoconfig/mail/config-v1.1.xml",
                "https://autoconfig.thunderbird.net/v1.1/example.com",
            ]
        );
    }
}
