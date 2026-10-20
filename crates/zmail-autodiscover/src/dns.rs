//! The DNS steps: SRV records (RFC 6186, RFC 8314, RFC 8620) and the MX
//! guess.
//!
//! std can resolve host names but not SRV or MX records. [`SystemDns`] does
//! both with hickory-resolver, through the operating system's own resolver
//! configuration (resolv.conf on Linux, the dynamic store on macOS, the
//! network adapters on Windows), so queries go where every other lookup on
//! the machine goes. Lookups are short (two tries of two seconds) and a
//! failed one never stops discovery: the step reports nothing found, with
//! the reason, and the next step runs.

use std::{sync::OnceLock, time::Duration};

use hickory_resolver::{TokioResolver, proto::rr::RData};

use crate::address::is_dns_name;
use crate::providers::Provider;
use crate::{
    AuthMethod, EmailAddress, Found, JmapSettings, Security, ServerSettings, Source, StepFindings,
    StepResult,
};

/// An SRV record's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrvRecord {
    /// Lower is tried first.
    pub priority: u16,
    /// Among equal priorities, higher is preferred.
    pub weight: u16,
    /// The service's port.
    pub port: u16,
    /// The service's host; `.` means the service is not offered.
    pub target: String,
}

/// An MX record's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxRecord {
    /// Lower is preferred.
    pub preference: u16,
    /// The mail exchanger's host.
    pub exchange: String,
}

/// SRV and MX lookups through the system's resolver. Implementations must
/// not send queries to a third party (no DNS-over-HTTPS to a public
/// resolver): that would leak the mail domain the same way ISPDB does.
pub trait DnsLookup: Send + Sync {
    /// The SRV records at `name`; empty when there are none.
    fn srv(&self, name: &str) -> Result<Vec<SrvRecord>, String>;
    /// The MX records for `domain`; empty when there are none.
    fn mx(&self, domain: &str) -> Result<Vec<MxRecord>, String>;
}

/// How long one query waits for an answer before it is tried again.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// How many times a query is tried.
const QUERY_ATTEMPTS: usize = 2;
/// The most one lookup may take, retries and TCP fallback included.
const LOOKUP_DEADLINE: Duration = Duration::from_secs(5);

/// The real [`DnsLookup`]: hickory-resolver with the system's resolver
/// configuration, on a small Tokio runtime of its own.
///
/// Nothing is set up until the first lookup, so creating one at startup
/// costs nothing. Lookups block the calling thread; call them off the UI
/// thread, as discovery's steps already are. Plain DNS over UDP and TCP
/// only: no DNS-over-HTTPS, TLS or QUIC to a third-party resolver.
pub struct SystemDns {
    state: OnceLock<Result<(tokio::runtime::Runtime, TokioResolver), String>>,
}

impl SystemDns {
    /// A resolver that reads the system configuration on first use.
    pub fn new() -> Self {
        Self {
            state: OnceLock::new(),
        }
    }

    /// The runtime and resolver, set up on first use. A setup failure (no
    /// readable resolver configuration, say) is returned by every lookup.
    fn state(&self) -> Result<&(tokio::runtime::Runtime, TokioResolver), String> {
        self.state
            .get_or_init(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("couldn't start the DNS runtime: {error}"))?;
                // The resolver spawns its connections on the runtime it is
                // built in.
                let resolver = {
                    let _entered = runtime.enter();
                    let mut builder = TokioResolver::builder_tokio().map_err(|error| {
                        format!("couldn't read the system's DNS settings: {error}")
                    })?;
                    let options = builder.options_mut();
                    options.timeout = QUERY_TIMEOUT;
                    options.attempts = QUERY_ATTEMPTS;
                    options.cache_size = 64;
                    options.try_tcp_on_error = true;
                    builder
                        .build()
                        .map_err(|error| format!("couldn't set up DNS: {error}"))?
                };
                Ok((runtime, resolver))
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Runs one lookup of `name` and maps its answers with `record`. No
    /// records (NXDOMAIN or NODATA) is an empty answer, not an error.
    fn lookup<T>(
        &self,
        name: &str,
        query: impl AsyncFnOnce(
            &TokioResolver,
            String,
        ) -> Result<
            hickory_resolver::lookup::Lookup,
            hickory_resolver::net::NetError,
        >,
        record: impl Fn(&RData) -> Option<T>,
    ) -> Result<Vec<T>, String> {
        let (runtime, resolver) = self.state()?;
        // Fully qualified, so the system's search domains are never
        // appended to the mail domain.
        let fqdn = format!("{}.", name.trim_end_matches('.'));
        let answer = runtime
            .block_on(async { tokio::time::timeout(LOOKUP_DEADLINE, query(resolver, fqdn)).await });
        match answer {
            Err(_) => Err("timed out".to_owned()),
            Ok(Err(error)) if error.is_no_records_found() => Ok(Vec::new()),
            Ok(Err(error)) => Err(error.to_string()),
            Ok(Ok(lookup)) => Ok(lookup
                .answers()
                .iter()
                .filter_map(|answer| record(&answer.data))
                .collect()),
        }
    }
}

impl Default for SystemDns {
    fn default() -> Self {
        Self::new()
    }
}

impl DnsLookup for SystemDns {
    fn srv(&self, name: &str) -> Result<Vec<SrvRecord>, String> {
        self.lookup(
            name,
            async |resolver, name| resolver.srv_lookup(name).await,
            |data| match data {
                RData::SRV(srv) => Some(SrvRecord {
                    priority: srv.priority,
                    weight: srv.weight,
                    port: srv.port,
                    target: srv.target.to_ascii(),
                }),
                _ => None,
            },
        )
    }

    fn mx(&self, domain: &str) -> Result<Vec<MxRecord>, String> {
        self.lookup(
            domain,
            async |resolver, name| resolver.mx_lookup(name).await,
            |data| match data {
                RData::MX(mx) => Some(MxRecord {
                    preference: mx.preference,
                    exchange: mx.exchange.to_ascii(),
                }),
                _ => None,
            },
        )
    }
}

/// The best usable target of an SRV record set: lowest priority, then
/// highest weight. `None` when there are no records, the service is
/// explicitly not offered (target `.`), or the target is not a host name.
fn best_target(records: &[SrvRecord]) -> Option<(String, u16)> {
    let best = records
        .iter()
        .min_by_key(|record| (record.priority, std::cmp::Reverse(record.weight)))?;
    let target = best.target.trim_end_matches('.').to_ascii_lowercase();
    (best.port != 0 && is_dns_name(&target)).then_some((target, best.port))
}

/// [`crate::Step::DnsSrv`]: `_jmap._tcp`, then `_imaps._tcp`, then
/// `_submissions._tcp` (TLS) before `_submission._tcp` (STARTTLS).
/// `_imap._tcp` (STARTTLS) is not looked up: zmail's IMAP client only
/// speaks implicit TLS.
pub(crate) fn srv_step(email: &EmailAddress, dns: &dyn DnsLookup) -> StepResult {
    let domain = email.domain();
    let mut findings = StepFindings::default();
    let mut errors = Vec::new();
    let mut lookup = |service: &str| {
        let name = format!("{service}.{domain}");
        match dns.srv(&name) {
            Ok(records) => best_target(&records).map(|target| (name, target)),
            Err(error) => {
                errors.push(format!("{name}: {error}"));
                None
            }
        }
    };

    if let Some((record, (host, port))) = lookup("_jmap._tcp") {
        // RFC 8620 §2.2: the session resource is at the target's
        // well-known URL.
        let session_url = if port == 443 {
            format!("https://{host}/.well-known/jmap")
        } else {
            format!("https://{host}:{port}/.well-known/jmap")
        };
        findings.jmap = Some(Found::new(
            JmapSettings {
                session_url,
                username: email.to_string(),
            },
            Source::DnsSrv { record },
        ));
        return Ok(findings);
    }
    let server = |host: String, port: u16, security: Security| ServerSettings {
        host,
        port,
        security,
        username: email.to_string(),
        auth: AuthMethod::Password,
    };
    if let Some((record, (host, port))) = lookup("_imaps._tcp") {
        findings.imap = Some(Found::new(
            server(host, port, Security::Tls),
            Source::DnsSrv { record },
        ));
    }
    for (service, security) in [
        ("_submissions._tcp", Security::Tls),
        ("_submission._tcp", Security::StartTls),
    ] {
        if let Some((record, (host, port))) = lookup(service) {
            findings.smtp = Some(Found::new(
                server(host, port, security),
                Source::DnsSrv { record },
            ));
            break;
        }
    }
    // A failed lookup is not fatal: the step found nothing, and says why.
    if findings.imap.is_none() && findings.smtp.is_none() && !errors.is_empty() {
        findings.note = Some(format!("DNS lookup failed: {}", errors.join("; ")));
    }
    Ok(findings)
}

/// [`crate::Step::MxGuess`]: whether the domain's preferred MX belongs to
/// a hosted provider zmail has settings for (a Google Workspace or
/// Microsoft 365 domain, say).
pub(crate) fn mx_step(email: &EmailAddress, dns: &dyn DnsLookup) -> StepResult {
    let records = match dns.mx(email.domain()) {
        Ok(records) => records,
        // A failed lookup is not fatal: the step found nothing, and says why.
        Err(error) => {
            return Ok(StepFindings {
                note: Some(format!("DNS lookup failed: {error}")),
                ..StepFindings::default()
            });
        }
    };
    let Some(best) = records.iter().min_by_key(|record| record.preference) else {
        return Ok(StepFindings::default());
    };
    let mx_host = best.exchange.trim_end_matches('.').to_ascii_lowercase();
    let Some(provider) = Provider::from_mx_host(&mx_host) else {
        return Ok(StepFindings::default());
    };
    let source = Source::MxGuess { mx_host, provider };
    Ok(provider.findings(email, source))
}

#[cfg(test)]
mod tests {
    use super::{SrvRecord, best_target};

    fn srv(priority: u16, weight: u16, port: u16, target: &str) -> SrvRecord {
        SrvRecord {
            priority,
            weight,
            port,
            target: target.to_owned(),
        }
    }

    #[test]
    fn picks_lowest_priority_then_highest_weight() {
        let records = [
            srv(20, 100, 993, "backup.example.com."),
            srv(10, 5, 993, "light.example.com."),
            srv(10, 50, 993, "Heavy.Example.com."),
        ];
        assert_eq!(
            best_target(&records),
            Some(("heavy.example.com".to_owned(), 993))
        );
    }

    #[test]
    fn a_dot_target_means_not_offered() {
        assert_eq!(best_target(&[srv(0, 0, 0, ".")]), None);
        assert_eq!(best_target(&[]), None);
        assert_eq!(best_target(&[srv(0, 0, 993, "not a host")]), None);
    }
}
