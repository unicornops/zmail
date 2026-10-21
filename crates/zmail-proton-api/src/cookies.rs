//! A small cookie jar for the API host.
//!
//! Proton's API sets session cookies (such as `Session-Id`) that it expects
//! back on later requests, and Bridge keeps them in a persistent jar
//! (`internal/cookies`). Zmail's client only ever talks to one host, so this
//! jar keeps host-only cookies from `Set-Cookie` headers: it honours
//! `Path`, `Max-Age`, `Expires` and `Secure`, and refuses cookies whose
//! `Domain` does not cover the host. It is Zmail's own code, not a port.
//!
//! [`CookieJar::snapshot`] and [`CookieJar::restore`] let the caller keep
//! persistent cookies across restarts, as Bridge does.

use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

/// One stored cookie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value.
    pub value: String,
    /// Path prefix the cookie applies to.
    pub path: String,
    /// Expiry as a Unix timestamp; `None` for a session cookie.
    pub expires: Option<i64>,
    /// Whether the cookie may only be sent over HTTPS.
    pub secure: bool,
}

impl StoredCookie {
    fn is_expired(&self, now: i64) -> bool {
        self.expires.is_some_and(|expires| expires <= now)
    }

    fn applies_to(&self, path: &str, secure: bool) -> bool {
        (secure || !self.secure) && path_matches(&self.path, path)
    }
}

/// Thread-safe cookie jar for a single host.
#[derive(Debug, Default)]
pub struct CookieJar {
    cookies: Mutex<Vec<StoredCookie>>,
}

impl CookieJar {
    /// Creates an empty jar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores the cookies from a response's `Set-Cookie` header values.
    pub(crate) fn store<'a>(
        &self,
        host: &str,
        request_path: &str,
        set_cookie: impl IntoIterator<Item = &'a str>,
    ) {
        let now = Utc::now().timestamp();
        let mut cookies = self.lock();
        for header in set_cookie {
            let Some(cookie) = parse_set_cookie(header, host, request_path, now) else {
                continue;
            };
            cookies.retain(|existing| existing.name != cookie.name || existing.path != cookie.path);
            if !cookie.is_expired(now) {
                cookies.push(cookie);
            }
        }
    }

    /// Returns the `Cookie` header value for a request, if any cookie applies.
    pub(crate) fn header_for(&self, path: &str, secure: bool) -> Option<String> {
        let now = Utc::now().timestamp();
        let mut cookies = self.lock();
        cookies.retain(|cookie| !cookie.is_expired(now));
        // RFC 6265 section 5.4: longer paths first.
        let mut matching: Vec<&StoredCookie> = cookies
            .iter()
            .filter(|cookie| cookie.applies_to(path, secure))
            .collect();
        matching.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
        let header = matching
            .iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>()
            .join("; ");
        (!header.is_empty()).then_some(header)
    }

    /// Returns the persistent (non-session, unexpired) cookies.
    pub fn snapshot(&self) -> Vec<StoredCookie> {
        let now = Utc::now().timestamp();
        self.lock()
            .iter()
            .filter(|cookie| cookie.expires.is_some() && !cookie.is_expired(now))
            .cloned()
            .collect()
    }

    /// Adds cookies from an earlier [`CookieJar::snapshot`].
    pub fn restore(&self, cookies: impl IntoIterator<Item = StoredCookie>) {
        let now = Utc::now().timestamp();
        let mut stored = self.lock();
        for cookie in cookies {
            if cookie.is_expired(now) {
                continue;
            }
            stored.retain(|existing| existing.name != cookie.name || existing.path != cookie.path);
            stored.push(cookie);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<StoredCookie>> {
        self.cookies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Parses one `Set-Cookie` value (RFC 6265 section 5.2), or returns `None`
/// for a cookie the jar must ignore.
fn parse_set_cookie(
    header: &str,
    host: &str,
    request_path: &str,
    now: i64,
) -> Option<StoredCookie> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut cookie = StoredCookie {
        name: name.to_owned(),
        value: value.trim().trim_matches('"').to_owned(),
        path: default_path(request_path),
        expires: None,
        secure: false,
    };
    let mut max_age = None;
    for attribute in parts {
        let (key, value) = attribute
            .split_once('=')
            .map_or((attribute.trim(), ""), |(key, value)| {
                (key.trim(), value.trim())
            });
        match key.to_ascii_lowercase().as_str() {
            "path" if value.starts_with('/') => cookie.path = value.to_owned(),
            "domain" if !domain_matches(host, value) => return None,
            "secure" => cookie.secure = true,
            "max-age" => {
                max_age = value.parse::<i64>().ok();
            }
            "expires" => {
                if let Some(expires) = parse_cookie_date(value) {
                    cookie.expires = Some(expires);
                }
            }
            _ => {}
        }
    }
    // Max-Age wins over Expires; zero or negative deletes the cookie.
    if let Some(max_age) = max_age {
        cookie.expires = Some(if max_age <= 0 {
            i64::MIN
        } else {
            now.saturating_add(max_age)
        });
    }
    Some(cookie)
}

/// RFC 6265 section 5.1.4: the request path up to its last `/`.
fn default_path(request_path: &str) -> String {
    match request_path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(index) => request_path[..index].to_owned(),
    }
}

/// RFC 6265 section 5.1.4 path matching.
fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// Whether a `Domain` attribute covers the host (RFC 6265 section 5.1.3).
fn domain_matches(host: &str, domain: &str) -> bool {
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    !domain.is_empty()
        && (host == domain
            || host
                .strip_suffix(&domain)
                .is_some_and(|prefix| prefix.ends_with('.')))
}

/// Parses the `Expires` date formats servers actually send.
fn parse_cookie_date(value: &str) -> Option<i64> {
    if let Ok(date) = DateTime::parse_from_rfc2822(value) {
        return Some(date.timestamp());
    }
    // `Wed, 21-Oct-2015 07:28:00 GMT` (the Netscape format).
    NaiveDateTime::parse_from_str(value, "%a, %d-%b-%Y %H:%M:%S GMT")
        .ok()
        .map(|date| date.and_utc().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "mail-api.proton.me";

    #[test]
    fn stores_and_returns_cookies() {
        let jar = CookieJar::new();
        jar.store(
            HOST,
            "/auth/v4",
            [
                "Session-Id=abc123; Path=/; Secure; HttpOnly",
                "Tag=default; Path=/; Max-Age=3600",
            ],
        );
        assert_eq!(
            jar.header_for("/core/v4/users", true).as_deref(),
            Some("Session-Id=abc123; Tag=default")
        );
        // Secure cookies never go over plain HTTP.
        assert_eq!(
            jar.header_for("/core/v4/users", false).as_deref(),
            Some("Tag=default")
        );
    }

    #[test]
    fn replaces_and_deletes_cookies() {
        let jar = CookieJar::new();
        jar.store(HOST, "/", ["Session-Id=one; Path=/"]);
        jar.store(HOST, "/", ["Session-Id=two; Path=/"]);
        assert_eq!(jar.header_for("/", true).as_deref(), Some("Session-Id=two"));
        jar.store(HOST, "/", ["Session-Id=gone; Path=/; Max-Age=0"]);
        assert_eq!(jar.header_for("/", true), None);
        jar.store(
            HOST,
            "/",
            ["Old=1; Path=/; Expires=Wed, 21 Oct 2015 07:28:00 GMT"],
        );
        assert_eq!(jar.header_for("/", true), None);
    }

    #[test]
    fn honours_path_and_domain() {
        let jar = CookieJar::new();
        jar.store(
            HOST,
            "/auth/v4/info",
            [
                "Scoped=1",
                "Other=1; Domain=example.com",
                "Parent=1; Domain=.proton.me; Path=/",
            ],
        );
        // Default path is the request path's directory.
        assert_eq!(
            jar.header_for("/auth/v4/refresh", true).as_deref(),
            Some("Scoped=1; Parent=1")
        );
        assert_eq!(
            jar.header_for("/auth/v40", true).as_deref(),
            Some("Parent=1")
        );
        assert_eq!(
            jar.header_for("/core/v4/users", true).as_deref(),
            Some("Parent=1")
        );
    }

    #[test]
    fn snapshot_keeps_only_persistent_cookies() {
        let jar = CookieJar::new();
        jar.store(
            HOST,
            "/",
            [
                "Session=1; Path=/",
                "Persist=1; Path=/; Expires=Fri, 01 Jan 2100 00:00:00 GMT",
                "Netscape=1; Path=/; Expires=Fri, 01-Jan-2100 00:00:00 GMT",
            ],
        );
        let snapshot = jar.snapshot();
        let names: Vec<&str> = snapshot.iter().map(|cookie| cookie.name.as_str()).collect();
        assert_eq!(names, ["Persist", "Netscape"]);

        let restored = CookieJar::new();
        restored.restore(snapshot);
        assert_eq!(
            restored.header_for("/", true).as_deref(),
            Some("Persist=1; Netscape=1")
        );
    }

    #[test]
    fn path_matching_follows_rfc_6265() {
        assert!(path_matches("/", "/anything"));
        assert!(path_matches("/auth", "/auth"));
        assert!(path_matches("/auth", "/auth/v4"));
        assert!(path_matches("/auth/", "/auth/v4"));
        assert!(!path_matches("/auth", "/authx"));
        assert_eq!(default_path("/auth/v4/info"), "/auth/v4");
        assert_eq!(default_path("/auth"), "/");
        assert!(domain_matches(HOST, "proton.me"));
        assert!(domain_matches(HOST, ".PROTON.me"));
        assert!(!domain_matches(HOST, "on.me"));
        assert!(!domain_matches(HOST, ""));
    }
}
