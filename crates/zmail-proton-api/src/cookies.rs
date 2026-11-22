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

use serde::{Deserialize, Serialize};
use zmail_core::time::{Stamp, parse_netscape_date, parse_rfc2822};

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
        let now = Stamp::now().seconds();
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
        let now = Stamp::now().seconds();
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
        let now = Stamp::now().seconds();
        self.lock()
            .iter()
            .filter(|cookie| cookie.expires.is_some() && !cookie.is_expired(now))
            .cloned()
            .collect()
    }

    /// Adds cookies from an earlier [`CookieJar::snapshot`].
    pub fn restore(&self, cookies: impl IntoIterator<Item = StoredCookie>) {
        let now = Stamp::now().seconds();
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
    parse_rfc2822(value)
        // `Wed, 21-Oct-2015 07:28:00 GMT` (the Netscape format).
        .or_else(|| parse_netscape_date(value))
        .map(Stamp::seconds)
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

    // Characterisation (issue #489): what `Expires` values (and the `Date`
    // header, which is read the same way) parsed to before the workspace
    // moved off chrono, as seconds since the Unix epoch.
    #[test]
    fn dates_parse_as_before() {
        let cases: &[(&str, Option<i64>)] = &[
            // The forms servers send.
            ("Thu, 15 Oct 2026 10:42:00 GMT", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 +0000", Some(1_792_060_920)),
            ("Thu, 15-Oct-2026 10:42:00 GMT", Some(1_792_060_920)),
            ("Wed, 21 Oct 2015 07:28:00 GMT", Some(1_445_412_480)),
            ("Wed, 21-Oct-2015 07:28:00 GMT", Some(1_445_412_480)),
            ("Fri, 01 Jan 2100 00:00:00 GMT", Some(4_102_444_800)),
            ("Thu, 01 Jan 1970 00:00:00 GMT", Some(0)),
            ("Wed, 31 Dec 1969 23:59:59 GMT", Some(-1)),
            ("Sun, 06 Nov 1994 08:49:37 GMT", Some(784_111_777)),
            ("Fri, 31 Dec 9999 23:59:59 GMT", Some(253_402_300_799)),
            ("Fri, 31-Dec-9999 23:59:59 GMT", Some(253_402_300_799)),
            // RFC 2822: zones.
            ("Thu, 15 Oct 2026 10:42:00 UT", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 Z", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 gmt", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 -0000", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 +0530", Some(1_792_041_120)),
            ("Thu, 15 Oct 2026 10:42:00 -0800", Some(1_792_089_720)),
            ("Thu, 15 Oct 2026 10:42:00 +2359", Some(1_791_974_580)),
            ("Thu, 15 Oct 2026 10:42:00 -2359", Some(1_792_147_260)),
            ("Thu, 15 Oct 2026 10:42:00 EST", Some(1_792_078_920)),
            ("Thu, 15 Oct 2026 10:42:00 EDT", Some(1_792_075_320)),
            ("Thu, 15 Oct 2026 10:42:00 CST", Some(1_792_082_520)),
            ("Thu, 15 Oct 2026 10:42:00 CDT", Some(1_792_078_920)),
            ("Thu, 15 Oct 2026 10:42:00 MST", Some(1_792_086_120)),
            ("Thu, 15 Oct 2026 10:42:00 MDT", Some(1_792_082_520)),
            ("Thu, 15 Oct 2026 10:42:00 PST", Some(1_792_089_720)),
            ("Thu, 15 Oct 2026 10:42:00 pdt", Some(1_792_086_120)),
            ("Thu, 15 Oct 2026 10:42:00 A", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 y", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 J", None),
            ("Thu, 15 Oct 2026 10:42:00 UTC", None),
            ("Thu, 15 Oct 2026 10:42:00 CET", None),
            ("Thu, 15 Oct 2026 10:42:00 +2400", None),
            ("Thu, 15 Oct 2026 10:42:00 +9959", None),
            ("Thu, 15 Oct 2026 10:42:00 -0060", None),
            ("Thu, 15 Oct 2026 10:42:00 +05:30", None),
            ("Thu, 15 Oct 2026 10:42:00 +053", None),
            ("Thu, 15 Oct 2026 10:42:00 +05300", None),
            ("Thu, 15 Oct 2026 10:42:00 + 0530", None),
            ("Thu, 15 Oct 2026 10:42:00 \u{2212}0530", None),
            ("Thu, 15 Oct 2026 10:42:00", None),
            ("Thu, 15 Oct 2026 10:42:00 ", None),
            // RFC 2822: optional and obsolete parts.
            ("15 Oct 2026 10:42:00 +0000", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42 +0000", Some(1_792_060_920)),
            ("15 Oct 2026 10:42 GMT", Some(1_792_060_920)),
            ("5 Oct 2026 10:42:00 +0000", Some(1_791_196_920)),
            ("Mon, 5 Oct 2026 10:42:00 +0000", Some(1_791_196_920)),
            ("Mon, 05 Oct 2026 10:42:00 +0000", Some(1_791_196_920)),
            ("15 Oct 26 10:42:00 +0000", Some(1_792_060_920)),
            ("15 Oct 49 10:42:00 +0000", Some(2_517_907_320)),
            ("15 Oct 50 10:42:00 +0000", Some(-606_316_680)),
            ("15 Oct 99 10:42:00 +0000", Some(939_984_120)),
            ("15 Oct 126 10:42:00 +0000", Some(1_792_060_920)),
            ("15 Oct 009 10:42:00 +0000", Some(-1_900_156_680)),
            ("15 Oct 0026 10:42:00 +0000", Some(-61_321_843_080)),
            ("15 Oct 02026 10:42:00 +0000", Some(1_792_060_920)),
            ("15 Oct 12026 10:42:00 +0000", Some(317_361_580_920)),
            ("1 Jan 0000 00:00:00 +0000", Some(-62_167_219_200)),
            ("1 Jan 0001 00:00:00 +0000", Some(-62_135_596_800)),
            ("15 Oct 6 10:42:00 +0000", None),
            ("015 Oct 2026 10:42:00 +0000", None),
            ("thu, 15 oct 2026 10:42:00 +0000", Some(1_792_060_920)),
            ("THU, 15 OCT 2026 10:42:00 +0000", Some(1_792_060_920)),
            ("Thursday, 15 Oct 2026 10:42:00 +0000", None),
            ("Thu, 15 October 2026 10:42:00 +0000", None),
            ("Thu 15 Oct 2026 10:42:00 +0000", None),
            ("Fri, 15 Oct 2026 10:42:00 +0000", None),
            ("Xyz, 15 Oct 2026 10:42:00 +0000", None),
            // RFC 2822: white space and comments.
            (" Thu, 15 Oct 2026 10:42:00 +0000", Some(1_792_060_920)),
            (
                "\t\r\n Thu, 15 Oct 2026 10:42:00 +0000",
                Some(1_792_060_920),
            ),
            ("Thu,15 Oct 2026 10:42:00 +0000", Some(1_792_060_920)),
            ("Thu ,15 Oct 2026 10:42:00 +0000", None),
            ("Thu,  15  Oct  2026  10:42:00  +0000", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10 : 42 : 00 +0000", None),
            ("Thu, 15 Oct 2026 10: 42 :00 +0000", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42: 00 +0000", None),
            ("Thu, 15 Oct 2026 10:42:00\r\n +0000", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026\u{a0}10:42:00 +0000", Some(1_792_060_920)),
            ("Thu, 15Oct 2026 10:42:00 +0000", None),
            ("Thu, 15 Oct2026 10:42:00 +0000", None),
            ("Thu, 15 Oct 202610:42:00 +0000", None),
            ("Thu, 15 Oct 2026 10:42:00+0000", None),
            ("Thu, 15 Oct 2026 10:42:00 +0000 (UTC)", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 +0000(UTC)", Some(1_792_060_920)),
            (
                "Thu, 15 Oct 2026 10:42:00 +0000 (UTC) (again)",
                Some(1_792_060_920),
            ),
            (
                "Thu, 15 Oct 2026 10:42:00 +0000 (nested (comment))",
                Some(1_792_060_920),
            ),
            (
                "Thu, 15 Oct 2026 10:42:00 +0000 (escaped \\) paren)",
                Some(1_792_060_920),
            ),
            ("Thu, 15 Oct 2026 10:42:00 +0000 ()", Some(1_792_060_920)),
            ("Thu, 15 Oct 2026 10:42:00 +0000 (unterminated", None),
            ("Thu, 15 Oct 2026 10:42:00 +0000 (UTC) ", None),
            ("Thu, 15 Oct 2026 10:42:00 +0000 (UTC) x", None),
            ("Thu, 15 Oct 2026 10:42:00 GMT (UTC)", Some(1_792_060_920)),
            ("Thu, 15 (c) Oct 2026 10:42:00 +0000", None),
            ("(c) Thu, 15 Oct 2026 10:42:00 +0000", None),
            ("Thu, 15 Oct 2026 10:42:00 +0000 ", None),
            ("Thu, 15 Oct 2026 10:42:00 +0000\n", None),
            ("Thu, 15 Oct 2026 10:42:00 +0000 x", None),
            ("Thu, 15 Oct 2026 10:42:00 GMT ", None),
            ("Thu, 15 Oct 2026 10:42:00 GMT+1", None),
            // RFC 2822: fields out of range, leap seconds.
            ("Thu, 15 Oct 2026 10:42:60 +0000", Some(1_792_060_979)),
            ("Thu, 31 Dec 2026 23:59:60 +0000", Some(1_798_761_599)),
            ("Thu, 15 Oct 2026 10:42:61 +0000", None),
            ("Thu, 15 Oct 2026 24:00:00 +0000", None),
            ("Thu, 15 Oct 2026 10:60:00 +0000", None),
            ("Thu, 15 Oct 2026 1:42:00 +0000", None),
            ("Thu, 15 Oct 2026 10:4:00 +0000", None),
            ("Thu, 15 Oct 2026 10:42:0 +0000", None),
            ("Thu, 15 Oct 2026 10:42:00.5 +0000", None),
            ("Thu, 15 Oct 2026 010:42:00 +0000", None),
            ("Sat, 31 Oct 2026 10:42:00 +0000", Some(1_793_443_320)),
            ("32 Oct 2026 10:42:00 +0000", None),
            ("0 Oct 2026 10:42:00 +0000", None),
            ("30 Feb 2026 10:42:00 +0000", None),
            ("29 Feb 2024 10:42:00 +0000", Some(1_709_203_320)),
            ("29 Feb 2026 10:42:00 +0000", None),
            ("31 Dec 9999 23:59:59 -2359", Some(253_402_387_139)),
            ("31 Dec 262142 23:59:59 +0000", Some(8_210_266_876_799)),
            ("31 Dec 262142 23:59:59 -0001", None),
            ("1 Jan 262143 00:00:00 +0000", None),
            ("15 Oct 99999999999999999999 10:42:00 +0000", None),
            // The Netscape form, read loosely.
            ("Thu, 15-Oct-2026 10:42:00 GMT", Some(1_792_060_920)),
            ("thu, 15-oct-2026 10:42:00 GMT", Some(1_792_060_920)),
            ("Thu,15-Oct-2026 10:42:00 GMT", Some(1_792_060_920)),
            ("Thu,   15-Oct-2026   10:42:00   GMT", Some(1_792_060_920)),
            ("Thu, 15-Oct-2026 10:42:00GMT", Some(1_792_060_920)),
            ("Thu, 5-Oct-2026 10:42:00 GMT", None),
            ("Mon, 5-Oct-2026 10:42:00 GMT", Some(1_791_196_920)),
            ("Mon, 05-Oct-2026 1:2:3 GMT", Some(1_791_162_123)),
            ("Thu, 15-Oct-+2026 10:42:00 GMT", Some(1_792_060_920)),
            ("Thu, 15-Oct-26 10:42:00 GMT", Some(-61_321_843_080)),
            ("Sun, 15-Oct-0026 10:42:00 GMT", None),
            ("Thu, 15- Oct-2026 10:42:00 GMT", None),
            ("Thu, 15-Oct- 2026 10:42:00 GMT", Some(1_792_060_920)),
            ("Thu, 15-Oct-2026 10:42:60 GMT", Some(1_792_060_979)),
            ("Thu, 15-Oct-2026 10:42:00 gmt", None),
            ("Thu, 15-Oct-2026 10:42:00 UTC", None),
            ("Thu, 15-Oct-2026 10:42:00 +0000", None),
            ("Thu, 15-Oct-2026 10:42:00", None),
            ("Thu, 15-Oct-2026 10:42 GMT", None),
            ("Thu, 15-Oct-2026 10:42:00 GMT ", None),
            (" Thu, 15-Oct-2026 10:42:00 GMT", None),
            ("Fri, 15-Oct-2026 10:42:00 GMT", None),
            ("15-Oct-2026 10:42:00 GMT", None),
            ("Thursday, 15-Oct-2026 10:42:00 GMT", None),
            ("Thursday, 15-Oct-26 10:42:00 GMT", None),
            ("Thu, 15-October-2026 10:42:00 GMT", None),
            ("Thu, 32-Oct-2026 10:42:00 GMT", None),
            ("Thu, 15-Oct-2026 24:00:00 GMT", None),
            ("Tue, 15-Oct-+12026 10:42:00 GMT", None),
            ("Fri, 15-Oct--0001 10:42:00 GMT", Some(-62_173_919_880)),
            // Not dates.
            ("", None),
            (" ", None),
            ("0", None),
            ("1792060920", None),
            ("Thu", None),
            ("Thu,", None),
            ("Thu, 15 Oct", None),
            ("Thu, 15 Oct 2026", None),
            ("2026-10-15T10:42:00Z", None),
            ("Thu Oct 15 10:42:00 2026", None),
            ("tomorrow", None),
        ];
        for (value, expected) in cases {
            assert_eq!(parse_cookie_date(value), *expected, "{value:?}");
        }
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
