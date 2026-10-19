//! Secret storage and security defaults for Zmail.

pub mod attachment_safety;
pub mod sender_safety;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use keyring::Entry;

/// Default remote content stance.
pub const REMOTE_CONTENT_POLICY: &str = "Blocked by default.";

/// Default HTML stance.
pub const HTML_RENDERING_POLICY: &str =
    "Use a constrained sanitized renderer unless a webview sandbox spike proves equally secure.";

const KEYRING_SERVICE: &str = "dev.zmail.desktop";

/// Sanitized HTML prepared for safe message rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedHtml {
    /// HTML with active blocks removed and remote references redacted.
    pub html: String,
    /// Readable text derived from the sanitized HTML.
    pub text: String,
    /// Whether the source HTML referenced remote content.
    pub has_remote_content: bool,
}

/// Named per-account secret slots.
///
/// The `Password` slot is the original single per-account secret; the
/// OAuth2 slots hold the provider client secret and the granted tokens so
/// background sync can refresh access tokens without user interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretSlot {
    /// Account password or app password.
    Password,
    /// OAuth2 provider application client secret (confidential clients only).
    ClientSecret,
    /// Current OAuth2 bearer access token.
    AccessToken,
    /// OAuth2 refresh token used to obtain new access tokens.
    RefreshToken,
}

impl SecretSlot {
    /// Returns the stable key fragment identifying the slot.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::ClientSecret => "client-secret",
            Self::AccessToken => "access-token",
            Self::RefreshToken => "refresh-token",
        }
    }
}

/// Stores and retrieves account secrets without writing them to the local metadata database.
pub trait SecretStore: Send + Sync {
    /// Persists the secret for an account.
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()>;
    /// Loads the secret for an account.
    fn get_account_secret(&self, account_id: i64) -> Result<String>;
    /// Removes the secret for an account.
    fn delete_account_secret(&self, account_id: i64) -> Result<()>;
    /// Persists the secret for an account in a named slot.
    fn set_account_secret_in_slot(
        &self,
        account_id: i64,
        slot: SecretSlot,
        secret: &str,
    ) -> Result<()>;
    /// Loads the secret for an account from a named slot.
    fn get_account_secret_from_slot(&self, account_id: i64, slot: SecretSlot) -> Result<String>;
    /// Removes the secret for an account in a named slot.
    fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()>;
}

/// OS keychain-backed implementation used by the desktop app.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    /// Persists the secret for an account in a named slot.
    fn set_slot(&self, account_id: i64, slot: SecretSlot, secret: &str) -> Result<()> {
        entry_for_slot(account_id, slot)?
            .set_password(secret)
            .with_context(|| {
                format!("failed to store {slot:?} credentials for account {account_id}")
            })
    }

    /// Loads the secret for an account from a named slot.
    fn get_slot(&self, account_id: i64, slot: SecretSlot) -> Result<String> {
        entry_for_slot(account_id, slot)?
            .get_password()
            .with_context(|| {
                format!("failed to load {slot:?} credentials for account {account_id}")
            })
    }

    /// Removes the secret for an account in a named slot.
    fn delete_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
        match entry_for_slot(account_id, slot)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!("failed to delete {slot:?} credentials for account {account_id}")
            }),
        }
    }
}

impl SecretStore for KeyringSecretStore {
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()> {
        entry_for(account_id)?
            .set_password(secret)
            .with_context(|| format!("failed to store credentials for account {account_id}"))
    }

    fn get_account_secret(&self, account_id: i64) -> Result<String> {
        entry_for(account_id)?
            .get_password()
            .with_context(|| format!("failed to load credentials for account {account_id}"))
    }

    fn delete_account_secret(&self, account_id: i64) -> Result<()> {
        match entry_for(account_id)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to delete credentials for account {account_id}")),
        }
    }

    fn set_account_secret_in_slot(
        &self,
        account_id: i64,
        slot: SecretSlot,
        secret: &str,
    ) -> Result<()> {
        self.set_slot(account_id, slot, secret)
    }

    fn get_account_secret_from_slot(&self, account_id: i64, slot: SecretSlot) -> Result<String> {
        self.get_slot(account_id, slot)
    }

    fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
        self.delete_slot(account_id, slot)
    }
}

/// In-memory secret store used by tests and local fixtures.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<(i64, SecretSlot), String>>,
}

impl InMemorySecretStore {
    /// Creates a new in-memory store wrapped in `Arc` for convenience.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(i64, SecretSlot), String>> {
        self.secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SecretStore for InMemorySecretStore {
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()> {
        self.set_account_secret_in_slot(account_id, SecretSlot::Password, secret)
    }

    fn get_account_secret(&self, account_id: i64) -> Result<String> {
        self.get_account_secret_from_slot(account_id, SecretSlot::Password)
    }

    fn delete_account_secret(&self, account_id: i64) -> Result<()> {
        self.delete_account_secret_in_slot(account_id, SecretSlot::Password)
    }

    fn set_account_secret_in_slot(
        &self,
        account_id: i64,
        slot: SecretSlot,
        secret: &str,
    ) -> Result<()> {
        self.lock().insert((account_id, slot), secret.to_owned());
        Ok(())
    }

    fn get_account_secret_from_slot(&self, account_id: i64, slot: SecretSlot) -> Result<String> {
        self.lock()
            .get(&(account_id, slot))
            .cloned()
            .with_context(|| format!("no credentials stored for account {account_id}"))
    }

    fn delete_account_secret_in_slot(&self, account_id: i64, slot: SecretSlot) -> Result<()> {
        self.lock().remove(&(account_id, slot));
        Ok(())
    }
}

fn entry_for(account_id: i64) -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, &format!("account-{account_id}"))
        .map_err(anyhow::Error::from)
        .with_context(|| format!("failed to prepare secure storage for account {account_id}"))
}

/// Builds the keyring entry for a named slot.
///
/// The password slot keeps the original `account-{id}` entry so existing
/// installs keep working; token slots append a stable suffix.
fn entry_for_slot(account_id: i64, slot: SecretSlot) -> Result<Entry> {
    match slot {
        SecretSlot::Password => entry_for(account_id),
        slot => Entry::new(
            KEYRING_SERVICE,
            &format!("account-{account_id}-{}", slot.as_str()),
        )
        .map_err(anyhow::Error::from)
        .with_context(|| {
            format!("failed to prepare secure storage for account {account_id} {slot:?}")
        }),
    }
}

/// Sanitizes HTML for the constrained Phase 3 reader.
pub fn sanitize_html_for_display(html: &str) -> SanitizedHtml {
    let without_scripts = strip_tag_block(html, "script");
    let without_styles = strip_tag_block(&without_scripts, "style");
    let has_remote_content = contains_remote_content(&without_styles);
    let html = redact_remote_urls(&without_styles);
    let text = html_to_text(&html);

    SanitizedHtml {
        html,
        text: if text.is_empty() {
            "No body available.".to_owned()
        } else {
            text
        },
        has_remote_content,
    }
}

fn strip_tag_block(input: &str, tag: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let open_tag = format!("<{tag}");
    let close_tag = format!("</{tag}>");
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;

    while let Some(start_rel) = lower[cursor..].find(&open_tag) {
        let start = cursor + start_rel;
        output.push_str(&input[cursor..start]);

        let Some(close_rel) = lower[start..].find(&close_tag) else {
            cursor = input.len();
            break;
        };
        let end = start + close_rel + close_tag.len();
        cursor = end;
    }

    if cursor < input.len() {
        output.push_str(&input[cursor..]);
    }

    output
}

fn contains_remote_content(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    [
        "src=\"http://",
        "src=\"https://",
        "src='http://",
        "src='https://",
        "href=\"http://",
        "href=\"https://",
        "href='http://",
        "href='https://",
        "background=\"http://",
        "background=\"https://",
        "background='http://",
        "background='https://",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Rewrites `http://` and `https://` (any letter case) to `blocked://`
/// inside tags, so attributes such as `src`, `href`, `background` and inline
/// `style` can never load remote content. URLs in the message's visible text
/// are left alone: they are words the sender wrote, not resources.
///
/// Quoted attribute values may contain `>`, so the scan tracks quotes and
/// only leaves a tag at an unquoted `>`. When unsure it stays inside the tag,
/// which redacts more, never less.
fn redact_remote_urls(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    let mut inside_tag = false;
    let mut attribute_quote = AttributeQuote::default();

    while let Some(ch) = rest.chars().next() {
        if inside_tag {
            if let Some(scheme_len) = remote_scheme_len(rest) {
                output.push_str("blocked://");
                rest = &rest[scheme_len..];
                continue;
            }
            if attribute_quote.ends_tag(ch) {
                inside_tag = false;
            }
        } else if ch == '<' {
            inside_tag = true;
            attribute_quote = AttributeQuote::default();
        }
        output.push(ch);
        rest = &rest[ch.len_utf8()..];
    }

    output
}

/// Tracks quoted attribute values inside a tag, so a `>` within
/// `title="a > b"` does not end the tag.
///
/// As in HTML, a quote only opens a value right after `=`; a stray quote in
/// an unquoted value (`width=50%"`) is an ordinary character and cannot
/// swallow the rest of the message.
#[derive(Default)]
struct AttributeQuote {
    open: Option<char>,
    after_equals: bool,
}

impl AttributeQuote {
    /// Feeds the next character inside a tag; returns whether it ends the tag.
    fn ends_tag(&mut self, ch: char) -> bool {
        if let Some(open) = self.open {
            if ch == open {
                self.open = None;
            }
            return false;
        }
        match ch {
            '>' => return true,
            '"' | '\'' if self.after_equals => self.open = Some(ch),
            _ => {}
        }
        if !ch.is_whitespace() {
            self.after_equals = ch == '=';
        }
        false
    }
}

/// Returns the length of a remote URL scheme (`http://` or `https://`, any
/// letter case) at the start of `input`.
fn remote_scheme_len(input: &str) -> Option<usize> {
    ["https://", "http://"]
        .iter()
        .find(|scheme| {
            input
                .get(..scheme.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
        })
        .map(|scheme| scheme.len())
}

/// Tags whose start or end begins a new line of readable text.
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "caption",
    "center",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "tr",
    "ul",
];

/// Inline formatting tags that can sit inside a word, so they add no
/// separator ("<b>dash</b>board" reads "dashboard"). Every other tag, such
/// as a table cell or an image, separates its neighbours with a space.
const INLINE_TAGS: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "del", "dfn", "em", "font", "i", "ins",
    "kbd", "mark", "q", "s", "samp", "small", "span", "strike", "strong", "sub", "sup", "time",
    "tt", "u", "var", "wbr",
];

fn html_to_text(input: &str) -> String {
    // The document title is metadata, not message text.
    let input = strip_tag_block(input, "title");
    let mut text = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut inside_tag = false;
    let mut attribute_quote = AttributeQuote::default();

    while let Some(ch) = chars.next() {
        match ch {
            '<' if !inside_tag => {
                inside_tag = true;
                attribute_quote = AttributeQuote::default();
                // Closing tags separate text exactly like opening ones, so
                // "<td>Plan</td><td>Price</td>" never reads "PlanPrice".
                if chars.peek() == Some(&'/') {
                    let _ = chars.next();
                }
                let mut tag_name = String::new();
                while let Some(next) = chars.peek().copied() {
                    if next == '>' || next.is_whitespace() || next == '/' {
                        break;
                    }
                    tag_name.push(next.to_ascii_lowercase());
                    let _ = chars.next();
                }
                if BLOCK_TAGS.contains(&tag_name.as_str()) {
                    text.push('\n');
                } else if !INLINE_TAGS.contains(&tag_name.as_str()) {
                    text.push(' ');
                }
            }
            _ if inside_tag => {
                if attribute_quote.ends_tag(ch) {
                    inside_tag = false;
                }
            }
            '&' => push_character_reference(&mut text, &mut chars),
            _ => text.push(ch),
        }
    }

    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Decodes one character reference after a consumed `&`.
///
/// Text that is not a well-formed reference, such as a bare "AT&T", is kept
/// as written instead of being swallowed.
fn push_character_reference(
    text: &mut String,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) {
    let mut reference = String::new();
    let mut terminated = false;
    while let Some(next) = chars.peek().copied() {
        if next == ';' {
            let _ = chars.next();
            terminated = true;
            break;
        }
        if !(next.is_ascii_alphanumeric() || next == '#') || reference.len() >= 10 {
            break;
        }
        reference.push(next);
        let _ = chars.next();
    }

    if terminated {
        // Unknown references (often invisible spacers such as "&zwnj;")
        // read as whitespace rather than as raw entity names.
        text.push(decode_character_reference(&reference).unwrap_or(' '));
    } else {
        text.push('&');
        text.push_str(&reference);
    }
}

fn decode_character_reference(reference: &str) -> Option<char> {
    let named = match reference {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "nbsp" => Some(' '),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "lsquo" => Some('\u{2018}'),
        "rsquo" => Some('\u{2019}'),
        "ldquo" => Some('\u{201c}'),
        "rdquo" => Some('\u{201d}'),
        "ndash" => Some('\u{2013}'),
        "mdash" => Some('\u{2014}'),
        "hellip" => Some('\u{2026}'),
        "euro" => Some('\u{20ac}'),
        "copy" => Some('\u{a9}'),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    let numeric = reference.strip_prefix('#')?;
    let value = match numeric.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => numeric.parse::<u32>().ok()?,
    };
    // Control characters become plain spaces so they cannot disturb layout.
    char::from_u32(value).map(|ch| if ch.is_control() { ' ' } else { ch })
}

/// A link in sanitized HTML: the text a reader sees and where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlLink {
    /// The link's visible text, as [`sanitize_html_for_display`] renders it.
    pub text: String,
    /// The `href` value with character references decoded. Remote
    /// addresses read `blocked://…`, as the sanitizer stored them.
    pub href: String,
}

/// Lists the `<a href>` links of sanitized HTML (the `html` of
/// [`SanitizedHtml`]) in document order.
///
/// This only reads the markup; it loads nothing. The reader uses it to make
/// a link's text clickable and to show the real target, and opens a link
/// only after the user clicks it and confirms.
pub fn html_links(html: &str) -> Vec<HtmlLink> {
    let html = strip_tag_block(html, "title");
    let lower = html.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut cursor = 0;
    while let Some(start) = find_anchor_start(&lower, cursor) {
        let Some(tag_end) = tag_end(&html, start) else {
            break;
        };
        let close = lower[tag_end..]
            .find("</a")
            .map_or(html.len(), |offset| tag_end + offset);
        if let Some(href) = attribute_value(&html[start..tag_end], "href") {
            links.push(HtmlLink {
                text: html_to_text(&html[tag_end..close]),
                href,
            });
        }
        cursor = close.max(tag_end);
    }
    links
}

/// The next `<a` tag at or after `from` (not `<abbr`, `<area`, …).
fn find_anchor_start(lower: &str, from: usize) -> Option<usize> {
    let mut cursor = from;
    while let Some(offset) = lower[cursor..].find("<a") {
        let start = cursor + offset;
        match lower[start + 2..].chars().next() {
            Some(next) if next.is_whitespace() || next == '>' || next == '/' => {
                return Some(start);
            }
            None => return None,
            _ => cursor = start + 2,
        }
    }
    None
}

/// The index just past the `>` that ends the tag starting at `start`,
/// skipping `>` inside quoted attribute values.
fn tag_end(html: &str, start: usize) -> Option<usize> {
    let mut attribute_quote = AttributeQuote::default();
    html[start + 1..]
        .char_indices()
        .find(|&(_, ch)| attribute_quote.ends_tag(ch))
        .map(|(offset, ch)| start + 1 + offset + ch.len_utf8())
}

/// The decoded value of attribute `wanted` in the start tag `tag`
/// (`<a href="…" …>`), if the tag has it.
fn attribute_value(tag: &str, wanted: &str) -> Option<String> {
    let inner = tag.strip_prefix('<')?.trim_end_matches('>');
    // Skip the tag name.
    let mut rest = inner.trim_start_matches(|ch: char| !ch.is_whitespace() && ch != '/');
    loop {
        rest = rest.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '/');
        if rest.is_empty() {
            return None;
        }
        let name_end = rest
            .find(|ch: char| ch.is_whitespace() || ch == '=' || ch == '/')
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        rest = rest[name_end..].trim_start();
        let mut value = "";
        if let Some(after_equals) = rest.strip_prefix('=') {
            let after_equals = after_equals.trim_start();
            match after_equals.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let body = &after_equals[1..];
                    let end = body.find(quote).unwrap_or(body.len());
                    value = &body[..end];
                    rest = body.get(end + 1..).unwrap_or("");
                }
                _ => {
                    let end = after_equals
                        .find(char::is_whitespace)
                        .unwrap_or(after_equals.len());
                    value = &after_equals[..end];
                    rest = &after_equals[end..];
                }
            }
        }
        if name.eq_ignore_ascii_case(wanted) {
            return Some(decode_character_references(value).trim().to_owned());
        }
    }
}

/// Decodes the character references in an attribute value.
fn decode_character_references(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&' {
            push_character_reference(&mut decoded, &mut chars);
        } else {
            decoded.push(ch);
        }
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::{
        HtmlLink, InMemorySecretStore, SecretSlot, SecretStore, html_links,
        sanitize_html_for_display,
    };

    #[test]
    fn in_memory_store_round_trips_secrets() {
        let store = InMemorySecretStore::default();
        store
            .set_account_secret(4, "top-secret")
            .expect("secret should store");
        assert_eq!(
            store.get_account_secret(4).expect("secret should load"),
            "top-secret"
        );
        store
            .delete_account_secret(4)
            .expect("secret should delete");
        assert!(store.get_account_secret(4).is_err());
    }

    #[test]
    fn in_memory_store_keeps_named_slots_independent() {
        let store = InMemorySecretStore::default();
        store
            .set_account_secret(4, "app-password")
            .expect("the password slot should store");
        store
            .set_account_secret_in_slot(4, SecretSlot::AccessToken, "access-1")
            .expect("the access token slot should store");
        store
            .set_account_secret_in_slot(4, SecretSlot::RefreshToken, "refresh-1")
            .expect("the refresh token slot should store");
        store
            .set_account_secret_in_slot(4, SecretSlot::ClientSecret, "client-1")
            .expect("the client secret slot should store");

        // The legacy single-secret API addresses the password slot only.
        assert_eq!(
            store.get_account_secret(4).expect("password loads"),
            "app-password"
        );
        assert_eq!(
            store
                .get_account_secret_from_slot(4, SecretSlot::AccessToken)
                .expect("access token loads"),
            "access-1"
        );
        assert_eq!(
            store
                .get_account_secret_from_slot(4, SecretSlot::RefreshToken)
                .expect("refresh token loads"),
            "refresh-1"
        );
        assert_eq!(
            store
                .get_account_secret_from_slot(4, SecretSlot::ClientSecret)
                .expect("client secret loads"),
            "client-1"
        );

        store
            .delete_account_secret_in_slot(4, SecretSlot::AccessToken)
            .expect("the access token slot should delete");
        assert!(
            store
                .get_account_secret_from_slot(4, SecretSlot::AccessToken)
                .is_err()
        );
        // Deleting one slot leaves the others untouched.
        assert!(store.get_account_secret(4).is_ok());
        assert!(
            store
                .get_account_secret_from_slot(4, SecretSlot::RefreshToken)
                .is_ok()
        );
    }

    #[test]
    fn secret_slots_have_stable_keys() {
        assert_eq!(SecretSlot::Password.as_str(), "password");
        assert_eq!(SecretSlot::ClientSecret.as_str(), "client-secret");
        assert_eq!(SecretSlot::AccessToken.as_str(), "access-token");
        assert_eq!(SecretSlot::RefreshToken.as_str(), "refresh-token");
    }

    #[test]
    fn sanitizes_html_and_blocks_remote_content() {
        let sanitized = sanitize_html_for_display(
            r#"<div>Hello<script>alert('x')</script><img src="https://example.test/a.png"></div>"#,
        );

        assert!(sanitized.has_remote_content);
        assert!(!sanitized.html.contains("<script"));
        assert!(sanitized.html.contains("blocked://example.test/a.png"));
        assert!(sanitized.text.contains("Hello"));
    }

    #[test]
    fn remote_urls_are_blocked_in_tags_but_kept_in_visible_text() {
        let sanitized = sanitize_html_for_display(concat!(
            r#"<p title="a > b" style="background:url(HTTP://track.example.test/bg)">"#,
            "Docs at https://docs.example.test/start and http://old.example.test",
            r#"<img SRC='HtTpS://track.example.test/pixel.gif'>"#,
            r#"<a href="https://shop.example.test/sale">Shop</a></p>"#,
            r#"<table><tr><td width=50%">Total</td></tr></table>"#,
        ));

        assert!(sanitized.has_remote_content);
        let lower = sanitized.html.to_ascii_lowercase();
        assert!(!lower.contains("src='http"));
        assert!(!lower.contains("href=\"http"));
        assert!(!lower.contains("url(http"));
        assert!(
            sanitized
                .html
                .contains("blocked://track.example.test/pixel.gif")
        );
        assert!(sanitized.html.contains("blocked://shop.example.test/sale"));
        assert_eq!(
            sanitized.text,
            "Docs at https://docs.example.test/start and http://old.example.test Shop\nTotal"
        );
    }

    #[test]
    fn readable_text_separates_block_elements_and_table_cells() {
        let sanitized = sanitize_html_for_display(concat!(
            "<html><head><title>Newsletter</title></head><body>",
            "<h1>Welcome</h1><p>Your new <b>dash</b>board</p>",
            "<h2>Faster exports</h2>",
            "<table><tr><th>Plan</th><th>Price</th></tr>",
            "<tr><td>Starter</td><td>EUR&nbsp;9</td></tr>",
            "<tr><td>Team</td><td>EUR 29</td></tr></table>",
            "<ul><li>One</li><li>Two</li></ul>",
            "</body></html>",
        ));

        assert_eq!(
            sanitized.text,
            "Welcome\nYour new dashboard\nFaster exports\nPlan Price\nStarter EUR 9\nTeam EUR 29\nOne\nTwo"
        );
    }

    #[test]
    fn readable_text_decodes_character_references_and_keeps_bare_ampersands() {
        let sanitized = sanitize_html_for_display(
            "<p>AT&T &amp; friends &#8212; it&#x2019;s &lt;fine&gt;&zwnj;!</p>",
        );

        assert_eq!(
            sanitized.text,
            "AT&T & friends \u{2014} it\u{2019}s <fine> !"
        );
    }

    #[test]
    fn html_links_list_text_and_targets_in_order() {
        let sanitized = sanitize_html_for_display(
            r#"<p>Pay your <a class="btn" href="https://evil.example/pay?a=1&amp;b=2">PayPal invoice</a>
            or <A HREF='mailto:help@example.com'>write to us</A>.
            <abbr title="x">abbr</abbr> <a name="top">no href</a>
            <a href=https://example.org/plain>plain</a>
            <a href="https://example.org/img"><img src="https://example.org/i.png"></a></p>"#,
        );
        let link = |text: &str, href: &str| HtmlLink {
            text: text.to_owned(),
            href: href.to_owned(),
        };
        assert_eq!(
            html_links(&sanitized.html),
            [
                link("PayPal invoice", "blocked://evil.example/pay?a=1&b=2"),
                link("write to us", "mailto:help@example.com"),
                link("plain", "blocked://example.org/plain"),
                link("", "blocked://example.org/img"),
            ]
        );
        // Every link text is in the rendered text, so the reader can find
        // it there.
        for link in html_links(&sanitized.html) {
            assert!(sanitized.text.contains(&link.text), "{link:?}");
        }
    }

    #[test]
    fn html_links_survive_a_quoted_angle_bracket_and_no_closing_tag() {
        assert_eq!(
            html_links(r#"<a title="a > b" href="blocked://example.org/x">Example"#),
            [HtmlLink {
                text: "Example".to_owned(),
                href: "blocked://example.org/x".to_owned(),
            }]
        );
        assert!(html_links("<a").is_empty());
    }
}
