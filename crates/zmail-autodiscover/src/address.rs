//! The email address discovery starts from.

use std::fmt;

/// A parsed email address: the local part and the ASCII (punycode) domain
/// that discovery looks up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailAddress {
    local_part: String,
    domain: String,
}

/// Why an address can't be used for discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    /// There is no `@`, or nothing before it.
    Missing,
    /// The part after the `@` is not a domain name discovery can look up
    /// (an IP literal, a single label such as `localhost`, or invalid
    /// characters).
    InvalidDomain,
}

impl fmt::Display for AddressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "Enter an email address such as name@example.com.",
            Self::InvalidDomain => "The part after @ isn't a mail domain.",
        })
    }
}

impl std::error::Error for AddressError {}

impl EmailAddress {
    /// Parses `name@example.com`. The domain is lowercased and converted to
    /// its ASCII form, so an internationalised domain is looked up by its
    /// punycode name.
    pub fn parse(address: &str) -> Result<Self, AddressError> {
        let address = address.trim();
        let (local_part, domain) = address.rsplit_once('@').ok_or(AddressError::Missing)?;
        if local_part.is_empty() || local_part.chars().any(char::is_whitespace) {
            return Err(AddressError::Missing);
        }
        let domain = ascii_domain(domain).ok_or(AddressError::InvalidDomain)?;
        Ok(Self {
            local_part: local_part.to_owned(),
            domain,
        })
    }

    /// The part before the `@`.
    pub fn local_part(&self) -> &str {
        &self.local_part
    }

    /// The lowercase ASCII domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }
}

impl fmt::Display for EmailAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.local_part, self.domain)
    }
}

/// The lowercase ASCII form of `name` when it is a multi-label DNS name, or
/// `None` for IP literals, single labels and anything else that can't be
/// put into a URL host or a DNS query as is.
pub(crate) fn ascii_domain(name: &str) -> Option<String> {
    let name = name.trim().trim_end_matches('.');
    // `Host::parse` applies IDNA, so `bücher.example` becomes
    // `xn--bcher-kva.example`, and rejects forbidden host characters.
    let url::Host::Domain(domain) = url::Host::parse(name).ok()? else {
        return None;
    };
    let domain = domain.to_ascii_lowercase();
    is_dns_name(&domain).then_some(domain)
}

/// Whether `name` is a multi-label ASCII DNS hostname: letters, digits and
/// hyphens in each label (underscores are not valid in hostnames), at most
/// 63 bytes per label and 253 in all.
pub(crate) fn is_dns_name(name: &str) -> bool {
    let labels = name.split('.').collect::<Vec<_>>();
    name.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        // A numeric last label means an IPv4 address in disguise.
        && labels
            .last()
            .is_some_and(|tld| !tld.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::{AddressError, EmailAddress, is_dns_name};

    #[test]
    fn parses_and_normalises_the_domain() {
        let address = EmailAddress::parse("  Jane.Doe@Example.COM ").expect("valid address");
        assert_eq!(address.local_part(), "Jane.Doe");
        assert_eq!(address.domain(), "example.com");
        assert_eq!(address.to_string(), "Jane.Doe@example.com");

        let address = EmailAddress::parse("me@bücher.example").expect("IDN address");
        assert_eq!(address.domain(), "xn--bcher-kva.example");

        // The last @ separates the domain; quoted local parts may hold one.
        let address = EmailAddress::parse("\"a@b\"@example.org").expect("quoted local part");
        assert_eq!(address.domain(), "example.org");
    }

    #[test]
    fn rejects_what_discovery_cannot_look_up() {
        assert_eq!(
            EmailAddress::parse("example.com"),
            Err(AddressError::Missing)
        );
        assert_eq!(
            EmailAddress::parse("@example.com"),
            Err(AddressError::Missing)
        );
        assert_eq!(
            EmailAddress::parse("a b@example.com"),
            Err(AddressError::Missing)
        );
        for domain in [
            "localhost",
            "127.0.0.1",
            "[::1]",
            "",
            "exa mple.com",
            "example.com/path",
            "_imaps.example.com",
            "-bad.example.com",
        ] {
            assert_eq!(
                EmailAddress::parse(&format!("me@{domain}")),
                Err(AddressError::InvalidDomain),
                "{domain} should be rejected"
            );
        }
    }

    #[test]
    fn dns_names_are_bounded() {
        assert!(is_dns_name("mail.example.com"));
        assert!(!is_dns_name(&format!("{}.com", "a".repeat(64))));
        assert!(!is_dns_name(&format!("{}com", "a.".repeat(127))));
        assert!(!is_dns_name("10.0.0.1"));
    }
}
