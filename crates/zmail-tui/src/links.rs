//! Links in a message, shown as numbered references (`[3]`) and opened
//! only after the user has seen the real address and confirmed (ADR 0007
//! section 4), as the GUI's link dialog does.
//!
//! A body's links are its HTML links (the sanitiser's link table, with
//! their original targets) and the web and `mailto:` addresses written in
//! its text. Only `http`, `https` and `mailto` addresses open, a
//! `mailto:` address with only its `to`, `cc`, `bcc`, `subject` and
//! `body` fields (`zmail_security::links`).

use std::process::{Command, Stdio};

use anyhow::{Context, Result};
/// Which link targets open, the addresses written in text, and the
/// warning for link text that shows another site, shared with the GUI's
/// link dialog.
pub use zmail_security::links::{link_target, misleading_host, written_urls};

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
        // An address in a sentence, or one with an invisible character.
        assert_eq!(
            misleading_host("Sign in at https://bank.example", "https://evil.example/").as_deref(),
            Some("bank.example")
        );
        assert_eq!(
            misleading_host("bank\u{200B}.example", "https://evil.example/").as_deref(),
            Some("bank.example")
        );
    }

    #[test]
    fn mail_links_lose_unusual_fields() {
        assert_eq!(
            link_target("mailto:a@example.com?subject=Hi&attach=/etc/passwd").as_deref(),
            Some("mailto:a@example.com?subject=Hi")
        );
    }
}
