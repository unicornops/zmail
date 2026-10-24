//! What the sanitiser does with each address in a message: the link
//! targets it keeps (behind opaque ids), the remote resources it blocks or
//! allows, the `cid:` parts it keeps, and everything it drops.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr},
};

use url::{Host, Url};

use super::{BLOCKED_SCHEME, LINK_SCHEME, LinkTarget, RemoteResource, SanitizeOptions};

/// The link schemes a message may point at. Everything else (`javascript:`,
/// `data:`, `file:`, `vbscript:`, relative and in-page links…) is dropped,
/// leaving the link's text inert.
const LINK_SCHEMES: &[&str] = &["http", "https", "mailto", "tel", "sms"];

/// The `data:` image types an `<img>` may embed. No SVG: it is a document,
/// not a picture.
const DATA_IMAGE_TYPES: &[&str] = &["image/png", "image/gif", "image/jpeg", "image/webp"];

/// Host suffixes that only name machines on a local network.
const LOCAL_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "localdomain",
    "internal",
    "intranet",
    "lan",
    "home",
    "corp",
    "private",
    "home.arpa",
];

/// Where an address appears, which decides what it may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceUse {
    /// `<img src>`: also a small allowlist of `data:` images.
    Image,
    /// A `background` attribute or a CSS `url()`.
    Background,
}

/// A position in the recorded remote resources, to undo the ones a
/// dropped CSS declaration recorded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Checkpoint(usize);

/// Collects the link targets, remote resources and `cid:` references of
/// one message while the sanitiser rewrites them.
#[derive(Debug, Default)]
pub(crate) struct UrlPolicy {
    allow_remote: bool,
    legacy_blocked_links: bool,
    /// `href`s keep their (checked) target instead of an id.
    pub(crate) inline_targets: bool,
    pub(crate) links: Vec<LinkTarget>,
    pub(crate) remote: Vec<RemoteResource>,
    remote_index: HashMap<String, usize>,
    pub(crate) cid_references: Vec<String>,
}

impl UrlPolicy {
    pub(crate) fn new(options: &SanitizeOptions) -> Self {
        Self {
            allow_remote: options.allow_remote,
            legacy_blocked_links: options.legacy_blocked_links,
            ..Self::default()
        }
    }

    pub(crate) fn legacy_blocked_links(&self) -> bool {
        self.legacy_blocked_links
    }

    /// The opaque `zmail-link:<n>` id that replaces `href` (or, with
    /// `inline_targets`, its checked target), or `None` to drop the
    /// attribute.
    pub(crate) fn link(&mut self, href: &str) -> Option<String> {
        let target = link_target(href, self.legacy_blocked_links)?;
        if self.inline_targets {
            return Some(target.url);
        }
        self.links.push(target);
        Some(format!("{LINK_SCHEME}:{}", self.links.len() - 1))
    }

    /// Records a link found by the text derivation of markup that was too
    /// complex to sanitise, and returns its index.
    pub(crate) fn link_index(&mut self, href: &str) -> Option<usize> {
        let target = link_target(href, self.legacy_blocked_links)?;
        self.links.push(target);
        Some(self.links.len() - 1)
    }

    /// What replaces a resource address: the address itself when it may
    /// load, `zmail-blocked:<n>` for a blocked remote resource, or `None`
    /// to drop it.
    pub(crate) fn resource(&mut self, raw: &str, used_as: ResourceUse) -> Option<String> {
        let value = trim_url(raw);
        if starts_with_ignore_case(value, "cid:") {
            let content_id = normalise_content_id(&value[4..])?;
            if !self.cid_references.contains(&content_id) {
                self.cid_references.push(content_id.clone());
            }
            return Some(format!("cid:{content_id}"));
        }
        if starts_with_ignore_case(value, "data:") {
            return (used_as == ResourceUse::Image && is_allowed_data_image(value))
                .then(|| value.to_owned());
        }
        let url = Url::parse(value).ok()?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return None;
        }
        let address = url.to_string();
        let index = match self.remote_index.get(&address) {
            Some(&index) => index,
            None => {
                let loads = self.allow_remote && is_public_remote_url(&address);
                self.remote.push(RemoteResource {
                    origin: url.origin().ascii_serialization(),
                    url: address.clone(),
                    loads,
                });
                self.remote_index.insert(address, self.remote.len() - 1);
                self.remote.len() - 1
            }
        };
        let resource = &self.remote[index];
        Some(if resource.loads {
            resource.url.clone()
        } else {
            format!("{BLOCKED_SCHEME}:{index}")
        })
    }

    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.remote.len())
    }

    /// Forgets the remote resources recorded since `checkpoint`.
    pub(crate) fn rollback(&mut self, checkpoint: Checkpoint) {
        if self.remote.len() > checkpoint.0 {
            self.remote.truncate(checkpoint.0);
            self.remote_index.retain(|_, index| *index < checkpoint.0);
        }
    }
}

/// The target of an `href`, when it is one a message may link to.
fn link_target(href: &str, legacy_blocked_links: bool) -> Option<LinkTarget> {
    let href = trim_url(href);
    // Bodies stored before ADR 0006 kept remote links as `blocked://host/…`
    // and lost their scheme; those open over HTTPS, as they always did.
    let legacy;
    let href = match href.get(..10) {
        Some(prefix) if legacy_blocked_links && prefix.eq_ignore_ascii_case("blocked://") => {
            legacy = format!("https://{}", &href[10..]);
            legacy.as_str()
        }
        _ => href,
    };
    let url = Url::parse(href).ok()?;
    let scheme = url.scheme();
    if !LINK_SCHEMES.contains(&scheme) {
        return None;
    }
    if matches!(scheme, "http" | "https") && url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    if !matches!(scheme, "http" | "https") && url.path().trim().is_empty() {
        return None;
    }
    Some(LinkTarget {
        url: url.to_string(),
        text: String::new(),
    })
}

/// Whether `url` may be loaded when the user allows remote content: an
/// `https:` address on the default port, without credentials, whose host
/// is a public name or a public IP address (never loopback, link-local,
/// private, shared or reserved ranges, nor a local network name).
///
/// This checks the address as written. Renderers must still refuse a
/// public name that resolves to a private address when they load it.
pub fn is_public_remote_url(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return false;
    }
    match url.host() {
        Some(Host::Domain(domain)) => is_public_domain(domain),
        Some(Host::Ipv4(address)) => is_public_ipv4(address),
        Some(Host::Ipv6(address)) => is_public_ipv6(address),
        None => false,
    }
}

/// Whether a resolved address is one remote content may be fetched from:
/// not loopback, private, link-local, shared (CGNAT), multicast,
/// documentation or reserved, nor such an IPv4 address mapped into IPv6.
/// A renderer that fetches allowed remote content checks every address a
/// public name resolves to with this before connecting.
pub fn is_public_ip(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => is_public_ipv4(address),
        std::net::IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_domain(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    let Some((_, top_level)) = domain.rsplit_once('.') else {
        // A single label ("intranet", "printer") is a local name.
        return false;
    };
    if top_level.is_empty() || top_level.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    !LOCAL_SUFFIXES.iter().any(|suffix| {
        domain == *suffix
            || domain
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with('.'))
    })
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        // "This network", shared address space (CGNAT), IETF protocol
        // assignments, benchmarking and the reserved 240/4.
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b))
        || a >= 240)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = address.segments();
    // NAT64 (64:ff9b::/96) carries an IPv4 address in its last 32 bits.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., high, low] = segments;
        let embedded = (u32::from(high) << 16) | u32::from(low);
        return is_public_ipv4(Ipv4Addr::from(embedded));
    }
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        // IPv4-compatible (::/96), unique local (fc00::/7), link-local
        // (fe80::/10), the old site-local (fec0::/10), discard (100::/64)
        // and documentation (2001:db8::/32).
        || segments[..6] == [0, 0, 0, 0, 0, 0]
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || segments[..4] == [0x100, 0, 0, 0]
        || segments[..2] == [0x2001, 0xdb8])
}

/// An address as an engine reads it: surrounding whitespace and control
/// characters removed. Tabs and newlines inside are removed by the URL
/// parser, as browsers do.
fn trim_url(raw: &str) -> &str {
    raw.trim_matches(|ch: char| ch.is_whitespace() || ch.is_control())
}

fn starts_with_ignore_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// A Content-ID from a `cid:` URL (RFC 2392): percent-decoded, without
/// angle brackets. `None` when empty or not something a renderer could
/// look up safely.
pub(crate) fn normalise_content_id(value: &str) -> Option<String> {
    let decoded = percent_decode(value.trim());
    let content_id = decoded
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    let printable = content_id
        .chars()
        .all(|ch| ch.is_ascii_graphic() && !matches!(ch, '"' | '\'' | '\\' | '<' | '>'));
    (!content_id.is_empty() && content_id.len() <= 256 && printable).then(|| content_id.to_owned())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = value.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn is_allowed_data_image(value: &str) -> bool {
    let Some(header) = value
        .get(5..)
        .and_then(|rest| rest.split_once(',').map(|(header, _)| header))
    else {
        return false;
    };
    let media_type = header.split(';').next().unwrap_or_default().trim();
    DATA_IMAGE_TYPES
        .iter()
        .any(|allowed| media_type.eq_ignore_ascii_case(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_https_addresses_may_load() {
        for url in [
            "https://cdn.example.com/a.png",
            "https://203.0.113.0.example.net/x",
            "https://8.8.8.8/pixel.gif",
            "https://[2606:4700::1111]/x",
            "https://images.example.test:443/a.png",
        ] {
            assert!(is_public_remote_url(url), "{url}");
        }
        for url in [
            "http://cdn.example.com/a.png",
            "https://user:pw@cdn.example.com/a.png",
            "https://cdn.example.com:8443/a.png",
            "https://localhost/a.png",
            "https://printer/a.png",
            "https://nas.local/a.png",
            "https://router.home.arpa/a.png",
            "https://127.0.0.1/a.png",
            "https://0x7f.1/a.png",
            "https://2130706433/a.png",
            "https://10.0.0.8/a.png",
            "https://172.16.4.1/a.png",
            "https://192.168.1.1/a.png",
            "https://169.254.169.254/latest/meta-data",
            "https://100.64.0.1/a.png",
            "https://0.0.0.0/a.png",
            "https://[::1]/a.png",
            "https://[fd00::1]/a.png",
            "https://[fe80::1]/a.png",
            "https://[::ffff:192.168.0.1]/a.png",
            "https://[64:ff9b::a00:1]/a.png",
            "file:///etc/passwd",
            "//cdn.example.com/a.png",
            "not a url",
        ] {
            assert!(!is_public_remote_url(url), "{url}");
        }
    }

    #[test]
    fn resolved_addresses_are_checked_too() {
        for (address, public) in [
            ("93.184.216.34", true),
            ("2606:4700::1111", true),
            ("127.0.0.1", false),
            ("10.1.2.3", false),
            ("169.254.169.254", false),
            ("::1", false),
            ("fe80::1", false),
            ("::ffff:192.168.1.1", false),
        ] {
            let address: std::net::IpAddr = address.parse().expect("an address");
            assert_eq!(is_public_ip(address), public, "{address}");
        }
    }

    #[test]
    fn links_keep_their_scheme_and_drop_dangerous_ones() {
        let target = |href| link_target(href, false).map(|target| target.url);
        assert_eq!(
            target("http://example.com/a").as_deref(),
            Some("http://example.com/a")
        );
        assert_eq!(
            target(" HTTPS://Example.COM/a?b=1 ").as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(
            target("mailto:help@example.com").as_deref(),
            Some("mailto:help@example.com")
        );
        assert_eq!(
            target("tel:+3531234567").as_deref(),
            Some("tel:+3531234567")
        );
        for href in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "java\tscript:alert(1)",
            "vbscript:msgbox(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "\\\\server\\share\\x",
            "//evil.example/x",
            "/relative",
            "#top",
            "blocked://example.com/x",
            "https://",
            "mailto:",
        ] {
            assert_eq!(target(href), None, "{href}");
        }
        assert_eq!(
            link_target("blocked://example.com/x", true)
                .map(|target| target.url)
                .as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn resources_are_blocked_kept_or_dropped() {
        let mut policy = UrlPolicy::new(&SanitizeOptions::default());
        assert_eq!(
            policy.resource("https://t.example/p.gif", ResourceUse::Image),
            Some("zmail-blocked:0".to_owned())
        );
        assert_eq!(
            policy.resource("https://t.example/p.gif", ResourceUse::Background),
            Some("zmail-blocked:0".to_owned()),
            "the same address keeps its index"
        );
        assert_eq!(
            policy.resource("cid:%3Clogo@x%3E", ResourceUse::Image),
            Some("cid:logo@x".to_owned())
        );
        assert_eq!(policy.cid_references, ["logo@x"]);
        assert_eq!(
            policy
                .resource("data:image/png;base64,iVBOR", ResourceUse::Image)
                .as_deref(),
            Some("data:image/png;base64,iVBOR")
        );
        for (raw, used_as) in [
            ("data:image/png;base64,iVBOR", ResourceUse::Background),
            ("data:image/svg+xml,<svg/>", ResourceUse::Image),
            ("data:text/html,x", ResourceUse::Image),
            ("file:///C:/x.png", ResourceUse::Image),
            ("\\\\server\\x.png", ResourceUse::Image),
            ("//t.example/p.gif", ResourceUse::Image),
            ("javascript:alert(1)", ResourceUse::Image),
            ("x.png", ResourceUse::Image),
        ] {
            assert_eq!(policy.resource(raw, used_as), None, "{raw}");
        }
        assert_eq!(policy.remote.len(), 1);
        assert!(!policy.remote[0].loads);

        let mut allowed = UrlPolicy::new(&SanitizeOptions {
            allow_remote: true,
            ..SanitizeOptions::default()
        });
        assert_eq!(
            allowed
                .resource("https://cdn.example.com/a.png", ResourceUse::Image)
                .as_deref(),
            Some("https://cdn.example.com/a.png")
        );
        assert_eq!(
            allowed
                .resource("http://cdn.example.com/a.png", ResourceUse::Image)
                .as_deref(),
            Some("zmail-blocked:1"),
            "plain http never loads"
        );
        assert_eq!(
            allowed
                .resource("https://192.168.1.1/a.png", ResourceUse::Image)
                .as_deref(),
            Some("zmail-blocked:2"),
            "nor does a LAN address"
        );
        assert_eq!(allowed.remote[0].origin, "https://cdn.example.com");
    }
}
