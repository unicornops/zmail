//! A tokenizer pass over the original markup before it is parsed into a
//! tree: it counts elements and approximate nesting against the caps,
//! collects `<style>` blocks for the CSS pass, and keeps the `<body>` tag's
//! presentational attributes, which fragment parsing drops.
//!
//! It runs in linear time and memory whatever the input, so markup that
//! would make the tree builder slow (deep nesting makes some of its steps
//! walk the whole stack of open elements) is refused before it is built.

use std::cell::RefCell;

use html5ever::tokenizer::{Tag, TagKind, Token, TokenSink, TokenSinkResult};

use super::{
    Complexity, MAX_ELEMENTS, MAX_PREPASS_DEPTH, MAX_STYLE_BYTES, elements, elements::OpenElements,
    text::tokenize,
};

/// What the pre-pass found.
#[derive(Debug, Default)]
pub(crate) struct PrePass {
    pub(crate) elements: usize,
    pub(crate) deepest: usize,
    /// The contents of each `<style>` block, in order.
    pub(crate) styles: Vec<String>,
    style_bytes: usize,
    style_too_large: bool,
    /// The first `<body>` tag's attributes, as (name, value).
    pub(crate) body_attributes: Vec<(String, String)>,
}

impl PrePass {
    pub(crate) fn run(html: &str) -> Self {
        let sink = PrePassSink {
            state: RefCell::new(State {
                open: OpenElements::with_limit(MAX_PREPASS_DEPTH),
                ..State::default()
            }),
        };
        let state = tokenize(html, sink).state.into_inner();
        state.found
    }

    /// The first cap the markup is over, if any.
    pub(crate) fn complexity(&self) -> Option<Complexity> {
        if self.elements > MAX_ELEMENTS {
            Some(Complexity::TooManyElements)
        } else if self.deepest > MAX_PREPASS_DEPTH {
            Some(Complexity::TooDeep)
        } else if self.style_too_large {
            Some(Complexity::StyleTooLarge)
        } else {
            None
        }
    }
}

struct PrePassSink {
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    found: PrePass,
    open: OpenElements,
    seen_body: bool,
    raw_text: Option<String>,
}

impl TokenSink for PrePassSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let mut state = self.state.borrow_mut();
        if state.found.complexity().is_some() {
            // Over a cap: nothing more to learn.
            return TokenSinkResult::Continue;
        }
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => return state.start_tag(&tag),
            Token::TagToken(tag) => {
                if state.raw_text.as_deref() == Some(&*tag.name) {
                    state.raw_text = None;
                }
                state.open.close(&tag.name);
                state.found.deepest = state.open.deepest();
            }
            Token::CharacterTokens(text) if state.raw_text.as_deref() == Some("style") => {
                let found = &mut state.found;
                found.style_bytes += text.len();
                if found.style_bytes > MAX_STYLE_BYTES {
                    found.style_too_large = true;
                } else if let Some(style) = found.styles.last_mut() {
                    style.push_str(&text);
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

impl State {
    fn start_tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        self.found.elements += 1;
        if name == "body" && !self.seen_body {
            self.seen_body = true;
            self.found.body_attributes = tag
                .attrs
                .iter()
                .map(|attribute| {
                    (
                        attribute.name.local.to_string(),
                        attribute.value.to_string(),
                    )
                })
                .collect();
        }
        if name == "style" {
            // Each block is its own stylesheet: one that ends inside a
            // rule must not swallow the next.
            self.found.styles.push(String::new());
        }
        self.open.open(name, tag.self_closing);
        self.found.deepest = self.open.deepest();
        match elements::raw_text_state(name) {
            Some(state) if !tag.self_closing => {
                self.raw_text = Some(name.to_owned());
                state
            }
            _ => TokenSinkResult::Continue,
        }
    }
}
