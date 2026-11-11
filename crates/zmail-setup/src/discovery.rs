//! Driving autodiscovery one step at a time, and how its progress reads.
//!
//! `zmail_autodiscover::Discovery` keeps the state between steps; a
//! frontend runs each step off its main thread (`zmail_autodiscover::run_step`)
//! and asks [`next`] what to do after recording it: run another step, stop
//! to ask before the ISPDB lookup (which sends the domain to Mozilla), or
//! finish. [`finish`] then fills the form from what was found: the result,
//! or the manual form with whatever was found and the usual guesses.
//!
//! Everything a server or DNS returned (hosts, notes, errors, URLs) is
//! untrusted text. A terminal frontend filters it before drawing it
//! (`zmail_security::terminal::sanitize`).

use zmail_autodiscover::{Discovery, MailSettings, ServerSettings, Source, Step};

use crate::form::AccountFormValues;

/// What discovery needs next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Run this step.
    Run(Step),
    /// Ask whether Zmail may look the domain up in Mozilla's ISPDB, then
    /// record the answer with `Discovery::set_ispdb_consent`.
    AskConsent,
    /// Discovery is over: call [`finish`].
    Finished,
}

/// What discovery does after the steps recorded so far.
pub fn next(discovery: &Discovery) -> Next {
    if discovery.needs_consent() {
        return Next::AskConsent;
    }
    match discovery.next_step() {
        Some(step) => Next::Run(step),
        None => Next::Finished,
    }
}

/// How discovery ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A complete configuration, now in the form: show it for checking.
    Found(MailSettings),
    /// Nothing complete: the form has what was found and the usual
    /// guesses for the domain, for the manual form.
    Manual,
}

/// Fills `form` from a finished discovery and says which step comes next.
pub fn finish(discovery: &Discovery, form: &mut AccountFormValues) -> Outcome {
    match discovery.result() {
        Some(settings) => {
            form.apply_settings(&settings);
            Outcome::Found(settings)
        }
        None => {
            let (imap, smtp) = discovery.partial();
            if let Some(imap) = imap {
                form.apply_imap(&imap.value);
            }
            if let Some(smtp) = smtp {
                form.apply_smtp(&smtp.value);
            }
            form.prefill_guesses(discovery.email());
            Outcome::Manual
        }
    }
}

/// Whether any of the settings is only a guess from common server names,
/// which nothing confirmed.
pub fn is_guess(settings: &MailSettings) -> bool {
    match settings {
        MailSettings::Jmap(jmap) => jmap.source == Source::HostnameGuess,
        MailSettings::ImapSmtp { imap, smtp } => [&imap.source, &smtp.source]
            .into_iter()
            .any(|source| *source == Source::HostnameGuess),
    }
}

/// The progress list's line for a step.
pub fn step_label(step: Step, domain: &str) -> String {
    match step {
        Step::DnsSrv => format!("Checking DNS records for {domain}"),
        Step::DomainAutoconfig => format!("Asking {domain} for its settings"),
        Step::WellKnownJmap => format!("Checking {domain} for JMAP"),
        Step::Ispdb => "Checking Mozilla's provider database".to_owned(),
        Step::MxGuess => format!("Checking where mail for {domain} goes"),
        Step::HostnameGuess => "Trying the usual server names".to_owned(),
    }
}

/// What the progress list says under a step the user declined.
pub const SKIPPED_NOTE: &str = "Skipped: you chose not to ask Mozilla";

/// What the consent question explains, for `domain`.
pub fn consent_explanation(domain: &str) -> String {
    format!(
        "To look them up, zmail sends your mail domain, {domain}, to Mozilla at autoconfig.thunderbird.net. It doesn't send your address or your password."
    )
}

/// The last, most specific part of a network error, which is the part a
/// person can act on ("connection refused", "certificate not valid").
pub fn short_error(error: &str) -> String {
    let first = error.split("; ").next().unwrap_or(error);
    let last = first.rsplit(": ").next().unwrap_or(first).trim();
    let mut short = last.to_owned();
    if let Some(first) = short.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    short
}

/// How a server is described: `imap.example.com:993 · TLS`.
pub fn describe_server(server: &ServerSettings) -> String {
    format!(
        "{}:{} · {}",
        server.host,
        server.port,
        server.security.label()
    )
}

/// Where settings came from, as a sentence ending.
pub fn describe_source(source: &Source) -> String {
    match source {
        Source::Provider(provider) => format!("zmail's built-in {provider} settings"),
        Source::DnsSrv {
            record,
            outside_domain: true,
        } => {
            // `_imaps._tcp.example.com` is a record of `example.com`.
            let domain = record.splitn(3, '.').nth(2).unwrap_or(record);
            format!(
                "DNS record {record}, which points outside {domain}. Check that this server is your provider's before you sign in"
            )
        }
        other => other.to_string(),
    }
}

/// Whether the user has to confirm the servers before signing in: a DNS
/// record sent them outside the mail domain (RFC 6186 section 6).
pub fn needs_confirmation(settings: &MailSettings) -> bool {
    !settings.unconfirmed_hosts().is_empty()
}

/// What to ask before signing in to servers a DNS record found outside
/// `domain`, naming them; `None` when nothing needs confirming.
///
/// DNS answers are not protected, so on an untrusted network they can be
/// forged to point at a server that has a valid certificate of its own and
/// would receive the password. Many domains do point at a hosting
/// provider's servers, so these are offered, but only once confirmed.
pub fn confirmation(settings: &MailSettings, domain: &str) -> Option<String> {
    let hosts = settings.unconfirmed_hosts();
    if hosts.is_empty() {
        return None;
    }
    let hosts = hosts.join(" and ");
    Some(format!(
        "The DNS records for {domain} point to {hosts}, outside {domain}. DNS answers can be forged on an untrusted network to send your password elsewhere, so continue only if {hosts} is where your mail provider says your mail is."
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use url::Url;
    use zmail_autodiscover::{
        Discovery, DnsLookup, EmailAddress, Fetch, FetchResponse, MailSettings, MxRecord, Network,
        PortProbe, Provider, Security, SrvRecord, Step,
    };

    use super::{
        Next, Outcome, confirmation, describe_source, finish, is_guess, needs_confirmation, next,
        short_error,
    };
    use crate::form::AccountFormValues;

    const AUTOCONFIG_URL: &str = "https://autoconfig.example.test/mail/config-v1.1.xml";

    /// Serves one autoconfig file, or nothing.
    struct FakeWeb(Option<&'static str>);

    impl Fetch for FakeWeb {
        fn get(&self, url: &Url) -> Result<FetchResponse, String> {
            let body = match self.0 {
                Some(body) if url.as_str() == AUTOCONFIG_URL => body.as_bytes().to_vec(),
                _ => {
                    return Ok(FetchResponse {
                        status: 404,
                        url: url.clone(),
                        body: Vec::new(),
                    });
                }
            };
            Ok(FetchResponse {
                status: 200,
                url: url.clone(),
                body,
            })
        }
    }

    /// No SRV records; an MX record pointing at Google.
    struct GoogleMx;

    impl DnsLookup for GoogleMx {
        fn srv(&self, _: &str) -> Result<Vec<SrvRecord>, String> {
            Ok(Vec::new())
        }

        fn mx(&self, _: &str) -> Result<Vec<MxRecord>, String> {
            Ok(vec![MxRecord {
                preference: 10,
                exchange: "aspmx.l.google.com.".to_owned(),
            }])
        }
    }

    /// Only these ports answer.
    struct Ports(Vec<(&'static str, u16)>);

    impl PortProbe for Ports {
        fn reachable(&self, host: &str, port: u16) -> bool {
            self.0.contains(&(host, port))
        }
    }

    fn email() -> EmailAddress {
        EmailAddress::parse("jane@example.test").expect("a valid address")
    }

    /// Runs discovery the way a frontend does, answering the ISPDB
    /// question with `consent`, and returns the steps run and how often
    /// it was asked.
    fn drive(network: &Network, consent: bool) -> (Discovery, Vec<Step>, usize) {
        let mut discovery = Discovery::new(email(), network.dns.is_some());
        let (mut ran, mut asked) = (Vec::new(), 0);
        loop {
            match next(&discovery) {
                Next::Run(step) => {
                    ran.push(step);
                    let result = zmail_autodiscover::run_step(step, discovery.email(), network);
                    discovery.record(step, result);
                }
                Next::AskConsent => {
                    asked += 1;
                    discovery.set_ispdb_consent(consent);
                }
                Next::Finished => return (discovery, ran, asked),
            }
        }
    }

    const AUTOCONFIG: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1"><emailProvider id="example.test">
  <incomingServer type="imap"><hostname>imap.example.test</hostname><port>993</port>
    <socketType>SSL</socketType><authentication>password-cleartext</authentication>
    <username>%EMAILADDRESS%</username></incomingServer>
  <outgoingServer type="smtp"><hostname>smtp.example.test</hostname><port>465</port>
    <socketType>SSL</socketType><authentication>password-cleartext</authentication>
    <username>%EMAILADDRESS%</username></outgoingServer>
</emailProvider></clientConfig>"#;

    #[test]
    fn the_domains_own_file_ends_discovery_and_fills_the_form() {
        let network = Network {
            fetch: Arc::new(FakeWeb(Some(AUTOCONFIG))),
            dns: None,
            probe: Arc::new(Ports(Vec::new())),
        };
        let (discovery, ran, asked) = drive(&network, false);
        assert_eq!(ran, [Step::DomainAutoconfig]);
        assert_eq!(asked, 0, "no need to ask Mozilla");
        let mut form = AccountFormValues::default();
        let Outcome::Found(settings) = finish(&discovery, &mut form) else {
            panic!("settings were found");
        };
        assert!(!is_guess(&settings));
        assert_eq!(form.imap_host, "imap.example.test");
        assert_eq!(form.smtp_port, "465");
        assert_eq!(form.imap_username, "jane@example.test");
    }

    #[test]
    fn discovery_stops_to_ask_before_ispdb_then_guesses() {
        let network = Network {
            fetch: Arc::new(FakeWeb(None)),
            dns: None,
            probe: Arc::new(Ports(vec![
                ("imap.example.test", 993),
                ("smtp.example.test", 465),
            ])),
        };
        let (discovery, ran, asked) = drive(&network, false);
        assert_eq!(asked, 1, "asked once");
        assert_eq!(
            ran,
            [
                Step::DomainAutoconfig,
                Step::WellKnownJmap,
                Step::HostnameGuess
            ],
            "declining skips ISPDB"
        );
        let mut form = AccountFormValues::default();
        let Outcome::Found(settings) = finish(&discovery, &mut form) else {
            panic!("the guess answered");
        };
        assert!(is_guess(&settings), "only a guess");
    }

    #[test]
    fn mx_records_find_a_hosted_provider() {
        let network = Network {
            fetch: Arc::new(FakeWeb(None)),
            dns: Some(Arc::new(GoogleMx)),
            probe: Arc::new(Ports(Vec::new())),
        };
        let (discovery, ran, _) = drive(&network, false);
        assert_eq!(ran.last(), Some(&Step::MxGuess));
        let Some(MailSettings::ImapSmtp { imap, .. }) = discovery.result() else {
            panic!("Google's servers");
        };
        assert_eq!(imap.value.host, "imap.gmail.com");
        assert!(matches!(
            imap.source,
            zmail_autodiscover::Source::MxGuess {
                provider: Provider::Google,
                ..
            }
        ));
    }

    #[test]
    fn nothing_found_leaves_the_manual_form_with_guesses() {
        let network = Network {
            fetch: Arc::new(FakeWeb(None)),
            dns: None,
            probe: Arc::new(Ports(Vec::new())),
        };
        let (discovery, _, _) = drive(&network, true);
        let mut form = AccountFormValues::default();
        assert_eq!(finish(&discovery, &mut form), Outcome::Manual);
        assert_eq!(form.imap_host, "imap.example.test");
        assert_eq!(form.smtp_host, "smtp.example.test");
        assert_eq!(form.imap_security, Security::Tls, "TLS stays the default");
    }

    /// SRV records sending example.test's mail to a hosting provider.
    struct HostedSrv;

    impl DnsLookup for HostedSrv {
        fn srv(&self, name: &str) -> Result<Vec<SrvRecord>, String> {
            let target = match name {
                "_imaps._tcp.example.test" => ("mail.provider.test.", 993),
                "_submissions._tcp.example.test" => ("mail.provider.test.", 465),
                _ => return Ok(Vec::new()),
            };
            Ok(vec![SrvRecord {
                priority: 0,
                weight: 0,
                port: target.1,
                target: target.0.to_owned(),
            }])
        }

        fn mx(&self, _: &str) -> Result<Vec<MxRecord>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn a_dns_record_outside_the_domain_is_named_and_needs_confirming() {
        let network = Network {
            fetch: Arc::new(FakeWeb(None)),
            dns: Some(Arc::new(HostedSrv)),
            probe: Arc::new(Ports(Vec::new())),
        };
        let (discovery, ran, asked) = drive(&network, false);
        assert_eq!(
            ran,
            [Step::DnsSrv, Step::DomainAutoconfig, Step::WellKnownJmap],
            "the domain's own HTTPS sources are asked first"
        );
        assert_eq!(asked, 0, "the DNS answer is enough to stop before Mozilla");
        let mut form = AccountFormValues::default();
        let Outcome::Found(settings) = finish(&discovery, &mut form) else {
            panic!("the hosting provider's servers are offered");
        };
        assert_eq!(form.imap_host, "mail.provider.test");
        assert!(needs_confirmation(&settings));
        let sentence = confirmation(&settings, "example.test").expect("a question");
        assert!(sentence.contains("mail.provider.test"), "{sentence}");
        assert!(sentence.contains("example.test"), "{sentence}");
        assert_eq!(
            sentence.matches("mail.provider.test").count(),
            2,
            "named once per mention, not once per server: {sentence}"
        );
        let MailSettings::ImapSmtp { imap, .. } = &settings else {
            panic!("IMAP settings");
        };
        let source = describe_source(&imap.source);
        assert!(
            source.contains("outside example.test") && source.contains("your provider's"),
            "{source}"
        );
    }

    #[test]
    fn the_domains_own_file_wins_over_a_dns_record_outside_it() {
        let network = Network {
            fetch: Arc::new(FakeWeb(Some(AUTOCONFIG))),
            dns: Some(Arc::new(HostedSrv)),
            probe: Arc::new(Ports(Vec::new())),
        };
        let (discovery, ran, _) = drive(&network, false);
        assert_eq!(ran, [Step::DnsSrv, Step::DomainAutoconfig]);
        let mut form = AccountFormValues::default();
        let Outcome::Found(settings) = finish(&discovery, &mut form) else {
            panic!("settings were found");
        };
        assert_eq!(form.imap_host, "imap.example.test");
        assert!(!needs_confirmation(&settings));
        assert_eq!(confirmation(&settings, "example.test"), None);
    }

    #[test]
    fn network_errors_are_shortened_to_their_cause() {
        assert_eq!(
            short_error(
                "error sending request for url (https://autoconfig.example.com/mail/config-v1.1.xml): client error (Connect): dns error: no record found; other"
            ),
            "No record found"
        );
        assert_eq!(short_error("timed out"), "Timed out");
    }
}
