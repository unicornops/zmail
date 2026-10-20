//! The DNS steps: SRV records (RFC 6186, RFC 8314, RFC 8620) and the MX
//! guess.
//!
//! std can resolve host names but not SRV or MX records, and zmail has no
//! DNS resolver dependency yet, so these steps run only when the caller
//! supplies a [`DnsLookup`]. [`crate::Network::system`] doesn't, and the
//! steps are left out of the sequence.

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
    if findings.imap.is_none() && findings.smtp.is_none() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(findings)
}

/// [`crate::Step::MxGuess`]: whether the domain's preferred MX belongs to
/// a hosted provider zmail has settings for (a Google Workspace or
/// Microsoft 365 domain, say).
pub(crate) fn mx_step(email: &EmailAddress, dns: &dyn DnsLookup) -> StepResult {
    let records = dns.mx(email.domain())?;
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
