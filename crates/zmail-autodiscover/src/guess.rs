//! [`crate::Step::HostnameGuess`]: the last resort, which tries the server
//! names most providers use (`imap.`, `smtp.` and `mail.<domain>`) on the
//! standard TLS ports.
//!
//! A port that accepts a TCP connection is only a hint; the UI shows the
//! result as a guess for the user to check, and the first sign-in proves it.
//! The probes only contact hosts under the user's own mail domain.

use std::{
    net::{TcpStream, ToSocketAddrs as _},
    time::Duration,
};

use crate::{
    AuthMethod, EmailAddress, Found, Security, ServerSettings, Source, StepFindings, StepResult,
};

/// Checks whether a TCP port answers.
pub trait PortProbe: Send + Sync {
    /// Whether `host:port` accepts a TCP connection.
    fn reachable(&self, host: &str, port: u16) -> bool;
}

/// The real [`PortProbe`]: resolves the host with the system resolver and
/// connects with a timeout.
#[derive(Debug, Clone, Copy)]
pub struct TcpProbe {
    /// The time limit for each connection attempt.
    pub timeout: Duration,
}

impl Default for TcpProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(3),
        }
    }
}

impl PortProbe for TcpProbe {
    fn reachable(&self, host: &str, port: u16) -> bool {
        let Ok(addresses) = (host, port).to_socket_addrs() else {
            return false;
        };
        addresses
            .take(4)
            .any(|address| TcpStream::connect_timeout(&address, self.timeout).is_ok())
    }
}

/// The IMAP candidates, best first. Implicit TLS only: zmail's IMAP client
/// has no STARTTLS.
fn imap_candidates(domain: &str) -> Vec<(String, u16, Security)> {
    vec![
        (format!("imap.{domain}"), 993, Security::Tls),
        (format!("mail.{domain}"), 993, Security::Tls),
    ]
}

/// The SMTP submission candidates, best first: implicit TLS on 465
/// (RFC 8314), then STARTTLS on 587.
fn smtp_candidates(domain: &str) -> Vec<(String, u16, Security)> {
    let mut candidates = Vec::new();
    for host in [format!("smtp.{domain}"), format!("mail.{domain}")] {
        candidates.push((host.clone(), 465, Security::Tls));
        candidates.push((host, 587, Security::StartTls));
    }
    candidates
}

/// Probes every candidate at once and returns the first, in preference
/// order, that answered.
fn first_reachable(
    candidates: Vec<(String, u16, Security)>,
    probe: &dyn PortProbe,
) -> Option<(String, u16, Security)> {
    let answered = std::thread::scope(|scope| {
        let probes = candidates
            .iter()
            .map(|(host, port, _)| scope.spawn(move || probe.reachable(host, *port)))
            .collect::<Vec<_>>();
        probes
            .into_iter()
            .map(|probe| probe.join().unwrap_or(false))
            .collect::<Vec<_>>()
    });
    candidates
        .into_iter()
        .zip(answered)
        .find_map(|(candidate, answered)| answered.then_some(candidate))
}

pub(crate) fn hostname_step(email: &EmailAddress, probe: &dyn PortProbe) -> StepResult {
    let domain = email.domain();
    let (imap, smtp) = std::thread::scope(|scope| {
        let imap = scope.spawn(|| first_reachable(imap_candidates(domain), probe));
        let smtp = first_reachable(smtp_candidates(domain), probe);
        (imap.join().ok().flatten(), smtp)
    });
    let found = |(host, port, security): (String, u16, Security)| {
        Found::new(
            ServerSettings {
                host,
                port,
                security,
                username: email.to_string(),
                auth: AuthMethod::Password,
            },
            Source::HostnameGuess,
        )
    };
    Ok(StepFindings {
        jmap: None,
        imap: imap.map(found),
        smtp: smtp.map(found),
        note: None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{PortProbe, hostname_step};
    use crate::{EmailAddress, Security};

    struct Open(HashSet<(&'static str, u16)>);

    impl PortProbe for Open {
        fn reachable(&self, host: &str, port: u16) -> bool {
            self.0
                .iter()
                .any(|(open, open_port)| *open == host && *open_port == port)
        }
    }

    #[test]
    fn prefers_dedicated_names_and_implicit_tls() {
        let email = EmailAddress::parse("me@example.com").expect("valid address");
        let probe = Open(HashSet::from([
            ("mail.example.com", 993),
            ("imap.example.com", 993),
            ("smtp.example.com", 587),
            ("mail.example.com", 465),
        ]));
        let findings = hostname_step(&email, &probe).expect("the step never fails");
        let imap = findings.imap.expect("imap found").value;
        assert_eq!((imap.host.as_str(), imap.port), ("imap.example.com", 993));
        // smtp.example.com:587 ranks above mail.example.com:465.
        let smtp = findings.smtp.expect("smtp found").value;
        assert_eq!(
            (smtp.host.as_str(), smtp.port, smtp.security),
            ("smtp.example.com", 587, Security::StartTls)
        );
    }

    #[test]
    fn finds_nothing_when_nothing_answers() {
        let email = EmailAddress::parse("me@example.com").expect("valid address");
        let findings = hostname_step(&email, &Open(HashSet::new())).expect("never fails");
        assert!(findings.imap.is_none() && findings.smtp.is_none());
    }
}
