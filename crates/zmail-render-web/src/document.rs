//! What a web view is given to show, and the page Zmail serves for it.
//!
//! A [`WebDocument`] is a prepared message: HTML that has already been
//! through the sanitiser, the inline (`cid:`) parts it may show, the remote
//! origins the user allowed, and the colours of the app theme. The page
//! the engine loads ([`page_html`]) wraps that HTML in Zmail's own `<head>`:
//! a Content Security Policy that repeats the one sent as a response header,
//! a no-referrer policy, DNS prefetching off, and a small base stylesheet.
//!
//! Nothing here is platform specific, so it is tested on every OS.

use std::{collections::HashMap, fmt::Write as _, net::IpAddr, sync::Arc};

use url::{Host, Url};

/// The scheme Zmail serves message pages from.
pub const MAIL_SCHEME: &str = "zmail-mail";

/// The scheme inline parts are referenced by in mail (`cid:<content-id>`).
/// The web view resolves it through the same handler as [`MAIL_SCHEME`].
pub const CID_SCHEME: &str = "cid";

/// The scheme of Zmail's link ids (`zmail-link:<n>`): the sanitiser
/// replaces every `href` with one, and a click on it comes back to Zmail.
pub const LINK_SCHEME: &str = "zmail-link";

/// A prepared message for the web renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct WebDocument {
    /// Sanitised HTML: a fragment or a whole document.
    pub html: String,
    /// Inline parts, by Content-ID.
    pub resources: Vec<InlineResource>,
    /// The remote origins images may load from, when the user allowed
    /// remote content for this message (`https://host[:port]`). Empty means
    /// remote content stays blocked.
    pub remote_origins: Vec<String>,
    /// The app theme the page is drawn in.
    pub appearance: Appearance,
}

/// An inline part of a message, served for `cid:` references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineResource {
    /// The Content-ID, without angle brackets.
    pub content_id: String,
    /// The part's media type, e.g. `image/png`.
    pub mime_type: String,
    /// The decoded bytes.
    pub data: Arc<[u8]>,
}

/// The theme colours the page uses, as `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    /// The app is in a dark theme.
    pub dark: bool,
    /// The message card's background.
    pub background: u32,
    /// Body text.
    pub foreground: u32,
    /// Links.
    pub link: u32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            dark: false,
            background: 0xffffff,
            foreground: 0x1f2328,
            link: 0x0969da,
        }
    }
}

impl WebDocument {
    /// A document with no inline parts and remote content blocked.
    pub fn new(html: impl Into<String>, appearance: Appearance) -> Self {
        Self {
            html: html.into(),
            resources: Vec::new(),
            remote_origins: Vec::new(),
            appearance,
        }
    }

    /// The remote origins that may actually load: [`Self::remote_origins`]
    /// with anything that is not a public `https` origin left out.
    pub fn allowed_origins(&self) -> Vec<String> {
        let mut origins: Vec<String> = self
            .remote_origins
            .iter()
            .filter_map(|origin| public_https_origin(origin))
            .collect();
        origins.sort();
        origins.dedup();
        origins
    }

    /// Whether the page is drawn dark: only in a dark theme, and only when
    /// the mail says it supports a dark colour scheme or sets no colours of
    /// its own (plain HTML, drawn in the theme's colours). Other mail is
    /// drawn as its author wrote it, on a light page, so dark text never
    /// lands on a dark background.
    pub fn draws_dark(&self) -> bool {
        self.appearance.dark && (supports_dark(&self.html) || !sets_colours(&self.html))
    }

    /// The inline parts by Content-ID, for the scheme handler.
    pub fn resources_by_id(&self) -> HashMap<String, InlineResource> {
        self.resources
            .iter()
            .map(|resource| (normalize_content_id(&resource.content_id), resource.clone()))
            .collect()
    }
}

/// The Content Security Policy for a page: the one from ADR 0006 with
/// `cid:` added (served by Zmail's scheme handler), plus the allowed remote
/// origins for images. `sandbox` is left out of the copy in the page's
/// `<meta>`, where browsers ignore it.
pub fn content_security_policy(allowed_origins: &[String], with_sandbox: bool) -> String {
    let mut images = format!("{MAIL_SCHEME}: {CID_SCHEME}: data:");
    for origin in allowed_origins {
        images.push(' ');
        images.push_str(origin);
    }
    let mut policy = format!(
        "default-src 'none'; img-src {images}; style-src 'unsafe-inline'; \
         form-action 'none'; base-uri 'none'"
    );
    if with_sandbox {
        policy.push_str("; sandbox");
    }
    policy
}

/// The page served for `document`: Zmail's head, then the mail.
///
/// The mail's own `<html>`, `<head>` and `<body>` tags end up inside this
/// page's body, where the HTML parser merges or drops them; its `<style>`
/// blocks still apply there. Zmail's policy `<meta>` comes first, before
/// any of the mail is parsed.
pub fn page_html(document: &WebDocument) -> String {
    let dark = document.draws_dark();
    let appearance = document.appearance;
    // A page not drawn dark is a light page even in a dark theme: the
    // mail's own colours were chosen for white paper.
    let (background, foreground, link) = if dark || !appearance.dark {
        (
            appearance.background,
            appearance.foreground,
            appearance.link,
        )
    } else {
        (0xffffff, 0x1f2328, 0x0969da)
    };
    let scheme = if dark { "dark" } else { "light" };
    let policy = content_security_policy(&document.allowed_origins(), false);
    let mut page = String::with_capacity(document.html.len() + 1024);
    page.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\">");
    let _ = write!(
        page,
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">"
    );
    page.push_str("<meta name=\"referrer\" content=\"no-referrer\">");
    page.push_str("<meta http-equiv=\"x-dns-prefetch-control\" content=\"off\">");
    let _ = write!(page, "<meta name=\"color-scheme\" content=\"{scheme}\">");
    let _ = write!(
        page,
        "<style id=\"zmail-base\">\
         :root{{color-scheme:{scheme}}}\
         html{{margin:0;padding:0;background:#{background:06x};color:#{foreground:06x};\
         overflow-y:hidden;overflow-x:auto;-webkit-text-size-adjust:100%}}\
         body{{margin:0;padding:4px 0 8px;\
         font:14px/1.45 -apple-system,BlinkMacSystemFont,\"Helvetica Neue\",sans-serif;\
         overflow-wrap:break-word}}\
         a{{color:#{link:06x}}}\
         img{{max-width:100%;height:auto}}\
         pre{{white-space:pre-wrap}}\
         </style>"
    );
    page.push_str("</head><body>");
    page.push_str(&document.html);
    page.push_str("</body></html>");
    page
}

/// Whether mail declares support for a dark colour scheme, through a
/// `prefers-color-scheme` query or a `color-scheme` that names `dark`.
pub fn supports_dark(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    if lower.contains("prefers-color-scheme") {
        return true;
    }
    let mut rest = lower.as_str();
    while let Some(at) = rest.find("color-scheme") {
        let after = &rest[at + "color-scheme".len()..];
        // `color-scheme: light dark` in CSS, or the meta tag's content.
        let declaration: String = after.chars().take(48).collect();
        let end = declaration
            .find([';', '}', '>', '\n'])
            .unwrap_or(declaration.len());
        if declaration[..end].contains("dark") {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether mail sets any colour or background of its own (CSS `color`,
/// `background`, or the `bgcolor`/`color`/`background` attributes), or has
/// images, which are drawn for a light page.
pub fn sets_colours(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    ["color", "background", "<img", "<svg"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// A Content-ID as the scheme handler looks it up: without angle brackets
/// or percent-encoding, and ASCII-lowercased (Content-IDs are matched
/// case-insensitively by most clients).
pub fn normalize_content_id(content_id: &str) -> String {
    percent_decode(content_id.trim())
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase()
}

/// What a request to Zmail's schemes asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemeRequest {
    /// The message page: `zmail-mail://message/<token>`.
    Page(String),
    /// An inline part: `cid:<content-id>`.
    Inline(String),
    /// Anything else, which is refused.
    Other,
}

/// Parses a URL the web view asked Zmail's scheme handler for.
pub fn parse_scheme_request(url: &str) -> SchemeRequest {
    let Some((scheme, rest)) = url.split_once(':') else {
        return SchemeRequest::Other;
    };
    if scheme.eq_ignore_ascii_case(CID_SCHEME) {
        let id = rest.split(['?', '#']).next().unwrap_or_default();
        if id.is_empty() {
            return SchemeRequest::Other;
        }
        return SchemeRequest::Inline(normalize_content_id(id));
    }
    if scheme.eq_ignore_ascii_case(MAIL_SCHEME)
        && let Some(path) = rest.strip_prefix("//message/")
    {
        let token = path.split(['?', '#']).next().unwrap_or_default();
        if !token.is_empty() && token.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return SchemeRequest::Page(token.to_owned());
        }
    }
    SchemeRequest::Other
}

/// The page URL for a load token.
pub fn page_url(token: &str) -> String {
    format!("{MAIL_SCHEME}://message/{token}")
}

/// The link id in a `zmail-link:<n>` URL.
pub fn parse_link_id(url: &str) -> Option<usize> {
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case(LINK_SCHEME) {
        return None;
    }
    rest.trim_start_matches('/').parse().ok()
}

/// `origin` as `https://host[:port]` when it is an https origin on a
/// public host: no IP literal in a private, loopback, link-local or
/// otherwise special range, no single-label or local-only name. Names
/// that resolve to private addresses cannot be caught here.
pub fn public_https_origin(origin: &str) -> Option<String> {
    let url = Url::parse(origin).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let public = match url.host()? {
        Host::Ipv4(ip) => is_public_ip(IpAddr::V4(ip)),
        Host::Ipv6(ip) => is_public_ip(IpAddr::V6(ip)),
        Host::Domain(name) => is_public_name(name),
    };
    if !public {
        return None;
    }
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("https://{host}:{port}"),
        None => format!("https://{host}"),
    })
}

fn is_public_name(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    const LOCAL_SUFFIXES: [&str; 7] = [
        ".local",
        ".localhost",
        ".internal",
        ".lan",
        ".home",
        ".home.arpa",
        ".localdomain",
    ];
    name.contains('.')
        && name != "localhost"
        && !LOCAL_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_multicast()
                // Carrier-grade NAT, 100.64.0.0/10.
                || (a == 100 && (64..128).contains(&b))
                // 0.0.0.0/8 and 240.0.0.0/4.
                || a == 0
                || a >= 240)
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                // Unique local fc00::/7 and link-local fe80::/10.
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || ip.to_ipv4_mapped().is_some_and(|v4| !is_public_ip(IpAddr::V4(v4))))
        }
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = text.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(html: &str, dark: bool) -> WebDocument {
        WebDocument::new(
            html,
            Appearance {
                dark,
                background: 0x101010,
                foreground: 0xeeeeee,
                link: 0x88aaff,
            },
        )
    }

    #[test]
    fn the_page_starts_with_zmail_policy_before_any_mail() {
        let page = page_html(&document(
            "<html><head><meta http-equiv=\"refresh\" content=\"0;url=https://x.example\"></head><body>hi</body></html>",
            false,
        ));
        let policy = page.find("Content-Security-Policy").expect("policy meta");
        let mail = page.find("refresh").expect("mail markup kept");
        assert!(policy < mail);
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("no-referrer"));
        assert!(page.contains("x-dns-prefetch-control"));
        assert!(
            !page.contains("sandbox"),
            "sandbox is only in the header copy"
        );
    }

    #[test]
    fn the_policy_allows_only_zmail_schemes_unless_origins_are_given() {
        assert_eq!(
            content_security_policy(&[], true),
            "default-src 'none'; img-src zmail-mail: cid: data:; style-src 'unsafe-inline'; \
             form-action 'none'; base-uri 'none'; sandbox"
        );
        let policy = content_security_policy(&["https://img.example".to_owned()], false);
        assert!(policy.contains("img-src zmail-mail: cid: data: https://img.example;"));
    }

    #[test]
    fn only_public_https_origins_are_allowed() {
        let mut doc = document("", false);
        doc.remote_origins = [
            "https://img.example.com",
            "https://img.example.com/",
            "https://cdn.example.com:8443/path",
            "http://plain.example.com",
            "https://localhost",
            "https://printer.local",
            "https://intranet",
            "https://10.0.0.1",
            "https://192.168.1.10",
            "https://127.0.0.1",
            "https://169.254.1.1",
            "https://100.64.0.1",
            "https://[::1]",
            "https://[fd00::1]",
            "https://[fe80::1]",
            "https://[::ffff:192.168.0.1]",
            "https://user:pw@img.example.com",
            "https://93.184.216.34",
            "file:///etc/passwd",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            doc.allowed_origins(),
            vec![
                "https://93.184.216.34",
                "https://cdn.example.com:8443",
                "https://img.example.com",
            ]
        );
    }

    #[test]
    fn dark_is_drawn_only_for_mail_that_supports_it() {
        assert!(document("<p>plain</p>", true).draws_dark());
        assert!(!document("<p style=\"color:#333\">grey</p>", true).draws_dark());
        assert!(!document("<td bgcolor=\"#fff\">cell</td>", true).draws_dark());
        assert!(!document("<p>logo <img src=\"cid:x\"></p>", true).draws_dark());
        assert!(!document("<p>plain</p>", false).draws_dark());
        assert!(
            document("<style>@media (prefers-color-scheme: dark){}</style>", true).draws_dark()
        );
        assert!(document("<meta name=\"color-scheme\" content=\"light dark\">", true).draws_dark());
        assert!(document("<style>:root{color-scheme: light dark;}</style>", true).draws_dark());
        assert!(!document("<style>:root{color-scheme: light;} .dark{}</style>", true).draws_dark());
        assert!(
            !document(
                "<style>@media (prefers-color-scheme: dark){}</style>",
                false
            )
            .draws_dark()
        );
    }

    #[test]
    fn mail_without_dark_support_gets_a_light_page_in_a_dark_theme() {
        let page = page_html(&document("<p style=\"color:#333\">text</p>", true));
        assert!(page.contains("color-scheme:light"));
        assert!(page.contains("background:#ffffff"));
        let page = page_html(&document(
            "<style>@media (prefers-color-scheme: dark){p{color:#eee}}</style>",
            true,
        ));
        assert!(page.contains("color-scheme:dark"));
        assert!(page.contains("background:#101010"));
    }

    #[test]
    fn scheme_requests_are_parsed_strictly() {
        assert_eq!(
            parse_scheme_request("zmail-mail://message/a1b2"),
            SchemeRequest::Page("a1b2".to_owned())
        );
        assert_eq!(
            parse_scheme_request("cid:Part1.ABC@example.com"),
            SchemeRequest::Inline("part1.abc@example.com".to_owned())
        );
        assert_eq!(
            parse_scheme_request("cid:%3Cimg%40x%3E"),
            SchemeRequest::Inline("img@x".to_owned())
        );
        for other in [
            "zmail-mail://message/",
            "zmail-mail://message/../etc",
            "zmail-mail://other/a1",
            "cid:",
            "https://example.com/",
            "file:///etc/passwd",
            "nonsense",
        ] {
            assert_eq!(parse_scheme_request(other), SchemeRequest::Other, "{other}");
        }
    }

    #[test]
    fn link_ids_come_only_from_the_link_scheme() {
        assert_eq!(parse_link_id("zmail-link:3"), Some(3));
        assert_eq!(parse_link_id("ZMAIL-LINK:12"), Some(12));
        assert_eq!(parse_link_id("zmail-link:x"), None);
        assert_eq!(parse_link_id("https://example.com/3"), None);
    }

    #[test]
    fn content_ids_match_without_brackets_or_case() {
        let mut doc = document("", false);
        doc.resources.push(InlineResource {
            content_id: "<Logo@Example.COM>".to_owned(),
            mime_type: "image/png".to_owned(),
            data: Arc::from(&b"png"[..]),
        });
        assert!(doc.resources_by_id().contains_key("logo@example.com"));
    }
}
