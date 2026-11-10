//! Links in mail, for both frontends' link dialogs: which targets open
//! ([`link_target`]), the addresses written in a body's text
//! ([`written_urls`]), and whether a link's text shows another site than
//! the one it goes to ([`misleading_host`]), the usual phishing trick.
//!
//! The desktop app and the terminal UI both confirm before opening a link
//! and show its real target; the warning from [`misleading_host`] is said
//! the same way in both.

use std::ops::Range;

use url::Url;

use crate::attachment_safety::is_invisible_format_char;

/// The `mailto:` fields handed to the system's mail app. Others
/// (`attach`, `attachment`, `from`, `x-…`) are dropped: some mail apps
/// honour them, attaching a local file or changing the sender.
const MAILTO_FIELDS: &[&str] = &["to", "cc", "bcc", "subject", "body"];

/// The address to open for a link target or a written address, normalised
/// by the URL parser: `http`, `https` and `mailto` only. A `mailto:`
/// address keeps only its `to`, `cc`, `bcc`, `subject` and `body` fields.
pub fn link_target(href: &str) -> Option<String> {
    let url = Url::parse(href.trim()).ok()?;
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => Some(url.to_string()),
        "mailto" if !url.path().is_empty() => Some(mailto_target(url)),
        _ => None,
    }
}

/// `url` with only the fields of [`MAILTO_FIELDS`] and no fragment. A
/// field whose name is percent-encoded is dropped too: no mail link needs
/// one.
fn mailto_target(mut url: Url) -> String {
    let query = url.query().map(|query| {
        query
            .split('&')
            .filter(|field| {
                let name = field.split('=').next().unwrap_or_default();
                MAILTO_FIELDS
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(name))
            })
            .collect::<Vec<_>>()
            .join("&")
    });
    url.set_query(query.as_deref().filter(|query| !query.is_empty()));
    url.set_fragment(None);
    url.to_string()
}

/// The byte ranges of the web and `mailto:` addresses written in `text`.
pub fn written_urls(text: &str) -> Vec<Range<usize>> {
    const SCHEMES: [&str; 3] = ["https://", "http://", "mailto:"];
    let lower = text.to_ascii_lowercase();
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let Some((start, scheme)) = SCHEMES
            .iter()
            .filter_map(|scheme| lower[cursor..].find(scheme).map(|at| (cursor + at, scheme)))
            .min_by_key(|(start, _)| *start)
        else {
            break;
        };
        // "xhttps://" is not an address.
        let after_word = text[..start]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_alphanumeric());
        let end = start
            + text[start..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '`'))
                .unwrap_or(text.len() - start);
        let end = start + trim_trailing_punctuation(&text[start..end]).len();
        if after_word && end > start + scheme.len() + 1 {
            ranges.push(start..end);
        }
        cursor = end.max(start + scheme.len());
    }
    ranges
}

/// Drops sentence punctuation after an address ("see https://x.org.") and
/// a closing bracket that has no opening one inside the address.
fn trim_trailing_punctuation(url: &str) -> &str {
    let mut url = url;
    loop {
        let Some(last) = url.chars().next_back() else {
            return url;
        };
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(open).count() < url.matches(close).count()
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
        {
            url = &url[..url.len() - last.len_utf8()];
        } else {
            return url;
        }
    }
}

/// When a link's text shows a web address on another site than its
/// target, the host the text shows (as written, in its ASCII form). Text
/// that shows a website on a `mailto:` link counts too.
///
/// The text is read the way a person reads it: invisible characters are
/// ignored, and an address anywhere in it counts ("Sign in at
/// https://bank.example"), with or without a scheme, a port or a trailing
/// dot. A bare name such as `bank.example` counts when it is the whole
/// text, or when it is written as an address (`www.` in front, a port or
/// a path after it); in a sentence, a dotted word such as "Node.js" does
/// not. A target on the shown host, a subdomain of it or its parent
/// domain is not misleading.
pub fn misleading_host(text: &str, target: &str) -> Option<String> {
    let target_host = Url::parse(target)
        .ok()
        .and_then(|url| url.host_str().map(normalized_host));
    let text = visible_text(text);
    let whole = text.split_whitespace().count() == 1;
    shown_hosts(&text)
        .into_iter()
        .filter(|shown| whole || shown.written_as_address)
        .find(|shown| match &target_host {
            Some(target) => !same_site(&shown.host, target),
            // Text that looks like a website on a mailto: link.
            None => true,
        })
        .map(|shown| shown.host)
}

/// A host named in some text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShownHost {
    /// The host in its ASCII form (punycode for an international name),
    /// lowercase and without a trailing dot.
    pub(crate) host: String,
    /// Whether it is written as an address: with a scheme, `www.` in
    /// front, or a port or a path after it.
    pub(crate) written_as_address: bool,
}

/// `text` without the characters a reader cannot see (bidirectional and
/// zero-width marks, controls), with the full-width and ideographic full
/// stops that international domain names treat as dots made dots.
pub(crate) fn visible_text(text: &str) -> String {
    text.chars()
        .filter(|&ch| !is_invisible_format_char(ch) && (!ch.is_control() || ch.is_whitespace()))
        .map(|ch| match ch {
            '\u{3002}' | '\u{FF0E}' | '\u{FF61}' => '.',
            ch => ch,
        })
        .collect()
}

/// The hosts `text` names, in order: web addresses with a scheme
/// (`https://bank.example:8443/x`) and dotted names that look like a
/// domain (`www.bank.example`, `bank.example/login`, `bank.example`).
/// `text` should have gone through [`visible_text`]. E-mail addresses are
/// not hosts.
pub(crate) fn shown_hosts(text: &str) -> Vec<ShownHost> {
    text.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '`' | '|' | ','
            )
    })
    .filter_map(|token| shown_host(trim_trailing_punctuation(token)))
    .collect()
}

fn shown_host(token: &str) -> Option<ShownHost> {
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let host = Url::parse(token).ok()?.host_str().map(normalized_host)?;
        return (!host.is_empty()).then_some(ShownHost {
            host,
            written_as_address: true,
        });
    }
    if lower.contains("://") || lower.starts_with("mailto:") {
        return None;
    }
    // "bank.example", "www.bank.example/login", "bank.example:8443".
    let (authority, rest) = match token.find(['/', '?', '#', '\\']) {
        Some(at) => token.split_at(at),
        None => (token, ""),
    };
    // An e-mail address, or a user name in front of a host.
    if authority.contains('@') {
        return None;
    }
    let (name, port) = match authority.rsplit_once(':') {
        Some((name, port)) => {
            if port.is_empty() || !port.chars().all(|ch| ch.is_ascii_digit()) {
                return None;
            }
            (name, Some(port))
        }
        None => (authority, None),
    };
    let name = name.trim_end_matches('.');
    if !looks_like_domain(name) {
        return None;
    }
    let host = Url::parse(&format!("https://{name}/"))
        .ok()?
        .host_str()
        .map(normalized_host)?;
    let written_as_address = host.starts_with("www.") || port.is_some() || !rest.is_empty();
    Some(ShownHost {
        host,
        written_as_address,
    })
}

/// A dotted name whose labels are letters, digits and hyphens and whose
/// last label is two or more letters (`bank.example`, `bücher.example`),
/// so version numbers and abbreviations ("v1.2", "e.g") are not domains.
pub(crate) fn looks_like_domain(name: &str) -> bool {
    let tld = name.rsplit('.').next().unwrap_or_default();
    name.contains('.')
        && tld.chars().count() >= 2
        && tld.chars().all(char::is_alphabetic)
        && name.split('.').all(|label| {
            !label.is_empty() && label.chars().all(|ch| ch.is_alphanumeric() || ch == '-')
        })
}

/// A host as compared: lowercase and without a trailing dot.
pub(crate) fn normalized_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Whether two hosts are one site: the same once a leading `www.` is
/// dropped, or one a subdomain of the other (`bank.example` and
/// `login.bank.example`), which only the same owner can set up.
fn same_site(first: &str, second: &str) -> bool {
    let site = |host: &str| host.strip_prefix("www.").unwrap_or(host).to_owned();
    let (first, second) = (site(first), site(second));
    let is_within = |inner: &str, outer: &str| {
        inner == outer
            || inner
                .strip_suffix(outer)
                .is_some_and(|prefix| prefix.ends_with('.'))
    };
    is_within(&first, &second) || is_within(&second, &first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_links_open() {
        assert_eq!(
            link_target("https://example.com/a b"),
            Some("https://example.com/a%20b".to_owned())
        );
        assert_eq!(
            link_target("mailto:a@example.com"),
            Some("mailto:a@example.com".to_owned())
        );
        for href in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://",
            "blocked://example.org/x",
            "data:text/html,x",
            "#top",
            "/relative",
            "tel:+3531234567",
        ] {
            assert_eq!(link_target(href), None, "{href}");
        }
    }

    #[test]
    fn mail_links_keep_only_the_ordinary_fields() {
        assert_eq!(
            link_target("mailto:x@example.com?subject=Hi&body=Hello%20there").as_deref(),
            Some("mailto:x@example.com?subject=Hi&body=Hello%20there")
        );
        assert_eq!(
            link_target("mailto:x@example.com?CC=y@example.com&Bcc=z@example.com&To=w@example.com")
                .as_deref(),
            Some("mailto:x@example.com?CC=y@example.com&Bcc=z@example.com&To=w@example.com")
        );
        assert_eq!(
            link_target("mailto:x@example.com?attach=/home/u/.ssh/id_rsa").as_deref(),
            Some("mailto:x@example.com")
        );
        assert_eq!(
            link_target(
                "mailto:x@example.com?subject=Hi&attachment=%2Fetc%2Fpasswd&%61ttach=/x&from=boss@example.com&body=b#frag"
            )
            .as_deref(),
            Some("mailto:x@example.com?subject=Hi&body=b")
        );
    }

    #[test]
    fn written_addresses_are_found_without_trailing_punctuation() {
        let text = "See https://example.com/a. Or (http://b.example/x_(y)) and mailto:me@x.org! \
                    *https://c.example*, xhttps://no.example";
        let found: Vec<&str> = written_urls(text)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(
            found,
            vec![
                "https://example.com/a",
                "http://b.example/x_(y)",
                "mailto:me@x.org",
                "https://c.example",
            ]
        );
    }

    #[test]
    fn link_text_naming_another_site_is_flagged() {
        assert_eq!(
            misleading_host("https://bank.example/login", "https://evil.example/x").as_deref(),
            Some("bank.example")
        );
        assert_eq!(
            misleading_host("www.bank.example", "https://evil.example/login").as_deref(),
            Some("www.bank.example")
        );
        // The userinfo trick: the target's host is what follows the @.
        assert_eq!(
            misleading_host("bank.example", "https://bank.example@evil.example/").as_deref(),
            Some("bank.example")
        );
        // Text that looks like a website on a mail link.
        assert_eq!(
            misleading_host("bank.example", "mailto:x@evil.example").as_deref(),
            Some("bank.example")
        );
    }

    #[test]
    fn disguised_link_text_is_still_read() {
        let target = "https://evil.example/";
        for text in [
            // An address in a sentence.
            "Sign in at https://bank.example",
            "Sign in at https://bank.example/login now.",
            "  https://bank.example  ",
            "Visit www.bank.example today",
            "Log in: bank.example/login",
            // A bare name with a port, a path, a trailing dot.
            "bank.example:443",
            "bank.example/@login",
            "bank.example.",
            "https://bank.example.:8443/x",
            "BANK.EXAMPLE",
            // Invisible characters.
            "bank\u{200B}.example",
            "ba\u{200D}nk.exam\u{FEFF}ple",
            "\u{202E}bank.example",
            "https://bank\u{2060}.example",
            // Ideographic and full-width full stops.
            "bank\u{3002}example",
            "bank\u{FF0E}example",
            // In brackets and quotes.
            "(bank.example)",
            "\"https://bank.example\"",
            "<https://bank.example>",
        ] {
            assert_eq!(
                misleading_host(text, target).as_deref(),
                Some(if text.to_ascii_lowercase().contains("www.") {
                    "www.bank.example"
                } else {
                    "bank.example"
                }),
                "{text:?}"
            );
        }
        // The first address that differs is the one named.
        assert_eq!(
            misleading_host(
                "evil.example is fine but https://bank.example is not",
                target
            )
            .as_deref(),
            Some("bank.example")
        );
    }

    #[test]
    fn international_names_compare_in_their_ascii_form() {
        assert_eq!(
            misleading_host("bücher.example", "https://xn--bcher-kva.example/").as_deref(),
            None
        );
        assert_eq!(
            misleading_host("bücher.example", "https://bucher.example/").as_deref(),
            Some("xn--bcher-kva.example")
        );
    }

    #[test]
    fn honest_links_are_not_flagged() {
        for (text, target) in [
            ("Pay your bill", "https://evil.example/x"),
            ("Click here", "https://evil.example/"),
            ("www.bank.example", "https://bank.example/login"),
            ("https://bank.example", "https://www.bank.example/"),
            ("bank.example", "https://bank.example./"),
            ("bank.example", "https://BANK.example:8443/x"),
            // A subdomain or the parent of the shown site.
            ("bank.example", "https://click.bank.example/t/1"),
            ("login.bank.example", "https://bank.example/"),
            (
                "https://example.com/changelog",
                "https://example.com/changelog",
            ),
            ("help@example.com", "mailto:help@example.com"),
            ("Write to help@example.com", "mailto:help@example.com"),
            ("mailto:help@example.com", "mailto:help@example.com"),
            ("v1.2", "https://example.com/"),
            ("e.g. this", "https://example.com/"),
            // A dotted word in a sentence is not an address.
            ("Read the Node.js docs", "https://nodejs.org/"),
            ("Sign in at https://bank.example", "https://bank.example/x"),
        ] {
            assert_eq!(misleading_host(text, target), None, "{text} -> {target}");
        }
    }
}
