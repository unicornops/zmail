//! Highlighting a search's matches in its result rows.
//!
//! Matching is local and mirrors the store's FTS5 `unicode61` tokens: runs
//! of letters and digits, compared without case. A word matches tokens it
//! is a prefix of, as in the index; a quoted phrase matches its words in a
//! row. Free text highlights in the subject and the snippet, `subject:` in
//! the subject only. Negated clauses never highlight.
//!
//! The result is byte ranges on character boundaries of the row's own
//! text, which the row draws with `StyledText` highlights. No markup is
//! built from the (untrusted) message text, so nothing in it can change
//! how it is drawn beyond the ranges marked.

use std::ops::Range;

use zmail_core::search::{SearchQuery, SearchTerm, SearchText};

/// Characters of context a snippet keeps before its first match when it
/// has to be shortened to bring the match into view.
const SNIPPET_LEAD: usize = 24;

/// A snippet whose first match starts further in than this many
/// characters is shortened to start near the match.
const SNIPPET_MATCH_AT_MOST: usize = 48;

/// Characters a body excerpt shows: more than a row has room for, so
/// the row's own truncation decides where it ends.
const EXCERPT_CHARS: usize = 240;

/// What one result row highlights.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RowMatches {
    pub(crate) subject: Vec<Range<usize>>,
    pub(crate) snippet: Vec<Range<usize>>,
}

/// The patterns a query highlights.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SearchHighlighter {
    /// Free text: subject and snippet.
    anywhere: Vec<Pattern>,
    /// `subject:` clauses.
    subject: Vec<Pattern>,
}

/// One word or phrase as lowercased tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    tokens: Vec<String>,
    /// The last token also matches longer tokens (an unquoted word).
    prefix: bool,
}

impl Pattern {
    fn new(text: &SearchText) -> Option<Self> {
        let tokens = tokens(&text.value)
            .into_iter()
            .map(|(_, token)| token)
            .collect::<Vec<_>>();
        (!tokens.is_empty()).then_some(Self {
            tokens,
            prefix: !text.phrase,
        })
    }
}

impl SearchHighlighter {
    pub(crate) fn new(query: &str) -> Self {
        let mut highlighter = Self::default();
        for clause in SearchQuery::parse(query).clauses {
            if clause.negated {
                continue;
            }
            match &clause.term {
                SearchTerm::Text(text) => highlighter.anywhere.extend(Pattern::new(text)),
                SearchTerm::Subject(text) => highlighter.subject.extend(Pattern::new(text)),
                _ => {}
            }
        }
        highlighter
    }

    /// Whether the query marks anything outside the subject, so a body
    /// excerpt could show it.
    pub(crate) fn marks_text(&self) -> bool {
        !self.anywhere.is_empty()
    }

    /// A one-line excerpt of `body` around its first match, for a result
    /// whose stored snippet does not show why it matched. Whitespace runs
    /// (line breaks included) become single spaces; ellipses mark cut ends.
    pub(crate) fn body_excerpt(&self, body: &str) -> Option<String> {
        let first = self.text(body).into_iter().next()?;
        let start = lead_in_start(body, first.start);
        let end = body[start..]
            .char_indices()
            .nth(EXCERPT_CHARS)
            .map_or(body.len(), |(index, _)| start + index);
        let mut excerpt = String::new();
        if start > 0 {
            excerpt.push('\u{2026}');
        }
        excerpt.push_str(
            &body[start..end]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        );
        if end < body.len() {
            excerpt.push('\u{2026}');
        }
        Some(excerpt)
    }

    /// The matches in a subject.
    pub(crate) fn subject(&self, text: &str) -> Vec<Range<usize>> {
        matches(text, self.anywhere.iter().chain(&self.subject))
    }

    /// The matches in other text (the snippet).
    pub(crate) fn text(&self, text: &str) -> Vec<Range<usize>> {
        matches(text, self.anywhere.iter())
    }

    /// The snippet to show for `text` and its matches: shortened to start
    /// near the first match (after an ellipsis) when that match would
    /// otherwise sit past the end of a one-line row.
    pub(crate) fn snippet(&self, text: &str) -> (String, Vec<Range<usize>>) {
        let found = self.text(text);
        let Some(first) = found.first() else {
            return (text.to_owned(), found);
        };
        if text[..first.start].chars().count() <= SNIPPET_MATCH_AT_MOST {
            return (text.to_owned(), found);
        }
        let cut = lead_in_start(text, first.start);
        let shown = format!("\u{2026}{}", &text[cut..]);
        let shift = '\u{2026}'.len_utf8();
        let ranges = found
            .into_iter()
            .map(|range| range.start - cut + shift..range.end - cut + shift)
            .collect();
        (shown, ranges)
    }
}

/// Where text shown from a match at `at` starts: SNIPPET_LEAD characters
/// before it, moved on to a word start when that stretch has one, or the
/// start of the text when the match is nearer than that.
fn lead_in_start(text: &str, at: usize) -> usize {
    // A match this near the start keeps the text from its start.
    let Some((lead_start, _)) = text[..at].char_indices().rev().nth(SNIPPET_LEAD - 1) else {
        return 0;
    };
    text[lead_start..at]
        .find(char::is_whitespace)
        .map(|offset| {
            let after = lead_start + offset;
            after + (text[after..].len() - text[after..].trim_start().len())
        })
        .filter(|cut| *cut <= at)
        .unwrap_or(lead_start)
}

/// The ranges of `text` any pattern matches, sorted and merged.
fn matches<'a>(text: &str, patterns: impl Iterator<Item = &'a Pattern>) -> Vec<Range<usize>> {
    let patterns = patterns.collect::<Vec<_>>();
    if patterns.is_empty() {
        return Vec::new();
    }
    let tokens = tokens(text);
    let mut found = Vec::new();
    for pattern in patterns {
        let count = pattern.tokens.len();
        for start in 0..tokens.len().saturating_sub(count - 1) {
            let window = &tokens[start..start + count];
            let all =
                window
                    .iter()
                    .zip(&pattern.tokens)
                    .enumerate()
                    .all(|(index, ((_, have), want))| {
                        if pattern.prefix && index == count - 1 {
                            have.starts_with(want.as_str())
                        } else {
                            have == want
                        }
                    });
            if all {
                found.push(window[0].0.start..window[count - 1].0.end);
            }
        }
    }
    found.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(found.len());
    for range in found {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// The tokens of `text`: runs of letters and digits, lowercased, with
/// their byte ranges.
fn tokens(text: &str) -> Vec<(Range<usize>, String)> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        match (ch.is_alphanumeric(), start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                tokens.push((from..index, text[from..index].to_lowercase()));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        tokens.push((from..text.len(), text[from..].to_lowercase()));
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(text: &str, ranges: &[Range<usize>]) -> String {
        let mut out = String::new();
        let mut at = 0;
        for range in ranges {
            out.push_str(&text[at..range.start]);
            out.push('[');
            out.push_str(&text[range.clone()]);
            out.push(']');
            at = range.end;
        }
        out.push_str(&text[at..]);
        out
    }

    #[test]
    fn words_match_as_prefixes_and_phrases_exactly() {
        let highlighter = SearchHighlighter::new("budg \"review notes\" -spam from:alice");
        let subject = "Budget: REVIEW notes, budgeting and review";
        assert_eq!(
            marked(subject, &highlighter.subject(subject)),
            "[Budget]: [REVIEW notes], [budgeting] and review"
        );
        // Negated and non-text clauses highlight nothing.
        assert!(highlighter.text("spam from alice").is_empty());
        assert!(
            SearchHighlighter::new("is:unread -x from:a")
                .subject("x from a")
                .is_empty()
        );
    }

    #[test]
    fn subject_clauses_only_mark_the_subject() {
        let highlighter = SearchHighlighter::new("subject:launch plan");
        assert_eq!(
            marked("Launch plan", &highlighter.subject("Launch plan")),
            "[Launch] [plan]"
        );
        assert_eq!(
            marked("launch plan", &highlighter.text("launch plan")),
            "launch [plan]"
        );
    }

    #[test]
    fn matches_merge_and_stay_on_character_boundaries() {
        let highlighter = SearchHighlighter::new("zoë zo ünal 日本");
        let text = "Zoë Ünal wrote 日本 notes; zoo";
        let ranges = highlighter.text(text);
        assert_eq!(
            marked(text, &ranges),
            "[Zoë] [Ünal] wrote [日本] notes; [zoo]"
        );
        for range in ranges {
            assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
        }
        // Markup in message text is just text.
        let hostile = "<b>bold</b> **bold**";
        assert_eq!(
            marked(hostile, &SearchHighlighter::new("bold").text(hostile)),
            "<b>[bold]</b> **[bold]**"
        );
    }

    #[test]
    fn body_excerpts_show_the_first_match_on_one_line() {
        let highlighter = SearchHighlighter::new("release");
        assert_eq!(highlighter.body_excerpt("Nothing to see"), None);
        assert_eq!(
            highlighter
                .body_excerpt("The release\nis   ready")
                .as_deref(),
            Some("The release is ready")
        );
        let body = format!(
            "{}Some notes before the release checklist.\n\n{}",
            "Lorem ipsum dolor sit amet. ".repeat(4),
            "More text after it. ".repeat(20)
        );
        let excerpt = highlighter.body_excerpt(&body).expect("an excerpt");
        assert!(excerpt.starts_with("\u{2026}"), "{excerpt}");
        assert!(excerpt.ends_with("\u{2026}"), "{excerpt}");
        assert!(!excerpt.contains('\n'));
        // The row then shows it with the match in view.
        let (shown, ranges) = highlighter.snippet(&excerpt);
        assert_eq!(shown, excerpt);
        assert_eq!(&shown[ranges[0].clone()], "release");
        // No free text: nothing to excerpt.
        assert!(!SearchHighlighter::new("subject:release is:unread").marks_text());
    }

    #[test]
    fn a_late_match_brings_the_snippet_to_it() {
        let highlighter = SearchHighlighter::new("kittiwake");
        let early = "A kittiwake flew past.";
        assert_eq!(highlighter.snippet(early).0, early);

        let late = "The meeting ran long and covered budgets, hiring and the roadmap \
                    before anyone mentioned the kittiwake nesting on the roof.";
        let (shown, ranges) = highlighter.snippet(late);
        assert_eq!(
            marked(&shown, &ranges),
            "\u{2026}anyone mentioned the [kittiwake] nesting on the roof."
        );
        assert!(shown.chars().count() < late.chars().count());
    }
}
