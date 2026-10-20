//! Account autodiscovery: from an email address to IMAP/SMTP or JMAP server
//! settings, without the password.
//!
//! Discovery runs a fixed sequence of [`Step`]s and stops as soon as it has
//! a complete configuration (a JMAP session, or an IMAP server and an SMTP
//! submission server). Each setting it returns carries the [`Source`] it
//! came from, so the UI can say where the settings were found.
//!
//! | Step | Asks | Notes |
//! |---|---|---|
//! | [`Step::DnsSrv`] | the system resolver | RFC 6186 / 8314 / 8620 SRV records |
//! | [`Step::DomainAutoconfig`] | the mail domain itself | `autoconfig.<domain>` and `<domain>/.well-known/autoconfig` |
//! | [`Step::WellKnownJmap`] | the mail domain itself | RFC 8620 `/.well-known/jmap` |
//! | [`Step::Ispdb`] | Mozilla (`autoconfig.thunderbird.net`) | only after [`Discovery::set_ispdb_consent`] |
//! | [`Step::MxGuess`] | the system resolver | known hosted providers by MX |
//! | [`Step::HostnameGuess`] | the mail domain itself | TCP probes of `imap.`/`smtp.`/`mail.<domain>` |
//!
//! Privacy and safety rules every step follows:
//!
//! - Discovery never sees the password, so it can never send it.
//! - HTTP is HTTPS only (redirects too), with timeouts and a size cap; see
//!   [`fetch`].
//! - Autoconfig XML is untrusted: no DTDs (so no entities, external or
//!   otherwise), a node limit, and strict host and port checks; see
//!   [`autoconfig`].
//! - Only the ISPDB step sends the domain to a third party, and only with
//!   consent. DNS goes to the system's own resolvers ([`SystemDns`]); there
//!   is no DNS-over-HTTPS fallback, which would leak the domain too.
//! - A step that fails (a timeout, a DNS error, a TLS error) never stops
//!   discovery; the next step runs.
//! - Plain-text servers are never offered. IMAP over STARTTLS is skipped
//!   too, because zmail's IMAP client only speaks implicit TLS.
//!
//! [`Discovery`] keeps the state between steps so the UI can run each step
//! off the main thread, show progress, and stop to ask for ISPDB consent;
//! [`discover`] runs the whole sequence in one blocking call.

use std::{fmt, sync::Arc};

pub mod address;
pub mod autoconfig;
pub mod dns;
pub mod fetch;
pub mod guess;
pub mod providers;

pub use address::{AddressError, EmailAddress};
pub use dns::{DnsLookup, MxRecord, SrvRecord, SystemDns};
pub use fetch::{Fetch, FetchResponse, HttpsFetcher};
pub use guess::{PortProbe, TcpProbe};
pub use providers::Provider;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// How a connection to a mail server is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Security {
    /// Cleartext. Discovery never returns it; it exists for the manual
    /// form, which warns before using it.
    None,
    /// A cleartext connection upgraded with STARTTLS.
    StartTls,
    /// TLS from the first byte (implicit TLS, "SSL").
    Tls,
}

impl Security {
    /// The label the account form shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::StartTls => "STARTTLS",
            Self::Tls => "TLS",
        }
    }
}

/// How zmail should sign in to a discovered server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMethod {
    /// A password (or app password).
    Password,
    /// OAuth2 (XOAUTH2 or a bearer token); the server offered no password
    /// method.
    OAuth2,
}

/// One discovered IMAP or SMTP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettings {
    /// The server's host name.
    pub host: String,
    /// The TCP port.
    pub port: u16,
    /// How the connection is secured.
    pub security: Security,
    /// The user name to sign in with (usually the email address).
    pub username: String,
    /// How to sign in.
    pub auth: AuthMethod,
}

/// A discovered JMAP account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmapSettings {
    /// The JMAP session URL (after following the domain's redirects).
    pub session_url: String,
    /// The user name to sign in with.
    pub username: String,
}

/// Where a discovered setting came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A built-in provider preset (the provider buttons).
    Provider(Provider),
    /// A DNS SRV record (RFC 6186, RFC 8314, RFC 8620).
    DnsSrv {
        /// The record's owner name, such as `_imaps._tcp.example.com`.
        record: String,
    },
    /// The mail domain's own autoconfig file.
    DomainAutoconfig {
        /// The URL the file was read from.
        url: String,
    },
    /// The mail domain's `/.well-known/jmap`.
    WellKnownJmap {
        /// The URL that was probed.
        url: String,
    },
    /// Mozilla's ISPDB.
    Ispdb {
        /// The URL the configuration was read from.
        url: String,
    },
    /// The domain's MX record points at a known hosted provider.
    MxGuess {
        /// The MX host that matched.
        mx_host: String,
        /// The provider it matched.
        provider: Provider,
    },
    /// A common server name answered on the standard port. Only a guess.
    HostnameGuess,
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(provider) => write!(formatter, "{} settings", provider.name()),
            Self::DnsSrv { record } => write!(formatter, "DNS record {record}"),
            Self::DomainAutoconfig { url } => {
                write!(formatter, "your provider's setup file ({url})")
            }
            Self::WellKnownJmap { url } => {
                write!(formatter, "your provider's JMAP address ({url})")
            }
            Self::Ispdb { .. } => formatter.write_str("Mozilla's provider database"),
            Self::MxGuess { mx_host, provider } => {
                write!(
                    formatter,
                    "mail for this domain goes to {provider} ({mx_host})"
                )
            }
            Self::HostnameGuess => formatter.write_str("a guess from common server names"),
        }
    }
}

/// A discovered value and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<T> {
    /// The value.
    pub value: T,
    /// Where it came from.
    pub source: Source,
}

impl<T> Found<T> {
    /// Pairs `value` with its `source`.
    pub fn new(value: T, source: Source) -> Self {
        Self { value, source }
    }
}

/// A complete configuration discovery found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailSettings {
    /// A JMAP account.
    Jmap(Found<JmapSettings>),
    /// An IMAP account with SMTP submission.
    ImapSmtp {
        /// The IMAP server.
        imap: Found<ServerSettings>,
        /// The SMTP submission server.
        smtp: Found<ServerSettings>,
    },
}

// ---------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------

/// One discovery step, in the order discovery runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    /// SRV records for JMAP, IMAP and submission.
    DnsSrv,
    /// The domain's own autoconfig file.
    DomainAutoconfig,
    /// The domain's `/.well-known/jmap`.
    WellKnownJmap,
    /// Mozilla's ISPDB; needs consent.
    Ispdb,
    /// Known hosted providers by MX record.
    MxGuess,
    /// Probes of common server names.
    HostnameGuess,
}

impl Step {
    /// Every step, in order.
    pub const ORDER: [Step; 6] = [
        Step::DnsSrv,
        Step::DomainAutoconfig,
        Step::WellKnownJmap,
        Step::Ispdb,
        Step::MxGuess,
        Step::HostnameGuess,
    ];

    /// What the step is doing, for a progress list.
    pub fn label(self) -> &'static str {
        match self {
            Self::DnsSrv => "Checking DNS for mail services",
            Self::DomainAutoconfig => "Asking your provider for its settings",
            Self::WellKnownJmap => "Checking for JMAP",
            Self::Ispdb => "Checking Mozilla's provider database",
            Self::MxGuess => "Checking where your domain's mail goes",
            Self::HostnameGuess => "Trying common server names",
        }
    }

    /// Whether the step needs DNS lookups that std can't do (SRV, MX).
    fn needs_dns(self) -> bool {
        matches!(self, Self::DnsSrv | Self::MxGuess)
    }
}

/// What one step found. Fields a step can't find stay `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepFindings {
    /// A JMAP session.
    pub jmap: Option<Found<JmapSettings>>,
    /// An IMAP server.
    pub imap: Option<Found<ServerSettings>>,
    /// An SMTP submission server.
    pub smtp: Option<Found<ServerSettings>>,
    /// Why something found was not used, such as a server that only offers
    /// cleartext.
    pub note: Option<String>,
}

impl StepFindings {
    fn is_empty(&self) -> bool {
        self.jmap.is_none() && self.imap.is_none() && self.smtp.is_none()
    }
}

/// The result of running one step: what it found, or why it failed.
pub type StepResult = Result<StepFindings, String>;

/// How a step went, for the progress list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// It found at least one server; `note` says why others it saw were
    /// not used.
    Found {
        /// Why servers it saw were not used.
        note: Option<String>,
    },
    /// It found nothing usable (`note` says why, when known).
    NothingFound {
        /// Why something found was not usable.
        note: Option<String>,
    },
    /// It failed (a timeout, a TLS error, malformed XML).
    Failed(String),
    /// It was not run: the user declined the ISPDB lookup.
    Skipped,
}

/// One finished step and how it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    /// The step.
    pub step: Step,
    /// How it went.
    pub outcome: StepOutcome,
}

/// Whether the user allowed the ISPDB lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IspdbConsent {
    /// Not asked yet. Discovery stops before ISPDB until it is answered.
    Undecided,
    /// The user allowed it.
    Granted,
    /// The user declined it; ISPDB is skipped.
    Declined,
}

/// What discovery talks to. Tests swap in fakes.
#[derive(Clone)]
pub struct Network {
    /// HTTPS fetches.
    pub fetch: Arc<dyn Fetch>,
    /// SRV and MX lookups; `None` skips the DNS steps.
    pub dns: Option<Arc<dyn DnsLookup>>,
    /// TCP reachability probes for the hostname guess.
    pub probe: Arc<dyn PortProbe>,
}

impl Network {
    /// The real network: HTTPS through [`HttpsFetcher`], SRV and MX
    /// lookups through [`SystemDns`], TCP probes through [`TcpProbe`].
    /// Nothing is set up until a step first needs it.
    pub fn system() -> Self {
        Self {
            fetch: Arc::new(HttpsFetcher::new()),
            dns: Some(Arc::new(SystemDns::new())),
            probe: Arc::new(TcpProbe::default()),
        }
    }
}

/// Runs one step for `email`. Blocks on the network; call it off the UI
/// thread.
pub fn run_step(step: Step, email: &EmailAddress, network: &Network) -> StepResult {
    match step {
        Step::DnsSrv => match &network.dns {
            Some(dns) => dns::srv_step(email, dns.as_ref()),
            None => Ok(StepFindings::default()),
        },
        Step::DomainAutoconfig => autoconfig::domain_step(email, network.fetch.as_ref()),
        Step::WellKnownJmap => fetch::well_known_jmap_step(email, network.fetch.as_ref()),
        Step::Ispdb => autoconfig::ispdb_step(email, network.fetch.as_ref()),
        Step::MxGuess => match &network.dns {
            Some(dns) => dns::mx_step(email, dns.as_ref()),
            None => Ok(StepFindings::default()),
        },
        Step::HostnameGuess => guess::hostname_step(email, network.probe.as_ref()),
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Discovery for one address: which steps ran, what they found, and what
/// comes next.
#[derive(Debug, Clone)]
pub struct Discovery {
    email: EmailAddress,
    dns_available: bool,
    consent: IspdbConsent,
    reports: Vec<StepReport>,
    jmap: Option<Found<JmapSettings>>,
    imap: Option<Found<ServerSettings>>,
    smtp: Option<Found<ServerSettings>>,
}

impl Discovery {
    /// Starts discovery for `email`. `dns_available` says whether the
    /// network has a [`DnsLookup`]; without one the SRV and MX steps are
    /// left out.
    pub fn new(email: EmailAddress, dns_available: bool) -> Self {
        Self {
            email,
            dns_available,
            consent: IspdbConsent::Undecided,
            reports: Vec::new(),
            jmap: None,
            imap: None,
            smtp: None,
        }
    }

    /// The address being discovered.
    pub fn email(&self) -> &EmailAddress {
        &self.email
    }

    /// The steps this discovery runs, in order.
    pub fn steps(&self) -> Vec<Step> {
        Step::ORDER
            .into_iter()
            .filter(|step| self.dns_available || !step.needs_dns())
            .collect()
    }

    /// The finished steps and how each went.
    pub fn reports(&self) -> &[StepReport] {
        &self.reports
    }

    /// The ISPDB consent so far.
    pub fn ispdb_consent(&self) -> IspdbConsent {
        self.consent
    }

    /// The step to run next, or `None` when discovery has a result, has
    /// run out of steps, or is waiting for [`Self::needs_consent`].
    pub fn next_step(&self) -> Option<Step> {
        if self.result().is_some() {
            return None;
        }
        let step = self
            .steps()
            .into_iter()
            .find(|step| !self.reports.iter().any(|report| report.step == *step))?;
        match (step, self.consent) {
            (Step::Ispdb, IspdbConsent::Undecided) => None,
            _ => Some(step),
        }
    }

    /// Whether discovery is waiting for the user to allow or decline the
    /// ISPDB lookup: every step before it found nothing complete.
    pub fn needs_consent(&self) -> bool {
        self.consent == IspdbConsent::Undecided
            && self.result().is_none()
            && self
                .steps()
                .into_iter()
                .take_while(|step| *step != Step::Ispdb)
                .all(|step| self.reports.iter().any(|report| report.step == step))
    }

    /// Records the user's answer. Declining marks the ISPDB step skipped.
    pub fn set_ispdb_consent(&mut self, granted: bool) {
        self.consent = if granted {
            IspdbConsent::Granted
        } else {
            IspdbConsent::Declined
        };
        if !granted && !self.reports.iter().any(|report| report.step == Step::Ispdb) {
            self.reports.push(StepReport {
                step: Step::Ispdb,
                outcome: StepOutcome::Skipped,
            });
        }
    }

    /// Records what `step` found. Earlier findings win: a later step only
    /// fills in what is still missing.
    pub fn record(&mut self, step: Step, result: StepResult) {
        let outcome = match result {
            Ok(findings) if findings.is_empty() => StepOutcome::NothingFound {
                note: findings.note,
            },
            Ok(findings) => {
                self.jmap = self.jmap.take().or(findings.jmap);
                self.imap = self.imap.take().or(findings.imap);
                self.smtp = self.smtp.take().or(findings.smtp);
                StepOutcome::Found {
                    note: findings.note,
                }
            }
            Err(error) => StepOutcome::Failed(error),
        };
        self.reports.push(StepReport { step, outcome });
    }

    /// The configuration found so far, once it is complete. A JMAP session
    /// wins over IMAP and SMTP found by the same or a later step.
    pub fn result(&self) -> Option<MailSettings> {
        if let Some(jmap) = &self.jmap {
            return Some(MailSettings::Jmap(jmap.clone()));
        }
        match (&self.imap, &self.smtp) {
            (Some(imap), Some(smtp)) => Some(MailSettings::ImapSmtp {
                imap: imap.clone(),
                smtp: smtp.clone(),
            }),
            _ => None,
        }
    }

    /// What was found even when it is incomplete (an IMAP server without
    /// SMTP, say), to pre-fill the manual form.
    pub fn partial(
        &self,
    ) -> (
        Option<&Found<ServerSettings>>,
        Option<&Found<ServerSettings>>,
    ) {
        (self.imap.as_ref(), self.smtp.as_ref())
    }

    /// Whether discovery is over: it has a result, or every step it may
    /// run has run.
    pub fn is_finished(&self) -> bool {
        self.next_step().is_none() && !self.needs_consent()
    }
}

/// Runs discovery to the end in one blocking call. `consent` answers the
/// ISPDB question up front; `on_step` is called as each step starts.
pub fn discover(
    email: EmailAddress,
    consent: bool,
    network: &Network,
    mut on_step: impl FnMut(Step),
) -> Discovery {
    let mut discovery = Discovery::new(email, network.dns.is_some());
    loop {
        if discovery.needs_consent() {
            discovery.set_ispdb_consent(consent);
        }
        let Some(step) = discovery.next_step() else {
            return discovery;
        };
        on_step(step);
        let result = run_step(step, discovery.email(), network);
        discovery.record(step, result);
    }
}

#[cfg(test)]
mod tests;
