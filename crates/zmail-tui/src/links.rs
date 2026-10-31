//! Links in a message, shown as numbered references (`[3]`) and opened
//! only after the user has seen the real address and confirmed (ADR 0007
//! section 4), as the GUI's link dialog does.
//!
//! A body's links are its HTML links (the sanitiser's link table, with
//! their original targets) and the web and `mailto:` addresses written in
//! its text. Only `http`, `https` and `mailto` addresses open.

use std::{
    ops::Range,
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use url::Url;

/// The address to open for a link target or a written address, normalised
/// by the URL parser: `http`, `https` and `mailto` only.
pub fn link_target(href: &str) -> Option<String> {
    let url = Url::parse(href.trim()).ok()?;
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => Some(url.to_string()),
        "mailto" if !url.path().is_empty() => Some(url.to_string()),
        _ => None,
    }
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
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
        {
            url = &url[..url.len() - last.len_utf8()];
        } else {
            return url;
        }
    }
}

/// When a link's text looks like a web address on another site than its
/// target (the usual phishing trick), the site the text shows. The same
/// rule as the GUI's link dialog (`zmail-ui`'s `reader::links`).
pub fn misleading_host(text: &str, target: &str) -> Option<String> {
    let host = Url::parse(target)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    text_host(text).filter(|shown| match &host {
        Some(host) => site(shown) != site(host),
        // Text that looks like a website on a mailto: link.
        None => true,
    })
}

/// A host compared without a leading "www.", in lowercase.
fn site(host: &str) -> String {
    let host = host.to_ascii_lowercase();
    host.strip_prefix("www.").unwrap_or(&host).to_owned()
}

/// The host a link's text looks like it names: "https://bank.example/x",
/// "www.bank.example" or "bank.example". `None` for ordinary words.
fn text_host(text: &str) -> Option<String> {
    let text = text
        .trim()
        .trim_matches(|ch| matches!(ch, '<' | '>' | '(' | ')' | '"'));
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let candidate = if lower.starts_with("http://") || lower.starts_with("https://") {
        text.to_owned()
    } else if lower.starts_with("mailto:") || text.contains('@') {
        return None;
    } else {
        // "bank.example" or "www.bank.example/login": a dotted name with
        // a letters-only top-level domain.
        let host = text.split(['/', '?', '#']).next().unwrap_or_default();
        let tld = host.rsplit('.').next().unwrap_or_default();
        let dotted = host.contains('.')
            && tld.len() >= 2
            && tld.chars().all(|ch| ch.is_ascii_alphabetic())
            && host
                .chars()
                .all(|ch| ch.is_alphanumeric() || ch == '.' || ch == '-');
        if !dotted {
            return None;
        }
        format!("https://{text}")
    };
    Url::parse(&candidate)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}

/// Hands `url` to the operating system's opener: `open` on macOS, the URL
/// protocol handler on Windows, `xdg-open` elsewhere. `url` must come from
/// [`link_target`].
pub fn open(url: &str) -> Result<()> {
    let url = link_target(url).context("only web and mail addresses open")?;
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        Command::new("xdg-open")
    };
    let mut child = command
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("couldn't start the system's link opener")?;
    // Reap it in the background; openers return quickly.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{link_target, misleading_host, written_urls};

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
        assert_eq!(link_target("javascript:alert(1)"), None);
        assert_eq!(link_target("file:///etc/passwd"), None);
        assert_eq!(link_target("https://"), None);
    }

    #[test]
    fn written_addresses_are_found_without_trailing_punctuation() {
        let text = "See https://example.com/a. Or (http://b.example/x) and mailto:me@x.org!";
        let found: Vec<&str> = written_urls(text)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(
            found,
            vec![
                "https://example.com/a",
                "http://b.example/x",
                "mailto:me@x.org"
            ]
        );
        assert!(written_urls("xhttps://example.com").is_empty());
    }

    #[test]
    fn link_text_naming_another_site_is_flagged() {
        assert_eq!(
            misleading_host("www.bank.example", "https://evil.example/login"),
            Some("www.bank.example".to_owned())
        );
        assert_eq!(
            misleading_host("https://bank.example", "https://www.bank.example/"),
            None
        );
        assert_eq!(misleading_host("Click here", "https://evil.example/"), None);
    }
}
