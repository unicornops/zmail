//! The HTML mail sanitiser (ADR 0006, "Preparation is the same for every
//! renderer").
//!
//! [`sanitize_html`] turns a message's original `text/html` part into
//! markup any renderer may be given, plus everything a renderer needs
//! besides it:
//!
//! 1. A tokenizer pre-pass checks the caps ([`MAX_HTML_BYTES`],
//!    [`MAX_ELEMENTS`], nesting, [`MAX_STYLE_BYTES`]) in linear time and
//!    collects the `<style>` blocks. Mail over a cap is "too complex": it
//!    gets no HTML, only text, so renderers fall back to the text view.
//! 2. ammonia parses the markup with html5ever's tree builder (iteratively:
//!    deep nesting cannot overflow the stack) and keeps an allowlist of
//!    elements and attributes: tables and their presentational attributes,
//!    `style`, images, text formatting. Scripts, event handlers, frames,
//!    objects, `<base>`, forms, `<meta>`, `<link>`, SVG and MathML go,
//!    with the contents of the ones that are not text.
//! 3. Every address is rewritten (see `policy`): an `href` becomes an
//!    opaque `zmail-link:<n>` id whose target stays in
//!    [`SanitizedDocument::links`], so engines never navigate; a remote
//!    image or background becomes `zmail-blocked:<n>` unless the user
//!    allowed remote content and it is a public `https:` address; `cid:`
//!    parts stay; everything else (`javascript:`, `data:` other than a few
//!    image types on `<img>`, `file:`, UNC paths, relative addresses) is
//!    dropped.
//! 4. CSS, inline and in `<style>`, goes through the CSS pass (`css`).
//! 5. The text derivation (`text`) reads the result into the text view's
//!    paragraphs and link ranges.
//!
//! Nothing here loads anything.

mod css;
mod elements;
mod policy;
mod prepass;
mod text;

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt,
    ops::Range,
    sync::{Arc, Mutex, PoisonError},
};

use policy::{ResourceUse, UrlPolicy};
pub use policy::{is_public_ip, is_public_remote_url};
use prepass::PrePass;

/// The scheme of the opaque link ids that replace every `href`:
/// `zmail-link:<n>`, where `n` indexes [`SanitizedDocument::links`].
pub const LINK_SCHEME: &str = "zmail-link";

/// The scheme of blocked remote resources: `zmail-blocked:<n>`, where `n`
/// indexes [`SanitizedDocument::remote`]. It never loads.
pub const BLOCKED_SCHEME: &str = "zmail-blocked";

/// The largest HTML part the sanitiser parses. Gmail clips mail at about
/// 100 KB; this is twenty times that.
pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;

/// The most elements (start tags) a sanitised message may have.
pub const MAX_ELEMENTS: usize = 50_000;

/// The deepest element nesting a sanitised message may have. Real mail
/// rarely passes 40; WebKit's parser stops nesting at 512.
pub const MAX_DEPTH: usize = 256;

/// The nesting the pre-pass refuses before parsing. Its count is
/// approximate (it applies only the common implied end tags), so it is
/// looser than [`MAX_DEPTH`], which is checked exactly afterwards.
const MAX_PREPASS_DEPTH: usize = 4 * MAX_DEPTH;

/// The most CSS, across all `<style>` blocks, a sanitised message may
/// have. Gmail drops styles past 16 KB.
pub const MAX_STYLE_BYTES: usize = 256 * 1024;

/// The most characters of a link's text kept in [`LinkTarget::text`].
pub const MAX_LINK_TEXT_CHARS: usize = 512;

/// The most markup the text derivation reads from mail too complex to
/// sanitise; the rest is cut.
pub const MAX_TEXT_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Elements kept (unwrapped elements keep their text).
const ALLOWED_ELEMENTS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "article",
    "aside",
    "b",
    "bdi",
    "bdo",
    "big",
    "blockquote",
    "br",
    "caption",
    "center",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "details",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "font",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "main",
    "mark",
    "nav",
    "ol",
    "p",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "section",
    "small",
    "span",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
];

/// Elements removed together with their contents: code, foreign documents
/// and form controls whose text is not message text. (`<style>` blocks are
/// put back by the CSS pass.)
const REMOVED_WITH_CONTENTS: &[&str] = &[
    "applet", "embed", "frame", "frameset", "head", "iframe", "math", "noembed", "noframes",
    "noscript", "object", "script", "select", "style", "svg", "template", "textarea", "title",
    "xmp",
];

/// Attributes kept on every allowed element. `background` and `style` are
/// rewritten by the address and CSS passes.
const GENERIC_ATTRIBUTES: &[&str] = &[
    "align",
    "background",
    "bgcolor",
    "border",
    "cellpadding",
    "cellspacing",
    "class",
    "colspan",
    "dir",
    "height",
    "id",
    "lang",
    "role",
    "rowspan",
    "style",
    "title",
    "valign",
    "width",
];

/// Attributes kept on particular elements.
const ELEMENT_ATTRIBUTES: &[(&str, &[&str])] = &[
    ("a", &["href"]),
    ("img", &["src", "alt", "hspace", "vspace"]),
    ("font", &["color", "face", "size"]),
    ("ol", &["start", "type", "reversed"]),
    ("ul", &["type"]),
    ("li", &["value", "type"]),
    ("td", &["abbr", "axis", "headers", "nowrap", "scope"]),
    ("th", &["abbr", "axis", "headers", "nowrap", "scope"]),
    ("col", &["span"]),
    ("colgroup", &["span"]),
    ("table", &["frame", "rules", "summary"]),
    ("hr", &["noshade", "size"]),
    ("br", &["clear"]),
    ("details", &["open"]),
    ("time", &["datetime"]),
    ("del", &["datetime"]),
    ("ins", &["datetime"]),
];

/// How to sanitise one message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SanitizeOptions {
    /// The user allowed remote content for this message: public `https:`
    /// resources keep their address instead of a blocked placeholder.
    pub allow_remote: bool,
    /// The markup is a body stored before ADR 0006, whose links the old
    /// sanitiser rewrote to `blocked://host/path`; read those as `https:`.
    pub legacy_blocked_links: bool,
}

/// Why a message gets no sanitised HTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    /// The HTML part is larger than [`MAX_HTML_BYTES`].
    InputTooLarge,
    /// More than [`MAX_ELEMENTS`] elements.
    TooManyElements,
    /// Nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// More than [`MAX_STYLE_BYTES`] of `<style>`.
    StyleTooLarge,
}

impl fmt::Display for Complexity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InputTooLarge => "the message's HTML is too large",
            Self::TooManyElements => "the message's HTML has too many elements",
            Self::TooDeep => "the message's HTML is nested too deeply",
            Self::StyleTooLarge => "the message's styles are too large",
        })
    }
}

/// Where a link in a message points: the original target, with its
/// scheme (`http`, `https`, `mailto`, `tel` or `sms`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTarget {
    /// The target URL, normalised by the URL parser (punycode hosts,
    /// lowercase scheme and host).
    pub url: String,
    /// The link's visible text as the text derivation reads it, spaces
    /// collapsed and cut at [`MAX_LINK_TEXT_CHARS`], for the look-alike
    /// warning. Empty for a link with no text (an image).
    pub text: String,
}

impl LinkTarget {
    /// The target's scheme, in lowercase.
    pub fn scheme(&self) -> &str {
        self.url.split_once(':').map_or("", |(scheme, _)| scheme)
    }
}

/// A remote resource (image, background) a message references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteResource {
    /// The resource's address.
    pub url: String,
    /// Its origin (`https://host[:port]`).
    pub origin: String,
    /// Whether the sanitised HTML references it directly, so it loads: the
    /// user allowed remote content and the address passes
    /// [`is_public_remote_url`]. Otherwise the HTML has
    /// `zmail-blocked:<index>` in its place.
    pub loads: bool,
}

/// A link's text in the derived text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLink {
    /// The link text, as a byte range of the derived text. A link whose
    /// text spans lines has one range per line.
    pub range: Range<usize>,
    /// The link, as an index of [`SanitizedDocument::links`].
    pub link: usize,
}

/// A sanitised message: HTML a renderer may be given (unless the message
/// was too complex), its text, and the side tables the HTML refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedDocument {
    /// The sanitised HTML: a fragment, beginning with the sanitised
    /// `<style>` block when the mail had styles. `None` when the message
    /// is too complex.
    pub html: Option<String>,
    /// Why `html` is `None`.
    pub too_complex: Option<Complexity>,
    /// The readable text of the message.
    pub text: String,
    /// Where each run of link text in `text` points.
    pub text_links: Vec<TextLink>,
    /// Link targets, indexed by the `n` of `zmail-link:<n>`.
    pub links: Vec<LinkTarget>,
    /// Remote resources, indexed by the `n` of `zmail-blocked:<n>`.
    pub remote: Vec<RemoteResource>,
    /// The Content-IDs the HTML references through `cid:`, without angle
    /// brackets, in order of first use.
    pub cid_references: Vec<String>,
}

impl SanitizedDocument {
    /// Whether the message references remote content (whether or not it
    /// loads).
    pub fn has_remote_content(&self) -> bool {
        !self.remote.is_empty()
    }
}

/// Sanitises a message's HTML part; see the module documentation.
pub fn sanitize_html(input: &str, options: &SanitizeOptions) -> SanitizedDocument {
    sanitize(input, UrlPolicy::new(options))
}

/// Markup for the store: sanitised with remote content blocked, every
/// `href` keeping its checked target (so the stored markup needs no side
/// table), and the readable text. Mail too complex to sanitise is stored as
/// its text, with its links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHtml {
    /// The markup to store.
    pub html: String,
    /// The readable text of the message.
    pub text: String,
    /// Whether the message references remote content.
    pub has_remote_content: bool,
}

/// Sanitises a message's HTML part for the store; see [`StoredHtml`].
pub fn sanitize_html_for_storage(input: &str) -> StoredHtml {
    let mut policy = UrlPolicy::new(&SanitizeOptions::default());
    policy.inline_targets = true;
    let document = sanitize(input, policy);
    let html = match document.html {
        Some(html) => html,
        None => text_as_html(&document.text, &document.text_links, &document.links),
    };
    StoredHtml {
        html,
        has_remote_content: !document.remote.is_empty(),
        text: document.text,
    }
}

/// A link's text in the text of stored markup, with its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTextLink {
    /// The link text, as a byte range of the text.
    pub range: Range<usize>,
    /// The link's target.
    pub target: LinkTarget,
}

/// The readable text of stored markup ([`StoredHtml::html`]), with its
/// links. `legacy` reads a body stored before ADR 0006: the old
/// sanitiser's output, which may be malformed and whose remote links read
/// `blocked://host/path`.
pub fn text_of_stored_html(html: &str, legacy: bool) -> (String, Vec<StoredTextLink>) {
    let mut policy = UrlPolicy::new(&SanitizeOptions {
        allow_remote: false,
        legacy_blocked_links: legacy,
    });
    let html = &html[..html.floor_char_boundary(MAX_TEXT_INPUT_BYTES)];
    let derived = text::derive(
        html,
        if legacy {
            text::Markup::Unsanitized(&mut policy)
        } else {
            text::Markup::SanitizedWithTargets(&mut policy)
        },
    );
    let links = derived
        .links
        .into_iter()
        .filter_map(|link| {
            Some(StoredTextLink {
                target: policy.links.get(link.link)?.clone(),
                range: link.range,
            })
        })
        .collect();
    (derived.text, links)
}

fn sanitize(input: &str, mut policy: UrlPolicy) -> SanitizedDocument {
    if input.len() > MAX_HTML_BYTES {
        return too_complex(input, Complexity::InputTooLarge, policy);
    }
    let prepass = PrePass::run(input);
    if let Some(complexity) = prepass.complexity() {
        return too_complex(input, complexity, policy);
    }

    let mut html = String::with_capacity(input.len());
    let styles: Vec<String> = prepass
        .styles
        .iter()
        .map(|style| css::sanitize_stylesheet(style, &mut policy))
        .filter(|style| !style.is_empty())
        .collect();
    if !styles.is_empty() {
        html.push_str("<style>");
        html.push_str(&styles.join("\n"));
        html.push_str("</style>");
    }
    let body_style = body_style(&prepass.body_attributes, &mut policy);

    let legacy_blocked_links = policy.legacy_blocked_links();
    let shared = Arc::new(Mutex::new(policy));
    let body = clean(input, legacy_blocked_links, Arc::clone(&shared));
    let mut policy = match Arc::try_unwrap(shared) {
        Ok(mutex) => mutex.into_inner().unwrap_or_else(PoisonError::into_inner),
        // The builder, and the filter's clone with it, is dropped by now.
        Err(shared) => std::mem::take(&mut *shared.lock().unwrap_or_else(PoisonError::into_inner)),
    };

    match &body_style {
        Some(style) => {
            html.push_str("<div style=\"");
            html.push_str(&escape_attribute(style));
            html.push_str("\">");
            html.push_str(&body);
            html.push_str("</div>");
        }
        None => html.push_str(&body),
    }

    let derived = if policy.inline_targets {
        // Links are recorded again by the derivation; only its text
        // matters here.
        let mut links = UrlPolicy::new(&SanitizeOptions::default());
        let derived = text::derive(&html, text::Markup::SanitizedWithTargets(&mut links));
        policy.links = links.links;
        derived
    } else {
        text::derive(&html, text::Markup::Sanitized)
    };
    fill_link_texts(&mut policy.links, &derived.text, &derived.links);
    let (html, too_complex) = if derived.deepest > MAX_DEPTH {
        (None, Some(Complexity::TooDeep))
    } else {
        (Some(html), None)
    };
    SanitizedDocument {
        html,
        too_complex,
        text: derived.text,
        text_links: derived.links,
        links: policy.links,
        remote: policy.remote,
        cid_references: policy.cid_references,
    }
}

/// The text of mail too complex to sanitise, read from the original markup
/// (cut at [`MAX_TEXT_INPUT_BYTES`]), with its links.
fn too_complex(input: &str, complexity: Complexity, mut policy: UrlPolicy) -> SanitizedDocument {
    let input = &input[..input.floor_char_boundary(MAX_TEXT_INPUT_BYTES)];
    let derived = text::derive(input, text::Markup::Unsanitized(&mut policy));
    fill_link_texts(&mut policy.links, &derived.text, &derived.links);
    SanitizedDocument {
        html: None,
        too_complex: Some(complexity),
        text: derived.text,
        text_links: derived.links,
        links: policy.links,
        remote: Vec::new(),
        cid_references: Vec::new(),
    }
}

/// Gives each link the text of its runs in the derived text.
fn fill_link_texts(links: &mut [LinkTarget], text: &str, text_links: &[TextLink]) {
    for run in text_links {
        let (Some(link), Some(piece)) = (links.get_mut(run.link), text.get(run.range.clone()))
        else {
            continue;
        };
        if !link.text.is_empty() {
            link.text.push(' ');
        }
        link.text.push_str(piece);
    }
    for link in links {
        let collapsed = link.text.split_whitespace().collect::<Vec<_>>().join(" ");
        link.text = collapsed.chars().take(MAX_LINK_TEXT_CHARS).collect();
    }
}

/// Markup for derived text and its links: escaped lines, paragraphs at
/// blank lines, links as `<a href>` with their targets. Its own derived
/// text is `text` again.
fn text_as_html(text: &str, text_links: &[TextLink], links: &[LinkTarget]) -> String {
    let mut html = String::with_capacity(text.len() + 64);
    html.push_str("<p>");
    let mut cursor = 0;
    let write_text = |html: &mut String, piece: &str| {
        let mut lines = piece.split('\n').peekable();
        while let Some(line) = lines.next() {
            html.push_str(&escape_attribute(line));
            if lines.peek().is_some() {
                html.push_str("<br>");
            }
        }
    };
    for link in text_links {
        let Some(target) = links.get(link.link) else {
            continue;
        };
        if link.range.start < cursor || link.range.end > text.len() {
            continue;
        }
        write_text(&mut html, &text[cursor..link.range.start]);
        html.push_str("<a href=\"");
        html.push_str(&escape_attribute(&target.url));
        html.push_str("\">");
        write_text(&mut html, &text[link.range.clone()]);
        html.push_str("</a>");
        cursor = link.range.end;
    }
    write_text(&mut html, &text[cursor..]);
    html.push_str("</p>");
    html
}

/// Runs ammonia over `input` with the allowlist, rewriting addresses and
/// styles through `policy`.
fn clean(input: &str, legacy_blocked_links: bool, policy: Arc<Mutex<UrlPolicy>>) -> String {
    let element_attributes: HashMap<&str, HashSet<&str>> = ELEMENT_ATTRIBUTES
        .iter()
        .map(|(element, attributes)| (*element, attributes.iter().copied().collect()))
        .collect();
    // ammonia checks `href` and `src` schemes against this list before
    // the filter below decides; the filter is the real policy.
    let mut url_schemes: HashSet<&str> = ["http", "https", "mailto", "tel", "sms", "cid", "data"]
        .into_iter()
        .collect();
    if legacy_blocked_links {
        url_schemes.insert("blocked");
    }

    let mut builder = ammonia::Builder::empty();
    builder
        .tags(ALLOWED_ELEMENTS.iter().copied().collect())
        .clean_content_tags(REMOVED_WITH_CONTENTS.iter().copied().collect())
        .generic_attributes(GENERIC_ATTRIBUTES.iter().copied().collect())
        .generic_attribute_prefixes(["aria-"].into_iter().collect())
        .tag_attributes(element_attributes)
        .url_schemes(url_schemes)
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(None)
        .strip_comments(true)
        .attribute_filter(move |element, attribute, value| {
            let mut policy = policy.lock().unwrap_or_else(PoisonError::into_inner);
            match (element, attribute) {
                ("a", "href") => policy.link(value).map(Cow::Owned),
                ("img", "src") => policy.resource(value, ResourceUse::Image).map(Cow::Owned),
                (_, "background") => policy
                    .resource(value, ResourceUse::Background)
                    .map(Cow::Owned),
                (_, "style") => {
                    let style = css::sanitize_declarations(value, &mut policy);
                    (!style.is_empty()).then_some(Cow::Owned(style))
                }
                _ => Some(Cow::Borrowed(value)),
            }
        });
    builder.clean(input).to_string()
}

/// The `<body>` tag's colours and background as a style for the wrapper
/// that stands in for it (fragment parsing drops the tag).
fn body_style(attributes: &[(String, String)], policy: &mut UrlPolicy) -> Option<String> {
    let mut style = String::new();
    for (name, value) in attributes {
        match name.as_str() {
            "bgcolor" if is_plain_colour(value) => {
                style.push_str(&format!("background-color:{};", value.trim()));
            }
            "text" if is_plain_colour(value) => {
                style.push_str(&format!("color:{};", value.trim()));
            }
            "background" => {
                if let Some(url) = policy.resource(value, ResourceUse::Background) {
                    let mut quoted = String::new();
                    let _ = cssparser::serialize_string(&url, &mut quoted);
                    style.push_str(&format!("background-image:url({quoted});"));
                }
            }
            _ => {}
        }
    }
    if let Some((_, value)) = attributes.iter().find(|(name, _)| name == "style") {
        style.push_str(&css::sanitize_declarations(value, policy));
    }
    (!style.is_empty()).then_some(style)
}

/// A colour attribute value that is a name or a hex code, nothing else.
fn is_plain_colour(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '#')
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests;
