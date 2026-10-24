//! The boundary between a message and whatever draws it (ADR 0006).
//!
//! Every renderer (the text view today, the OS web engines and the native
//! renderer later) is given the same [`PreparedMessage`], made by
//! [`prepare`] from what the store holds for a message ([`RenderSource`],
//! read with [`render_source`]) and the user's choices
//! ([`PrepareOptions`]):
//!
//! - **HTML**: sanitised by `zmail_security::html` (allowlisted elements
//!   and attributes, a CSS pass, size and nesting caps), or a reason there
//!   is none ([`HtmlUnavailable`]): no HTML part, or too complex to
//!   sanitise, which sends renderers to the text.
//! - **Text**: the plain-text alternative when the message has one,
//!   otherwise text derived from the HTML, with the ranges of its links.
//! - **Links**: every `href` in the HTML is an opaque `zmail-link:<n>`
//!   ([`LINK_SCHEME`]). A renderer never navigates: it hands the id back,
//!   and Zmail shows [`PreparedMessage::link`]'s real target in its
//!   confirmation dialog (with the look-alike warning) before opening it.
//! - **Inline parts**: `cid:` addresses are kept in the HTML, and
//!   [`PreparedMessage::inline_part`] finds each part's content type and
//!   bytes by Content-ID.
//! - **Remote content**: [`RemoteContent`] lists the remote resources the
//!   mail references and the user's decision. Blocked resources read
//!   `zmail-blocked:<n>` ([`BLOCKED_SCHEME`]) in the HTML and must never
//!   load. When the user allowed remote content, public `https:` resources
//!   keep their address; a renderer may load exactly those, from
//!   [`RemoteContent::allowed_origins`], and must still refuse any address
//!   that resolves to a private network ([`is_public_remote_url`] checks
//!   the address as written).
//! - **Theme**: a [`ThemeHint`] for the renderer's colours.
//!
//! Preparing loads nothing and touches no network.

use std::{fmt, sync::Arc};

pub use zmail_security::html::{
    BLOCKED_SCHEME, Complexity, LINK_SCHEME, LinkTarget, RemoteResource, TextLink, is_public_ip,
    is_public_remote_url,
};
use zmail_security::html::{SanitizeOptions, sanitize_html};

/// Reads the bytes of an inline part, which the store keeps encrypted.
pub trait PartBytes: Send + Sync {
    /// Decrypts and returns the part's bytes.
    fn read(&self) -> anyhow::Result<Vec<u8>>;
}

/// A part of the message addressable by Content-ID (an inline image, for
/// `cid:` in the HTML).
#[derive(Clone)]
pub struct InlinePart {
    /// The Content-ID, without angle brackets.
    pub content_id: String,
    /// The part's MIME content type, from the sender: check it before
    /// trusting it.
    pub content_type: String,
    /// The part's size in bytes.
    pub size_bytes: u64,
    bytes: Arc<dyn PartBytes>,
}

impl InlinePart {
    /// A part whose bytes `bytes` reads.
    pub fn new(
        content_id: impl Into<String>,
        content_type: impl Into<String>,
        size_bytes: u64,
        bytes: Arc<dyn PartBytes>,
    ) -> Self {
        Self {
            content_id: content_id.into(),
            content_type: content_type.into(),
            size_bytes,
            bytes,
        }
    }

    /// The part's bytes.
    pub fn read(&self) -> anyhow::Result<Vec<u8>> {
        self.bytes.read()
    }
}

impl fmt::Debug for InlinePart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InlinePart")
            .field("content_id", &self.content_id)
            .field("content_type", &self.content_type)
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

/// What the store holds for one message, as a renderer needs it.
#[derive(Debug, Clone, Default)]
pub struct RenderSource {
    /// The message's local id.
    pub message_id: i64,
    /// The plain-text body, when the message has one.
    pub body_text: Option<String>,
    /// The original `text/html` part, when the message has one.
    pub html: Option<String>,
    /// `html` is a body stored before ADR 0006 (the old sanitiser's
    /// output, its links rewritten to `blocked://`), not the original
    /// part. It renders, but with no remote content or inline parts until
    /// the message is fetched again.
    pub html_is_legacy: bool,
    /// The message's inline parts.
    pub inline_parts: Vec<InlinePart>,
    /// The user chose "Always for this sender" for the message's sender
    /// (stored per account).
    pub remote_content_sender_allowed: bool,
}

/// Whether a message's remote content may load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RemoteContentDecision {
    /// Blocked: the default.
    #[default]
    Blocked,
    /// The user chose "Load remote content" for this message.
    AllowedForMessage,
    /// The user chose "Always for this sender" for the message's sender.
    AllowedForSender,
}

impl RemoteContentDecision {
    /// The decision for a message: allowed for its sender (stored), else
    /// for this message ("Load remote content", for the session), else
    /// blocked.
    pub fn new(allowed_for_message: bool, allowed_for_sender: bool) -> Self {
        if allowed_for_sender {
            Self::AllowedForSender
        } else if allowed_for_message {
            Self::AllowedForMessage
        } else {
            Self::Blocked
        }
    }

    /// Whether remote content may load.
    pub fn allows(self) -> bool {
        !matches!(self, Self::Blocked)
    }
}

/// The colours the renderer should suit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThemeHint {
    /// A light appearance.
    #[default]
    Light,
    /// A dark appearance.
    Dark,
}

/// The user's choices that shape a prepared message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrepareOptions {
    /// Whether remote content may load.
    pub remote: RemoteContentDecision,
    /// The appearance to suit.
    pub theme: ThemeHint,
}

/// Why a prepared message has no HTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlUnavailable {
    /// The message has no HTML part.
    NoHtml,
    /// The HTML is over the sanitiser's caps; renderers show the text.
    TooComplex(Complexity),
}

impl fmt::Display for HtmlUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHtml => formatter.write_str("the message has no HTML"),
            Self::TooComplex(complexity) => complexity.fmt(formatter),
        }
    }
}

/// The remote content a message references, and whether it may load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteContent {
    /// The user's decision.
    pub decision: RemoteContentDecision,
    /// The remote resources, indexed by the `n` of `zmail-blocked:<n>`.
    pub resources: Vec<RemoteResource>,
}

impl RemoteContent {
    /// Whether the message references remote content.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// The distinct origins the message references, in order.
    pub fn origins(&self) -> Vec<&str> {
        distinct(
            self.resources
                .iter()
                .map(|resource| resource.origin.as_str()),
        )
    }

    /// The distinct origins whose resources the HTML may load: empty
    /// unless the user allowed remote content, and only public `https:`
    /// origins then.
    pub fn allowed_origins(&self) -> Vec<&str> {
        distinct(
            self.resources
                .iter()
                .filter(|resource| resource.loads)
                .map(|resource| resource.origin.as_str()),
        )
    }

    /// How many resources stay blocked.
    pub fn blocked_count(&self) -> usize {
        self.resources
            .iter()
            .filter(|resource| !resource.loads)
            .count()
    }
}

fn distinct<'a>(items: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen = Vec::new();
    for item in items {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
}

/// A message ready for any renderer; see the crate documentation.
#[derive(Debug, Clone)]
pub struct PreparedMessage {
    /// The message's local id.
    pub message_id: i64,
    html: Result<String, HtmlUnavailable>,
    /// The readable text: the plain-text alternative when there is one,
    /// otherwise text derived from the HTML.
    pub text: String,
    /// Whether `text` is the plain-text alternative (`text_links` is then
    /// empty: links are the addresses written in it).
    pub text_is_plain: bool,
    /// The ranges of link text in `text`, when it was derived from HTML.
    pub text_links: Vec<TextLink>,
    /// Link targets, indexed by the `n` of `zmail-link:<n>`.
    pub links: Vec<LinkTarget>,
    /// The remote content and the decision about it.
    pub remote: RemoteContent,
    /// The message's inline parts.
    pub inline_parts: Vec<InlinePart>,
    /// The Content-IDs the HTML references.
    pub cid_references: Vec<String>,
    /// The appearance to suit.
    pub theme: ThemeHint,
}

impl PreparedMessage {
    /// The sanitised HTML, or why there is none.
    pub fn html(&self) -> Result<&str, HtmlUnavailable> {
        self.html.as_deref().map_err(|reason| *reason)
    }

    /// The target of link `id` (the `n` of `zmail-link:<n>`).
    pub fn link(&self, id: usize) -> Option<&LinkTarget> {
        self.links.get(id)
    }

    /// The target of a `zmail-link:<n>` address a renderer reports.
    pub fn link_for_url(&self, url: &str) -> Option<&LinkTarget> {
        let id = url
            .trim()
            .strip_prefix(LINK_SCHEME)?
            .strip_prefix(':')?
            .parse()
            .ok()?;
        self.link(id)
    }

    /// The inline part a `cid:` address names. Takes `cid:x`, `<x>` or
    /// `x`; Content-IDs compare exactly first, then ignoring ASCII case.
    pub fn inline_part(&self, content_id: &str) -> Option<&InlinePart> {
        let content_id = content_id.trim();
        let content_id = match content_id.get(..4) {
            Some(prefix) if prefix.eq_ignore_ascii_case("cid:") => &content_id[4..],
            _ => content_id,
        };
        let content_id = content_id.trim_start_matches('<').trim_end_matches('>');
        self.inline_parts
            .iter()
            .find(|part| part.content_id == content_id)
            .or_else(|| {
                self.inline_parts
                    .iter()
                    .find(|part| part.content_id.eq_ignore_ascii_case(content_id))
            })
    }
}

/// Prepares a message for rendering; see the crate documentation.
pub fn prepare(source: &RenderSource, options: &PrepareOptions) -> PreparedMessage {
    let plain = source
        .body_text
        .as_deref()
        .filter(|text| !text.trim().is_empty());
    let Some(html) = source
        .html
        .as_deref()
        .filter(|html| !html.trim().is_empty())
    else {
        return PreparedMessage {
            message_id: source.message_id,
            html: Err(HtmlUnavailable::NoHtml),
            text: plain.unwrap_or_default().to_owned(),
            text_is_plain: true,
            text_links: Vec::new(),
            links: Vec::new(),
            remote: RemoteContent {
                decision: options.remote,
                resources: Vec::new(),
            },
            inline_parts: source.inline_parts.clone(),
            cid_references: Vec::new(),
            theme: options.theme,
        };
    };
    let document = sanitize_html(
        html,
        &SanitizeOptions {
            allow_remote: options.remote.allows(),
            legacy_blocked_links: source.html_is_legacy,
        },
    );
    let html = match (document.html, document.too_complex) {
        (Some(html), _) => Ok(html),
        (None, Some(complexity)) => Err(HtmlUnavailable::TooComplex(complexity)),
        (None, None) => Err(HtmlUnavailable::NoHtml),
    };
    let (text, text_is_plain, text_links) = match plain {
        Some(plain) => (plain.to_owned(), true, Vec::new()),
        None => (document.text, false, document.text_links),
    };
    PreparedMessage {
        message_id: source.message_id,
        html,
        text,
        text_is_plain,
        text_links,
        links: document.links,
        remote: RemoteContent {
            decision: options.remote,
            resources: document.remote,
        },
        inline_parts: source.inline_parts.clone(),
        cid_references: document.cid_references,
        theme: options.theme,
    }
}

/// The bytes of an inline part the store keeps, decrypted on demand.
struct StoredPartBytes(zmail_store::AttachmentBlob);

impl PartBytes for StoredPartBytes {
    fn read(&self) -> anyhow::Result<Vec<u8>> {
        self.0.read()
    }
}

/// What the store holds for message `message_id`, ready for [`prepare`].
/// `None` for a missing message.
pub fn render_source(
    store: &zmail_store::LocalStore,
    message_id: i64,
) -> anyhow::Result<Option<RenderSource>> {
    let Some(parts) = store.message_render_parts(message_id)? else {
        return Ok(None);
    };
    Ok(Some(RenderSource {
        message_id,
        body_text: parts.body_text,
        html: parts.html,
        html_is_legacy: parts.html_is_legacy,
        remote_content_sender_allowed: parts.remote_content_sender_allowed,
        inline_parts: parts
            .inline_parts
            .into_iter()
            .map(|part| {
                InlinePart::new(
                    part.content_id,
                    part.content_type,
                    part.size_bytes,
                    Arc::new(StoredPartBytes(part.blob)),
                )
            })
            .collect(),
    }))
}

/// Which kind of renderer draws a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererKind {
    /// The OS web engine (WKWebView, WebView2, WebKitGTK).
    Web,
    /// The in-house subset renderer drawn with GPUI elements.
    Native,
    /// The text view: [`PreparedMessage::text`] as paragraphs.
    Text,
}

/// A renderer behind the boundary. Implementations live with their
/// toolkit (GPUI, the OS engines); this crate only defines what they are
/// given and how Zmail chooses between them.
pub trait MessageRenderer {
    /// Which kind of renderer this is.
    fn kind(&self) -> RendererKind;

    /// Whether this renderer can show `message`. A renderer that needs
    /// HTML refuses one without it, and Zmail falls back to the next.
    fn accepts(&self, message: &PreparedMessage) -> bool {
        message.html().is_ok()
    }
}

/// The text view: shows every message, as text.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextRenderer;

impl MessageRenderer for TextRenderer {
    fn kind(&self) -> RendererKind {
        RendererKind::Text
    }

    fn accepts(&self, _message: &PreparedMessage) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
