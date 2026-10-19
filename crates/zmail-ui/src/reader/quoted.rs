//! Finds the quoted text and signature at the end of a plain-text body, so
//! the reader can fold them behind a "…" toggle.
//!
//! Detection is deliberately conservative: hiding a sentence the sender
//! wrote is worse than showing a quote. Only these fold, and only at the end
//! of the message:
//!
//! - a trailing block of `>` lines, with the "On …, … wrote:" line above it;
//! - everything from an Outlook "-----Original Message-----" separator, or a
//!   line of underscores followed by a `From:` line, to the end;
//! - a signature after the RFC 3676 `-- ` delimiter (the adapters trim line
//!   ends, so a bare `--` counts), when it is short.
//!
//! Interleaved quotes (replies written between quoted lines) and forwarded
//! messages stay visible: they are the content. A body that would be all
//! fold folds nothing.

/// The most lines a signature may have and still fold; anything longer is
/// more likely text after a `--` line than a signature.
const MAX_SIGNATURE_LINES: usize = 12;

/// The byte offset where the foldable tail of `body` starts, or `None` when
/// nothing folds. The tail runs to the end of the body.
pub(super) fn folded_tail_start(body: &str) -> Option<usize> {
    let lines = split_lines(body);
    let mut end = lines.len();
    if let Some(separator) = original_message_separator(&lines) {
        end = separator;
    }
    if let Some(quote) = trailing_quote_start(&lines[..end]) {
        end = quote;
    }
    if let Some(signature) = signature_start(&lines[..end]) {
        end = signature;
    }
    if end == lines.len() {
        return None;
    }
    // Something must stay visible above the fold.
    if lines[..end].iter().all(|line| line.text.trim().is_empty()) {
        return None;
    }
    Some(lines[end].offset)
}

/// Splits `body` into the visible text and the folded tail. The visible
/// text loses the blank lines before the fold.
pub(super) fn split_folded_tail(body: &str) -> (&str, Option<&str>) {
    match folded_tail_start(body) {
        Some(start) => (body[..start].trim_end(), Some(body[start..].trim_end())),
        None => (body, None),
    }
}

/// One line of the body and where it starts.
#[derive(Debug, Clone, Copy)]
struct Line<'a> {
    offset: usize,
    text: &'a str,
}

fn split_lines(body: &str) -> Vec<Line<'_>> {
    let mut offset = 0;
    body.split('\n')
        .map(|text| {
            let line = Line {
                offset,
                text: text.trim_end_matches('\r'),
            };
            offset += text.len() + 1;
            line
        })
        .collect()
}

fn is_blank(line: &Line<'_>) -> bool {
    line.text.trim().is_empty()
}

fn is_quoted(line: &Line<'_>) -> bool {
    line.text.trim_start().starts_with('>')
}

/// The first line of an Outlook-style quoted original message.
fn original_message_separator(lines: &[Line<'_>]) -> Option<usize> {
    lines.iter().enumerate().position(|(index, line)| {
        let text = line.text.trim();
        let dashes = text.trim_matches('-').trim();
        let is_original = text.starts_with("--")
            && text.ends_with("--")
            && dashes.eq_ignore_ascii_case("original message");
        let is_rule_then_from = text.len() >= 8
            && text.chars().all(|ch| ch == '_')
            && lines
                .get(index + 1)
                .is_some_and(|next| next.text.trim_start().starts_with("From:"));
        is_original || is_rule_then_from
    })
}

/// The start of the block of `>` lines that ends the text (blank lines
/// after it allowed), including its attribution line.
fn trailing_quote_start(lines: &[Line<'_>]) -> Option<usize> {
    let last = lines.iter().rposition(|line| !is_blank(line))?;
    if !is_quoted(&lines[last]) {
        return None;
    }
    let mut start = last;
    while start > 0 && (is_quoted(&lines[start - 1]) || is_blank(&lines[start - 1])) {
        start -= 1;
    }
    // Leading blank lines belong above the fold.
    while is_blank(&lines[start]) {
        start += 1;
    }
    Some(attribution_start(lines, start).unwrap_or(start))
}

/// The "On …, … wrote:" line above the quote starting at `quote`, which a
/// mail client may have wrapped onto two lines.
fn attribution_start(lines: &[Line<'_>], quote: usize) -> Option<usize> {
    let mut index = quote.checked_sub(1)?;
    if is_blank(&lines[index]) {
        index = index.checked_sub(1)?;
    }
    let line = lines[index].text.trim();
    if !line.to_ascii_lowercase().ends_with("wrote:") {
        return None;
    }
    if !line.starts_with("On ")
        && let Some(previous) = index.checked_sub(1)
        && lines[previous].text.trim_start().starts_with("On ")
    {
        return Some(previous);
    }
    Some(index)
}

/// The last signature delimiter, when a short signature follows it.
fn signature_start(lines: &[Line<'_>]) -> Option<usize> {
    let delimiter = lines
        .iter()
        .rposition(|line| line.text.trim_end() == "--")?;
    let signature_lines = lines[delimiter + 1..]
        .iter()
        .filter(|line| !is_blank(line))
        .count();
    (signature_lines > 0 && signature_lines <= MAX_SIGNATURE_LINES).then_some(delimiter)
}

#[cfg(test)]
mod tests {
    use super::split_folded_tail;

    #[test]
    fn plain_text_folds_nothing() {
        let body = "Hi Sam,\n\nThe numbers are attached.\n\nPriya";
        assert_eq!(split_folded_tail(body), (body, None));
    }

    #[test]
    fn a_trailing_quote_folds_with_its_attribution() {
        let body = "Sounds good.\n\nOn Mon, 3 Mar 2026 at 10:00, Priya <p@example.com> wrote:\n> Shall we meet?\n>\n> Priya\n";
        assert_eq!(
            split_folded_tail(body),
            (
                "Sounds good.",
                Some(
                    "On Mon, 3 Mar 2026 at 10:00, Priya <p@example.com> wrote:\n> Shall we meet?\n>\n> Priya"
                )
            )
        );
    }

    #[test]
    fn a_wrapped_attribution_folds_both_lines() {
        let body = "Yes.\n\nOn Mon, 3 Mar 2026 at 10:00, Priya Raman\n<priya@example.com> wrote:\n\n> Shall we meet?";
        let (visible, folded) = split_folded_tail(body);
        assert_eq!(visible, "Yes.");
        assert!(folded.is_some_and(|folded| folded.starts_with("On Mon")));
    }

    #[test]
    fn interleaved_quotes_stay_visible() {
        // Replies between quoted lines are the message.
        let body = "> Can you do Thursday?\nYes, after lunch.\n> And the slides?\nAttached.";
        assert_eq!(split_folded_tail(body), (body, None));
    }

    #[test]
    fn a_quote_with_nothing_above_it_stays_visible() {
        let body = "> Only a quote.\n> Nothing else.";
        assert_eq!(split_folded_tail(body), (body, None));
    }

    #[test]
    fn an_outlook_original_message_folds_to_the_end() {
        let body =
            "Thanks!\n\n-----Original Message-----\nFrom: Priya\nSent: Monday\n\nShall we meet?";
        let (visible, folded) = split_folded_tail(body);
        assert_eq!(visible, "Thanks!");
        assert!(folded.is_some_and(|folded| folded.ends_with("Shall we meet?")));

        let body = "Thanks!\n________________________________\nFrom: Priya\nShall we meet?";
        assert_eq!(split_folded_tail(body).0, "Thanks!");
    }

    #[test]
    fn a_short_signature_folds_and_a_long_tail_does_not() {
        let body = "See you then.\n\n-- \nSam Taylor\nExample Ltd";
        assert_eq!(
            split_folded_tail(body),
            ("See you then.", Some("-- \nSam Taylor\nExample Ltd"))
        );

        let long = format!("Intro\n--\n{}", "line\n".repeat(20));
        assert_eq!(split_folded_tail(&long).1, None);
    }

    #[test]
    fn a_signature_above_a_trailing_quote_folds_with_it() {
        let body = "Sure.\n--\nSam\n\nOn Tue, Priya wrote:\n> Lunch?";
        assert_eq!(
            split_folded_tail(body),
            ("Sure.", Some("--\nSam\n\nOn Tue, Priya wrote:\n> Lunch?"))
        );
    }

    #[test]
    fn forwarded_messages_stay_visible() {
        let body = "FYI\n\n---------- Forwarded message ----------\nFrom: Priya\n\nThe plan.";
        assert_eq!(split_folded_tail(body), (body, None));
    }
}
