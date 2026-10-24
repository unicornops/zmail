//! Element facts shared by the tokenizer passes: void elements, the raw
//! text elements whose contents the tokenizer must not read as markup,
//! and an approximate stack of open elements.

use html5ever::tokenizer::{TokenSinkResult, states::RawKind};

/// Elements that never have contents or an end tag.
pub(crate) const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "keygen", "link", "meta", "param",
    "source", "track", "wbr",
];

pub(crate) fn is_void(name: &str) -> bool {
    VOID_ELEMENTS.contains(&name)
}

/// How the tokenizer reads what follows a start tag, as the HTML tree
/// builder tells it to in a document body (scripting on, as in the parser
/// the sanitiser uses). `None` for ordinary markup.
pub(crate) fn raw_text_state(name: &str) -> Option<TokenSinkResult<()>> {
    match name {
        "title" | "textarea" => Some(TokenSinkResult::RawData(RawKind::Rcdata)),
        "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
            Some(TokenSinkResult::RawData(RawKind::Rawtext))
        }
        "script" => Some(TokenSinkResult::RawData(RawKind::ScriptData)),
        "plaintext" => Some(TokenSinkResult::Plaintext),
        _ => None,
    }
}

/// Elements whose start tag closes an open `<p>`.
const CLOSES_PARAGRAPH: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "div",
    "dl",
    "fieldset",
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
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
];

/// Formatting elements, which the tree builder reopens after a
/// misnested end tag closes them early (the adoption agency).
const FORMATTING_ELEMENTS: &[&str] = &[
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];

/// Elements that fence off formatting: what is inside them is not
/// reopened outside.
const FORMATTING_MARKERS: &[&str] = &[
    "applet", "caption", "marquee", "object", "table", "td", "template", "th", "tr",
];

/// A stack of open elements, kept from tags alone. For well-formed markup
/// (the sanitiser's own output) it is exact; for anything else it applies
/// the commonest implied end tags (`<p>`, `<li>`, table cells and rows), so
/// sloppy but ordinary mail does not look deeply nested, and reopens the
/// formatting elements a misnested end tag closed, so misnesting that
/// really does nest deeper (`<a><b>x</a>` repeated) is counted.
///
/// The stack holds at most `limit` elements: deeper elements are counted
/// in [`Self::deepest`] but not kept, so no input makes a tag cost more
/// than `limit` steps.
#[derive(Debug)]
pub(crate) struct OpenElements {
    stack: Vec<String>,
    deepest: usize,
    limit: usize,
}

/// How far an implied end tag looks for the element it closes.
const IMPLIED_END_REACH: usize = 64;

impl Default for OpenElements {
    fn default() -> Self {
        Self::with_limit(usize::MAX)
    }
}

impl OpenElements {
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            stack: Vec::new(),
            deepest: 0,
            limit,
        }
    }

    pub(crate) fn depth(&self) -> usize {
        self.stack.len()
    }

    /// The deepest the stack has been.
    pub(crate) fn deepest(&self) -> usize {
        self.deepest
    }

    /// A start tag: closes what it implies, then opens `name` unless it is
    /// void or self-closing.
    pub(crate) fn open(&mut self, name: &str, self_closing: bool) {
        if CLOSES_PARAGRAPH.contains(&name) {
            self.close_within("p", &["table", "td", "th", "button", "caption"]);
        }
        match name {
            "li" => {
                self.close_within("li", &["ul", "ol", "table", "td", "th"]);
            }
            "dd" | "dt" => {
                self.close_any_within(&["dd", "dt"], &["dl", "table", "td", "th"]);
            }
            "tr" => {
                self.close_within("tr", &["table"]);
            }
            "td" | "th" => {
                self.close_any_within(&["td", "th"], &["tr", "table"]);
            }
            "tbody" | "thead" | "tfoot" => {
                self.close_any_within(&["tbody", "thead", "tfoot"], &["table"]);
            }
            "option" => {
                self.close_within("option", &["select"]);
            }
            _ => {}
        }
        if !self_closing && !is_void(name) {
            let depth = if self.stack.len() < self.limit {
                self.stack.push(name.to_owned());
                self.stack.len()
            } else {
                self.limit.saturating_add(1)
            };
            self.deepest = self.deepest.max(depth);
        }
    }

    /// An end tag: closes the nearest open `name` and everything inside
    /// it, then reopens the formatting elements it closed early. Returns
    /// whether one was open.
    pub(crate) fn close(&mut self, name: &str) -> bool {
        let Some(index) = self.stack.iter().rposition(|open| open == name) else {
            return false;
        };
        let closed = self.stack.split_off(index);
        if !closed
            .iter()
            .any(|element| FORMATTING_MARKERS.contains(&element.as_str()))
        {
            let room = self.limit.saturating_sub(self.stack.len());
            self.stack.extend(
                closed
                    .into_iter()
                    .skip(1)
                    .filter(|element| FORMATTING_ELEMENTS.contains(&element.as_str()))
                    .take(room),
            );
            self.deepest = self.deepest.max(self.stack.len());
        }
        true
    }

    fn close_within(&mut self, name: &str, boundaries: &[&str]) {
        self.close_any_within(&[name], boundaries);
    }

    /// Closes the nearest open element named in `names`, unless one of
    /// `boundaries` is open inside it.
    fn close_any_within(&mut self, names: &[&str], boundaries: &[&str]) {
        let reach = self.stack.len().saturating_sub(IMPLIED_END_REACH);
        for (index, open) in self.stack.iter().enumerate().skip(reach).rev() {
            if names.contains(&open.as_str()) {
                self.stack.truncate(index);
                return;
            }
            if boundaries.contains(&open.as_str()) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OpenElements;

    #[test]
    fn implied_end_tags_keep_sloppy_markup_shallow() {
        let mut open = OpenElements::default();
        for _ in 0..500 {
            open.open("p", false);
        }
        assert_eq!(open.depth(), 1);
        open.open("table", false);
        for _ in 0..100 {
            open.open("tr", false);
            open.open("td", false);
            open.open("td", false);
        }
        assert_eq!(open.depth(), 3, "table > tr > td: the table closed the p");
        open.open("ul", false);
        open.open("li", false);
        open.open("li", false);
        open.open("br", false);
        open.open("img", true);
        assert_eq!(open.depth(), 5);
        assert!(open.close("table"));
        assert_eq!(open.depth(), 0);
        assert!(!open.close("table"));
        assert_eq!(open.deepest(), 5);
    }

    #[test]
    fn misnested_formatting_reopens() {
        let mut open = OpenElements::default();
        for _ in 0..100 {
            for name in ["a", "b", "i"] {
                open.open(name, false);
            }
            open.close("a");
        }
        assert_eq!(open.depth(), 200, "b and i reopen each time");

        let mut open = OpenElements::default();
        open.open("table", false);
        for _ in 0..100 {
            open.open("tr", false);
            open.open("td", false);
            open.open("font", false);
            open.close("td");
        }
        assert_eq!(open.depth(), 2, "a cell fences off its formatting");
    }

    #[test]
    fn the_stack_stops_at_its_limit() {
        let mut open = OpenElements::with_limit(10);
        for _ in 0..100 {
            open.open("div", false);
        }
        assert_eq!(open.depth(), 10);
        assert!(open.deepest() > 10);
    }

    #[test]
    fn real_nesting_is_counted() {
        let mut open = OpenElements::default();
        for _ in 0..1_000 {
            open.open("table", false);
            open.open("tr", false);
            open.open("td", false);
        }
        assert_eq!(open.deepest(), 3_000);
    }
}
