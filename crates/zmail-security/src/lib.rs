//! Secret storage and security defaults for Zmail.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
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

/// Stores and retrieves account secrets without writing them to the local metadata database.
pub trait SecretStore: Send + Sync {
    /// Persists the secret for an account.
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()>;
    /// Loads the secret for an account.
    fn get_account_secret(&self, account_id: i64) -> Result<String>;
    /// Removes the secret for an account.
    fn delete_account_secret(&self, account_id: i64) -> Result<()>;
}

/// OS keychain-backed implementation used by the desktop app.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringSecretStore;

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
}

/// In-memory secret store used by tests and local fixtures.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<i64, String>>,
}

impl InMemorySecretStore {
    /// Creates a new in-memory store wrapped in `Arc` for convenience.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl SecretStore for InMemorySecretStore {
    fn set_account_secret(&self, account_id: i64, secret: &str) -> Result<()> {
        self.secrets
            .lock()
            .map_err(|_| anyhow!("secret store mutex was poisoned"))?
            .insert(account_id, secret.to_owned());
        Ok(())
    }

    fn get_account_secret(&self, account_id: i64) -> Result<String> {
        self.secrets
            .lock()
            .map_err(|_| anyhow!("secret store mutex was poisoned"))?
            .get(&account_id)
            .cloned()
            .with_context(|| format!("no credentials stored for account {account_id}"))
    }

    fn delete_account_secret(&self, account_id: i64) -> Result<()> {
        self.secrets
            .lock()
            .map_err(|_| anyhow!("secret store mutex was poisoned"))?
            .remove(&account_id);
        Ok(())
    }
}

fn entry_for(account_id: i64) -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, &format!("account-{account_id}"))
        .map_err(anyhow::Error::from)
        .with_context(|| format!("failed to prepare secure storage for account {account_id}"))
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

fn html_to_text(input: &str) -> String {
    let mut text = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut inside_tag = false;

    while let Some(ch) = chars.next() {
        match ch {
            '<' => {
                inside_tag = true;
                let mut tag_name = String::new();
                while let Some(next) = chars.peek().copied() {
                    if next == '>' || next.is_whitespace() || next == '/' {
                        break;
                    }
                    tag_name.push(next.to_ascii_lowercase());
                    let _ = chars.next();
                }
                if matches!(
                    tag_name.as_str(),
                    "br" | "p" | "div" | "li" | "tr" | "table"
                ) {
                    text.push('\n');
                }
            }
            '>' => {
                inside_tag = false;
                text.push(' ');
            }
            _ if inside_tag => {}
            '&' => {
                let mut entity = String::new();
                while let Some(next) = chars.peek().copied() {
                    entity.push(next);
                    let _ = chars.next();
                    if next == ';' || entity.len() >= 10 {
                        break;
                    }
                }
                text.push_str(match entity.as_str() {
                    "amp;" => "&",
                    "lt;" => "<",
                    "gt;" => ">",
                    "nbsp;" => " ",
                    "quot;" => "\"",
                    "#39;" => "'",
                    _ => " ",
                });
            }
            _ => text.push(ch),
        }
    }

    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{InMemorySecretStore, SecretStore, sanitize_html_for_display};

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
    fn sanitizes_html_and_blocks_remote_content() {
        let sanitized = sanitize_html_for_display(
            r#"<div>Hello<script>alert('x')</script><img src="https://example.test/a.png"></div>"#,
        );

        assert!(sanitized.has_remote_content);
        assert!(!sanitized.html.contains("<script"));
        assert!(sanitized.html.contains("blocked://example.test/a.png"));
        assert!(sanitized.text.contains("Hello"));
    }
}
