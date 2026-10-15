//! Secret storage and security defaults for Zmail.

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

fn redact_remote_urls(input: &str) -> String {
    input
        .replace("https://", "blocked://")
        .replace("http://", "blocked://")
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

    while let Some(ch) = chars.next() {
        match ch {
            '<' if !inside_tag => {
                inside_tag = true;
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
            '>' if inside_tag => inside_tag = false,
            _ if inside_tag => {}
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

#[cfg(test)]
mod tests {
    use super::{InMemorySecretStore, SecretSlot, SecretStore, sanitize_html_for_display};

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
}
