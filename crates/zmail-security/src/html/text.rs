//! Readable text from HTML, for the text view and as every renderer's
//! fallback.
//!
//! The derivation reads tokens (html5ever's tokenizer, so character
//! references decode as browsers decode them) and never builds a tree, so
//! nesting costs nothing. It normally reads the sanitiser's own output,
//! which is well formed; for mail too complex to sanitise, and for bodies
//! stored before ADR 0006, it reads markup that may not be, and records
//! links through the same link policy.
//!
//! Layout rules: paragraphs, headings, quotes, lists and tables are
//! separated by a blank line; `<div>`, rows, list items and a single
//! `<br>` start a new line; two `<br>`s make a blank line; runs of blank
//! lines collapse to one. Table cells on one line are separated by two
//! spaces. List items start with "• " or their number. Content hidden with
//! `display: none` (preheaders) and invisible spacer characters are left
//! out. Link text becomes a range of the text pointing at its link, never
//! across a line break.

use std::cell::RefCell;

use html5ever::{
    tendril::StrTendril,
    tokenizer::{
        BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    },
};

use super::{LINK_SCHEME, TextLink, elements, elements::OpenElements, policy::UrlPolicy};

/// Elements separated from their neighbours by a blank line.
const PARAGRAPH_ELEMENTS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "dl",
    "table",
    "pre",
    "hr",
    "figure",
];

/// Elements that start and end a line.
const LINE_ELEMENTS: &[&str] = &[
    "div",
    "li",
    "dt",
    "dd",
    "tr",
    "caption",
    "figcaption",
    "address",
    "center",
    "summary",
    "details",
    "header",
    "footer",
    "nav",
    "section",
    "article",
    "aside",
    "main",
    "form",
    "fieldset",
    "legend",
    "thead",
    "tbody",
    "tfoot",
];

/// The most open elements the derivation keeps; beyond it, nesting is
/// only counted (well past [`MAX_DEPTH`](super::MAX_DEPTH)).
const STACK_LIMIT: usize = 4 * super::MAX_DEPTH;

/// Characters mail uses as invisible spacers (preheader padding).
fn is_invisible(ch: char) -> bool {
    matches!(
        ch,
        '\u{00ad}' | '\u{034f}' | '\u{200b}'..='\u{200f}' | '\u{2060}'..='\u{2064}' | '\u{feff}'
    )
}

/// The derived text of a document.
#[derive(Debug, Default)]
pub(crate) struct Derived {
    pub(crate) text: String,
    pub(crate) links: Vec<TextLink>,
    /// The deepest element nesting seen.
    pub(crate) deepest: usize,
}

/// What the derivation reads.
pub(crate) enum Markup<'p> {
    /// The sanitiser's output, whose `href`s are `zmail-link:<n>` ids.
    Sanitized,
    /// The sanitiser's output with each `href` its target, recorded
    /// through the policy.
    SanitizedWithTargets(&'p mut UrlPolicy),
    /// Markup that may not be well formed, whose `href`s are recorded
    /// through the policy. Nothing is hidden, since an unclosed hidden
    /// element could hide the rest of the message.
    Unsanitized(&'p mut UrlPolicy),
}

/// Derives the text of `html`.
pub(crate) fn derive(html: &str, markup: Markup<'_>) -> Derived {
    let (policy, raw) = match markup {
        Markup::Sanitized => (None, false),
        Markup::SanitizedWithTargets(policy) => (Some(policy), false),
        Markup::Unsanitized(policy) => (Some(policy), true),
    };
    let sink = TextSink {
        state: RefCell::new(TextState {
            policy,
            raw,
            open: OpenElements::with_limit(STACK_LIMIT),
            ..TextState::default()
        }),
    };
    let sink = tokenize(html, sink);
    sink.state.into_inner().finish()
}

/// Runs html5ever's tokenizer over `input` into `sink`.
pub(crate) fn tokenize<S: TokenSink<Handle = ()>>(input: &str, sink: S) -> S {
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(input));
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    tokenizer.sink
}

struct TextSink<'p> {
    state: RefCell<TextState<'p>>,
}

impl TokenSink for TextSink<'_> {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let mut state = self.state.borrow_mut();
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => return state.start_tag(&tag),
            Token::TagToken(tag) => state.end_tag(&tag.name),
            Token::CharacterTokens(text) => state.push_text(&text),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

#[derive(Default)]
struct TextState<'p> {
    policy: Option<&'p mut UrlPolicy>,
    /// Reading markup that may not be well formed.
    raw: bool,
    text: String,
    links: Vec<TextLink>,
    open: OpenElements,
    /// Line breaks owed before the next text (at most 2: a blank line).
    pending_breaks: u8,
    pending_space: bool,
    pending_cell: bool,
    pending_prefix: Option<String>,
    /// The open link: its index, and where its current run of text starts.
    link: Option<(usize, Option<usize>)>,
    /// A raw text element (`<style>`, `<script>`, `<title>`…) whose
    /// contents are not message text.
    raw_text: Option<String>,
    /// The depth of the outermost hidden element, while inside one.
    hidden_at: Option<usize>,
    /// The depth of the outermost `<pre>`, while inside one.
    pre_at: Option<usize>,
    /// Open lists: their depth and, for `<ol>`, the next number.
    lists: Vec<(usize, Option<u32>)>,
}

impl TextState<'_> {
    fn start_tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        if self.raw_text.is_some() {
            return TokenSinkResult::Continue;
        }
        if name != "plaintext"
            && let Some(state) = elements::raw_text_state(name)
        {
            self.raw_text = Some(name.to_owned());
            return state;
        }
        let hidden = !self.raw && !elements::is_void(name) && is_hidden(tag);
        self.open.open(name, tag.self_closing);
        self.close_finished_scopes();
        let depth = self.open.depth();
        if hidden && self.hidden_at.is_none() {
            self.hidden_at = Some(depth);
        }
        if self.hidden_at.is_some() {
            return TokenSinkResult::Continue;
        }
        match name {
            "a" => {
                self.finish_link();
                self.link = attribute(tag, "href")
                    .and_then(|href| self.link_index(href))
                    .map(|index| (index, None));
            }
            "br" => self.line_break(),
            "img" => self.pending_space = true,
            "td" | "th" => {
                if self.pending_breaks == 0 && self.line_has_text() {
                    self.pending_cell = true;
                }
            }
            "li" => {
                self.block(1);
                self.lists.retain(|(list_depth, _)| *list_depth < depth);
                self.pending_prefix = Some(match self.lists.last_mut() {
                    Some((_, Some(number))) => {
                        let prefix = format!("{number}. ");
                        *number += 1;
                        prefix
                    }
                    _ => "• ".to_owned(),
                });
            }
            "ul" | "ol" => {
                self.lists.retain(|(list_depth, _)| *list_depth < depth);
                // A list inside a list item is part of that item.
                self.block(if self.lists.is_empty() { 2 } else { 1 });
                let start = attribute(tag, "start")
                    .and_then(|start| start.trim().parse().ok())
                    .unwrap_or(1);
                self.lists.push((depth, (name == "ol").then_some(start)));
            }
            "pre" => {
                self.block(2);
                self.pre_at.get_or_insert(depth);
            }
            _ if PARAGRAPH_ELEMENTS.contains(&name) => self.block(2),
            _ if LINE_ELEMENTS.contains(&name) => self.block(1),
            _ => {}
        }
        TokenSinkResult::Continue
    }

    fn end_tag(&mut self, name: &str) {
        if let Some(raw_text) = &self.raw_text {
            if raw_text == name {
                self.raw_text = None;
            }
            return;
        }
        if !self.open.close(name) {
            return;
        }
        let was_hidden = self.hidden_at.is_some();
        self.close_finished_scopes();
        if was_hidden {
            return;
        }
        match name {
            "a" => self.finish_link(),
            "ul" | "ol" => {
                let nested = self
                    .lists
                    .iter()
                    .any(|(depth, _)| *depth <= self.open.depth());
                self.block(if nested { 1 } else { 2 });
            }
            _ if PARAGRAPH_ELEMENTS.contains(&name) => self.block(2),
            _ if LINE_ELEMENTS.contains(&name) => self.block(1),
            _ => {}
        }
    }

    /// Leaves the hidden, preformatted and list scopes whose element just
    /// closed.
    fn close_finished_scopes(&mut self) {
        let depth = self.open.depth();
        if self.hidden_at.is_some_and(|at| depth < at) {
            self.hidden_at = None;
        }
        if self.pre_at.is_some_and(|at| depth < at) {
            self.pre_at = None;
        }
        self.lists.retain(|(list_depth, _)| *list_depth <= depth);
    }

    fn link_index(&mut self, href: &str) -> Option<usize> {
        match self.policy.as_deref_mut() {
            Some(policy) => policy.link_index(href),
            None => href
                .strip_prefix(LINK_SCHEME)
                .and_then(|rest| rest.strip_prefix(':'))
                .and_then(|index| index.parse().ok()),
        }
    }

    fn push_text(&mut self, chunk: &str) {
        if self.raw_text.is_some() || self.hidden_at.is_some() {
            return;
        }
        let pre = self.pre_at.is_some();
        for ch in chunk.chars() {
            if is_invisible(ch) || ch == '\r' {
                continue;
            }
            if pre && ch == '\n' {
                self.line_break();
            } else if (!pre && ch.is_whitespace()) || (ch.is_control() && ch != '\t') {
                // Control characters read as spaces, so they cannot
                // disturb the layout.
                self.pending_space = true;
            } else {
                self.emit(ch);
            }
        }
    }

    fn emit(&mut self, ch: char) {
        if !self.text.is_empty() {
            if self.pending_breaks > 0 {
                // A link's text never runs across a line break.
                self.end_link_run();
                for _ in 0..self.pending_breaks {
                    self.text.push('\n');
                }
            } else if self.pending_cell {
                self.text.push_str("  ");
            } else if self.pending_space && self.line_has_text() {
                self.text.push(' ');
            }
        }
        self.pending_breaks = 0;
        self.pending_space = false;
        self.pending_cell = false;
        if let Some(prefix) = self.pending_prefix.take() {
            self.text.push_str(&prefix);
        }
        if let Some((_, start @ None)) = &mut self.link {
            *start = Some(self.text.len());
        }
        self.text.push(ch);
    }

    fn line_has_text(&self) -> bool {
        !self.text.is_empty() && !self.text.ends_with('\n')
    }

    /// A block boundary owing `breaks` line breaks (1: new line, 2: blank
    /// line).
    fn block(&mut self, breaks: u8) {
        self.pending_breaks = self.pending_breaks.max(breaks);
    }

    fn line_break(&mut self) {
        if !self.text.is_empty() {
            self.pending_breaks = (self.pending_breaks + 1).min(2);
        }
    }

    fn end_link_run(&mut self) {
        if let Some((index, start)) = &mut self.link
            && let Some(start) = start.take()
            && start < self.text.len()
        {
            self.links.push(TextLink {
                range: start..self.text.len(),
                link: *index,
            });
        }
    }

    fn finish_link(&mut self) {
        self.end_link_run();
        self.link = None;
    }

    fn finish(mut self) -> Derived {
        self.finish_link();
        let end = self.text.trim_end().len();
        self.text.truncate(end);
        Derived {
            text: self.text,
            links: self.links,
            deepest: self.open.deepest(),
        }
    }
}

fn attribute<'t>(tag: &'t Tag, name: &str) -> Option<&'t str> {
    tag.attrs
        .iter()
        .find(|attribute| &*attribute.name.local == name)
        .map(|attribute| &*attribute.value)
}

/// Whether an element's inline style hides it (mail preheaders).
fn is_hidden(tag: &Tag) -> bool {
    attribute(tag, "style").is_some_and(|style| {
        let style: String = style
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        style.contains("display:none") || style.contains("mso-hide:all")
    })
}
