//! Discovery ordering against fake network services.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use url::Url;

use crate::{
    Discovery, DnsLookup, EmailAddress, Fetch, FetchResponse, IspdbConsent, MailSettings, MxRecord,
    Network, PortProbe, Provider, Security, Source, SrvRecord, Step, StepOutcome, discover,
    run_step,
};

const AUTOCONFIG_URL: &str = "https://autoconfig.example.com/mail/config-v1.1.xml";
const WELL_KNOWN_AUTOCONFIG_URL: &str =
    "https://example.com/.well-known/autoconfig/mail/config-v1.1.xml";
const ISPDB_URL: &str = "https://autoconfig.thunderbird.net/v1.1/example.com";
const WELL_KNOWN_JMAP_URL: &str = "https://example.com/.well-known/jmap";

/// Serves canned responses and records every URL asked for. Unknown URLs
/// are 404s.
#[derive(Default)]
struct FakeWeb {
    responses: HashMap<String, Result<FetchResponse, String>>,
    requested: Mutex<Vec<String>>,
}

impl FakeWeb {
    fn serve(mut self, url: &str, status: u16, final_url: &str, body: &[u8]) -> Self {
        let response = FetchResponse {
            status,
            url: Url::parse(final_url).expect("valid URL"),
            body: body.to_vec(),
        };
        self.responses.insert(url.to_owned(), Ok(response));
        self
    }

    fn fail(mut self, url: &str, error: &str) -> Self {
        self.responses.insert(url.to_owned(), Err(error.to_owned()));
        self
    }

    fn requested(&self) -> Vec<String> {
        self.requested.lock().expect("lock").clone()
    }
}

impl Fetch for FakeWeb {
    fn get(&self, url: &Url) -> Result<FetchResponse, String> {
        self.requested.lock().expect("lock").push(url.to_string());
        self.responses
            .get(url.as_str())
            .cloned()
            .unwrap_or_else(|| {
                Ok(FetchResponse {
                    status: 404,
                    url: url.clone(),
                    body: Vec::new(),
                })
            })
    }
}

#[derive(Default)]
struct FakeDns {
    srv: HashMap<String, Vec<SrvRecord>>,
    mx: Vec<MxRecord>,
}

impl DnsLookup for FakeDns {
    fn srv(&self, name: &str) -> Result<Vec<SrvRecord>, String> {
        Ok(self.srv.get(name).cloned().unwrap_or_default())
    }

    fn mx(&self, _: &str) -> Result<Vec<MxRecord>, String> {
        Ok(self.mx.clone())
    }
}

/// Every port is closed unless listed.
#[derive(Default)]
struct FakePorts(Vec<(&'static str, u16)>);

impl PortProbe for FakePorts {
    fn reachable(&self, host: &str, port: u16) -> bool {
        self.0.contains(&(host, port))
    }
}

fn network(web: &Arc<FakeWeb>, dns: Option<FakeDns>, ports: FakePorts) -> Network {
    Network {
        fetch: web.clone(),
        dns: dns.map(|dns| Arc::new(dns) as Arc<dyn DnsLookup>),
        probe: Arc::new(ports),
    }
}

fn email() -> EmailAddress {
    EmailAddress::parse("jane@example.com").expect("valid address")
}

fn srv(target: &str, port: u16) -> Vec<SrvRecord> {
    vec![SrvRecord {
        priority: 0,
        weight: 1,
        port,
        target: target.to_owned(),
    }]
}

fn steps_run(discovery: &Discovery) -> Vec<Step> {
    discovery
        .reports()
        .iter()
        .map(|report| report.step)
        .collect()
}

#[test]
fn without_dns_the_dns_steps_are_left_out() {
    let discovery = Discovery::new(email(), false);
    assert_eq!(
        discovery.steps(),
        [
            Step::DomainAutoconfig,
            Step::WellKnownJmap,
            Step::Ispdb,
            Step::HostnameGuess
        ]
    );
    assert_eq!(Discovery::new(email(), true).steps(), Step::ORDER);
}

#[test]
fn the_domains_own_autoconfig_ends_discovery_before_ispdb() {
    let web = Arc::new(FakeWeb::default().serve(
        AUTOCONFIG_URL,
        200,
        AUTOCONFIG_URL,
        include_bytes!("../fixtures/example-autoconfig.xml"),
    ));
    let discovery = discover(
        email(),
        true,
        &network(&web, None, FakePorts::default()),
        |_| {},
    );
    assert_eq!(steps_run(&discovery), [Step::DomainAutoconfig]);
    let Some(MailSettings::ImapSmtp { imap, smtp }) = discovery.result() else {
        panic!("expected IMAP and SMTP: {:?}", discovery.result());
    };
    let source = Source::DomainAutoconfig {
        url: AUTOCONFIG_URL.to_owned(),
    };
    assert_eq!(imap.source, source);
    assert_eq!(smtp.source, source);
    assert_eq!(web.requested(), [AUTOCONFIG_URL]);
}

#[test]
fn ispdb_waits_for_consent_and_is_skipped_when_declined() {
    let web = Arc::new(FakeWeb::default().serve(
        ISPDB_URL,
        200,
        ISPDB_URL,
        include_bytes!("../fixtures/example-autoconfig.xml"),
    ));
    let network = network(
        &web,
        None,
        FakePorts(vec![("imap.example.com", 993), ("smtp.example.com", 465)]),
    );
    let mut discovery = Discovery::new(email(), false);
    while let Some(step) = discovery.next_step() {
        let result = run_step(step, discovery.email(), &network);
        discovery.record(step, result);
    }
    // Both of the domain's own sources came up empty: stop and ask.
    assert!(discovery.needs_consent());
    assert!(!discovery.is_finished());
    assert_eq!(
        steps_run(&discovery),
        [Step::DomainAutoconfig, Step::WellKnownJmap]
    );
    assert!(
        !web.requested()
            .iter()
            .any(|url| url.contains("thunderbird.net")),
        "ISPDB must not be contacted before consent: {:?}",
        web.requested()
    );

    discovery.set_ispdb_consent(false);
    assert_eq!(discovery.ispdb_consent(), IspdbConsent::Declined);
    assert_eq!(discovery.next_step(), Some(Step::HostnameGuess));
    let result = run_step(Step::HostnameGuess, discovery.email(), &network);
    discovery.record(Step::HostnameGuess, result);
    assert!(discovery.is_finished());
    assert_eq!(
        discovery.reports()[2].outcome,
        StepOutcome::Skipped,
        "the declined ISPDB step shows as skipped"
    );
    let Some(MailSettings::ImapSmtp { imap, smtp }) = discovery.result() else {
        panic!("expected a hostname guess: {:?}", discovery.result());
    };
    assert_eq!(imap.source, Source::HostnameGuess);
    assert_eq!(smtp.value.security, Security::Tls);
    assert!(
        !web.requested()
            .iter()
            .any(|url| url.contains("thunderbird.net"))
    );
}

#[test]
fn ispdb_runs_with_consent() {
    let web = Arc::new(FakeWeb::default().serve(
        ISPDB_URL,
        200,
        ISPDB_URL,
        include_bytes!("../fixtures/example-autoconfig.xml"),
    ));
    let discovery = discover(
        email(),
        true,
        &network(&web, None, FakePorts::default()),
        |_| {},
    );
    assert_eq!(
        steps_run(&discovery),
        [Step::DomainAutoconfig, Step::WellKnownJmap, Step::Ispdb]
    );
    let Some(MailSettings::ImapSmtp { imap, .. }) = discovery.result() else {
        panic!("expected ISPDB settings: {:?}", discovery.result());
    };
    assert_eq!(
        imap.source,
        Source::Ispdb {
            url: ISPDB_URL.to_owned()
        }
    );
    // Only the domain's own URLs and ISPDB were asked, in that order, and
    // no URL carries the address.
    assert_eq!(
        web.requested(),
        [
            AUTOCONFIG_URL,
            WELL_KNOWN_AUTOCONFIG_URL,
            WELL_KNOWN_JMAP_URL,
            ISPDB_URL
        ]
    );
    assert!(!web.requested().iter().any(|url| url.contains("jane")));
}

#[test]
fn well_known_jmap_follows_redirects_to_the_session() {
    let web = Arc::new(FakeWeb::default().serve(
        WELL_KNOWN_JMAP_URL,
        401,
        "https://api.example.com/jmap/session",
        b"",
    ));
    let discovery = discover(
        email(),
        false,
        &network(&web, None, FakePorts::default()),
        |_| {},
    );
    let Some(MailSettings::Jmap(jmap)) = discovery.result() else {
        panic!("expected JMAP: {:?}", discovery.result());
    };
    assert_eq!(
        jmap.value.session_url,
        "https://api.example.com/jmap/session"
    );
    assert_eq!(jmap.value.username, "jane@example.com");
    assert_eq!(
        jmap.source,
        Source::WellKnownJmap {
            url: WELL_KNOWN_JMAP_URL.to_owned()
        }
    );
}

#[test]
fn srv_records_come_first_and_need_no_http() {
    let web = Arc::new(FakeWeb::default());
    let dns = FakeDns {
        srv: HashMap::from([
            (
                "_imaps._tcp.example.com".to_owned(),
                srv("imap.example.net.", 993),
            ),
            (
                "_submission._tcp.example.com".to_owned(),
                srv("smtp.example.net.", 587),
            ),
            ("_submissions._tcp.example.com".to_owned(), srv(".", 0)),
        ]),
        mx: Vec::new(),
    };
    let discovery = discover(
        email(),
        false,
        &network(&web, Some(dns), FakePorts::default()),
        |_| {},
    );
    assert_eq!(steps_run(&discovery), [Step::DnsSrv]);
    let Some(MailSettings::ImapSmtp { imap, smtp }) = discovery.result() else {
        panic!("expected SRV settings: {:?}", discovery.result());
    };
    assert_eq!(
        (imap.value.host.as_str(), imap.value.port),
        ("imap.example.net", 993)
    );
    // `_submissions` says "not offered", so STARTTLS submission is used.
    assert_eq!(smtp.value.security, Security::StartTls);
    assert_eq!(
        smtp.source,
        Source::DnsSrv {
            record: "_submission._tcp.example.com".to_owned()
        }
    );
    assert!(web.requested().is_empty());
}

#[test]
fn partial_results_merge_with_each_servers_source() {
    // SRV finds IMAP only; the autoconfig file supplies SMTP. The SRV IMAP
    // server wins because it came first.
    let web = Arc::new(FakeWeb::default().serve(
        AUTOCONFIG_URL,
        200,
        AUTOCONFIG_URL,
        include_bytes!("../fixtures/example-autoconfig.xml"),
    ));
    let dns = FakeDns {
        srv: HashMap::from([(
            "_imaps._tcp.example.com".to_owned(),
            srv("imap.example.net", 993),
        )]),
        mx: Vec::new(),
    };
    let discovery = discover(
        email(),
        false,
        &network(&web, Some(dns), FakePorts::default()),
        |_| {},
    );
    let Some(MailSettings::ImapSmtp { imap, smtp }) = discovery.result() else {
        panic!("expected merged settings: {:?}", discovery.result());
    };
    assert_eq!(imap.value.host, "imap.example.net");
    assert!(matches!(imap.source, Source::DnsSrv { .. }));
    assert_eq!(smtp.value.host, "smtp.example.com");
    assert!(matches!(smtp.source, Source::DomainAutoconfig { .. }));
}

#[test]
fn mx_guess_spots_hosted_domains_after_ispdb() {
    let web = Arc::new(FakeWeb::default());
    let dns = FakeDns {
        srv: HashMap::new(),
        mx: vec![
            MxRecord {
                preference: 10,
                exchange: "alt1.aspmx.l.google.com.".to_owned(),
            },
            MxRecord {
                preference: 1,
                exchange: "aspmx.l.google.com.".to_owned(),
            },
        ],
    };
    let mut started = Vec::new();
    let discovery = discover(
        email(),
        false,
        &network(&web, Some(dns), FakePorts::default()),
        |step| started.push(step),
    );
    assert_eq!(
        started,
        [
            Step::DnsSrv,
            Step::DomainAutoconfig,
            Step::WellKnownJmap,
            Step::MxGuess
        ],
        "ISPDB was declined, so MX follows the domain's own sources"
    );
    let Some(MailSettings::ImapSmtp { imap, .. }) = discovery.result() else {
        panic!("expected Google settings: {:?}", discovery.result());
    };
    assert_eq!(imap.value.host, "imap.gmail.com");
    assert_eq!(
        imap.source,
        Source::MxGuess {
            mx_host: "aspmx.l.google.com".to_owned(),
            provider: Provider::Google,
        }
    );
}

#[test]
fn a_failing_step_is_reported_and_discovery_moves_on() {
    let web = Arc::new(
        FakeWeb::default()
            .fail(AUTOCONFIG_URL, "connection timed out")
            .fail(WELL_KNOWN_AUTOCONFIG_URL, "certificate not valid for name"),
    );
    let discovery = discover(
        email(),
        false,
        &network(&web, None, FakePorts::default()),
        |_| {},
    );
    assert!(discovery.result().is_none());
    assert!(discovery.is_finished());
    let StepOutcome::Failed(error) = &discovery.reports()[0].outcome else {
        panic!("expected a failure: {:?}", discovery.reports());
    };
    assert!(
        error.contains("timed out") && error.contains("certificate"),
        "{error}"
    );
    assert_eq!(
        steps_run(&discovery),
        [
            Step::DomainAutoconfig,
            Step::WellKnownJmap,
            Step::Ispdb,
            Step::HostnameGuess
        ]
    );
}

#[test]
fn a_starttls_only_imap_server_is_explained() {
    let web = Arc::new(FakeWeb::default().serve(
        AUTOCONFIG_URL,
        200,
        AUTOCONFIG_URL,
        include_bytes!("../fixtures/starttls-only.xml"),
    ));
    let discovery = discover(
        email(),
        false,
        &network(&web, None, FakePorts::default()),
        |_| {},
    );
    // SMTP alone isn't a result, but the manual form gets it.
    assert!(discovery.result().is_none());
    let (imap, smtp) = discovery.partial();
    assert!(imap.is_none());
    assert_eq!(smtp.map(|smtp| smtp.value.port), Some(587));
    let StepOutcome::Found { note: Some(note) } = &discovery.reports()[0].outcome else {
        panic!("the SMTP server counts as found: {:?}", discovery.reports());
    };
    assert!(note.contains("STARTTLS"), "{note}");
}
