//! What a web view is given to show, and the page Zmail serves for it.
//!
//! A [`WebDocument`] is a prepared message: HTML that has already been
//! through the sanitiser, the inline (`cid:`) parts it may show, the remote
//! images the user allowed (fetched by Zmail, never by the engine: see
//! `remote`), and the colours of the app theme. The page
//! the engine loads ([`page_html`]) wraps that HTML in Zmail's own `<head>`:
//! a Content Security Policy that repeats the one sent as a response header,
//! a no-referrer policy, DNS prefetching off, and a small base stylesheet.
//!
//! Nothing here is platform specific, so it is tested on every OS.

use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use zmail_render::{
    BLOCKED_SCHEME, HtmlUnavailable, PreparedMessage, RemoteContentDecision, is_public_remote_url,
};

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
    /// The remote resources Zmail may fetch for the page, by the `n` of
    /// `zmail-mail://remote/<n>` (the index of the prepared message's remote
    /// resource): `Some(url)` for an allowed public `https` image, `None`
    /// otherwise. Empty while remote content is blocked.
    pub remote: Vec<Option<String>>,
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
    /// The document for a prepared message: its sanitised HTML, its inline
    /// image parts (a part that cannot be read, or is not an image, is
    /// left out) and, when `remote` allows it, its remote images.
    ///
    /// `prepared` must have been prepared with remote content blocked, so
    /// its HTML has a `zmail-blocked:<n>` placeholder for every remote
    /// resource. When the user allowed remote content, each placeholder of
    /// a public `https` resource becomes `zmail-mail://remote/<n>`, which
    /// Zmail's scheme handler fetches itself (`crate::remote`): the engine
    /// never connects anywhere. Other placeholders stay blocked.
    pub fn from_prepared(
        prepared: &PreparedMessage,
        appearance: Appearance,
        remote: RemoteContentDecision,
    ) -> Result<Self, HtmlUnavailable> {
        let mut html = prepared.html()?.to_owned();
        let resources = prepared
            .inline_parts
            .iter()
            .filter(|part| part.content_type.to_ascii_lowercase().starts_with("image/"))
            .filter_map(|part| match part.read() {
                Ok(bytes) => Some(InlineResource {
                    content_id: part.content_id.clone(),
                    mime_type: part.content_type.to_ascii_lowercase(),
                    data: Arc::from(bytes),
                }),
                Err(error) => {
                    eprintln!(
                        "zmail: web renderer: inline part {} could not be read: {error:#}",
                        part.content_id
                    );
                    None
                }
            })
            .collect();
        let remote = if remote.allows() {
            let fetchable: Vec<Option<String>> = prepared
                .remote
                .resources
                .iter()
                .map(|resource| is_public_remote_url(&resource.url).then(|| resource.url.clone()))
                .collect();
            html = rewrite_blocked(&html, |index| {
                fetchable
                    .get(index)
                    .is_some_and(Option::is_some)
                    .then(|| remote_url(index))
            });
            fetchable
        } else {
            Vec::new()
        };
        Ok(Self {
            html,
            resources,
            remote,
            appearance,
        })
    }

    /// A document with no inline parts and remote content blocked.
    pub fn new(html: impl Into<String>, appearance: Appearance) -> Self {
        Self {
            html: html.into(),
            resources: Vec::new(),
            remote: Vec::new(),
            appearance,
        }
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
/// `cid:` added (served by Zmail's scheme handler). Remote images, when
/// allowed, also come from Zmail's scheme, so no remote origin is ever
/// listed. `sandbox` is left out of the copy in the page's `<meta>`, where
/// browsers ignore it.
pub fn content_security_policy(with_sandbox: bool) -> String {
    let images = format!("{MAIL_SCHEME}: {CID_SCHEME}: data:");
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
    let policy = content_security_policy(false);
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
    /// A remote image Zmail fetches: `zmail-mail://remote/<n>`.
    Remote(usize),
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
        && let Some(path) = rest.strip_prefix("//remote/")
    {
        let index = path.split(['?', '#']).next().unwrap_or_default();
        if !index.is_empty() && index.chars().all(|ch| ch.is_ascii_digit()) {
            return index
                .parse()
                .map_or(SchemeRequest::Other, SchemeRequest::Remote);
        }
        return SchemeRequest::Other;
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

/// The address the page loads remote resource `index` from.
pub fn remote_url(index: usize) -> String {
    format!("{MAIL_SCHEME}://remote/{index}")
}

/// Replaces each `zmail-blocked:<n>` placeholder in `html` that `replace`
/// gives an address for; the others stay blocked.
fn rewrite_blocked(html: &str, replace: impl Fn(usize) -> Option<String>) -> String {
    let prefix = format!("{BLOCKED_SCHEME}:");
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find(&prefix) {
        out.push_str(&rest[..at]);
        let after = &rest[at + prefix.len()..];
        let digits = after
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(after.len());
        let replacement = after[..digits]
            .parse()
            .ok()
            .filter(|_| digits > 0)
            .and_then(&replace);
        match replacement {
            Some(address) => out.push_str(&address),
            None => {
                out.push_str(&prefix);
                out.push_str(&after[..digits]);
            }
        }
        rest = &after[digits..];
    }
    out.push_str(rest);
    out
}

/// The link id in a `zmail-link:<n>` URL.
pub fn parse_link_id(url: &str) -> Option<usize> {
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case(LINK_SCHEME) {
        return None;
    }
    rest.trim_start_matches('/').parse().ok()
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
    fn the_policy_allows_only_zmail_schemes() {
        assert_eq!(
            content_security_policy(true),
            "default-src 'none'; img-src zmail-mail: cid: data:; style-src 'unsafe-inline'; \
             form-action 'none'; base-uri 'none'; sandbox"
        );
        assert!(!content_security_policy(false).contains("sandbox"));
    }

    #[test]
    fn only_allowed_placeholders_are_rewritten() {
        let html = "<img src=\"zmail-blocked:0\"><img src=\"zmail-blocked:12\">\
                    <p style=\"background:url(zmail-blocked:1)\">zmail-blocked:x zmail-blocked:</p>";
        let rewritten = rewrite_blocked(html, |index| (index != 1).then(|| remote_url(index)));
        assert_eq!(
            rewritten,
            "<img src=\"zmail-mail://remote/0\"><img src=\"zmail-mail://remote/12\">\
             <p style=\"background:url(zmail-blocked:1)\">zmail-blocked:x zmail-blocked:</p>"
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
        assert_eq!(
            parse_scheme_request("zmail-mail://remote/7"),
            SchemeRequest::Remote(7)
        );
        for other in [
            "zmail-mail://message/",
            "zmail-mail://message/../etc",
            "zmail-mail://other/a1",
            "zmail-mail://remote/",
            "zmail-mail://remote/-1",
            "zmail-mail://remote/1x",
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

    struct Bytes(&'static [u8]);

    impl zmail_render::PartBytes for Bytes {
        fn read(&self) -> anyhow::Result<Vec<u8>> {
            Ok(self.0.to_vec())
        }
    }

    #[test]
    fn a_prepared_message_brings_its_html_images_and_allowed_remote_images() {
        use zmail_render::{InlinePart, PrepareOptions, RenderSource, prepare};
        let source = RenderSource {
            message_id: 7,
            body_text: None,
            html: Some(
                "<p><a href=\"https://example.com/x\">x</a><img src=\"cid:logo@x\">\
                 <img src=\"https://img.example.com/a.png\"></p>"
                    .to_owned(),
            ),
            html_is_legacy: false,
            inline_parts: vec![
                InlinePart::new("logo@x", "IMAGE/PNG", 3, Arc::new(Bytes(b"png"))),
                InlinePart::new("doc@x", "application/pdf", 3, Arc::new(Bytes(b"pdf"))),
            ],
        };
        let prepared = prepare(&source, &PrepareOptions::default());
        let document = WebDocument::from_prepared(
            &prepared,
            Appearance::default(),
            RemoteContentDecision::Blocked,
        )
        .expect("has HTML");
        assert!(document.html.contains("zmail-link:0"), "{}", document.html);
        assert!(
            document.html.contains("zmail-blocked:0"),
            "{}",
            document.html
        );
        assert!(
            !document.html.contains("img.example.com"),
            "{}",
            document.html
        );
        assert!(document.remote.is_empty());
        assert_eq!(document.resources.len(), 1);
        assert_eq!(document.resources[0].mime_type, "image/png");

        // Allowed: the image comes from Zmail's scheme, never from its host.
        let document = WebDocument::from_prepared(
            &prepared,
            Appearance::default(),
            RemoteContentDecision::AllowedForMessage,
        )
        .expect("has HTML");
        assert!(
            document.html.contains("src=\"zmail-mail://remote/0\""),
            "{}",
            document.html
        );
        assert!(
            !document.html.contains("img.example.com"),
            "{}",
            document.html
        );
        assert_eq!(
            document.remote,
            [Some("https://img.example.com/a.png".to_owned())]
        );

        let plain = prepare(
            &RenderSource {
                message_id: 8,
                body_text: Some("text".to_owned()),
                ..RenderSource::default()
            },
            &PrepareOptions::default(),
        );
        assert!(
            WebDocument::from_prepared(
                &plain,
                Appearance::default(),
                RemoteContentDecision::Blocked
            )
            .is_err()
        );
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
