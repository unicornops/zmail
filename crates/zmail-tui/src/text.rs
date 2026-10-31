//! Display widths, truncation and wrapping, in terminal cells.
//!
//! Every string here has already been through
//! [`zmail_security::terminal::sanitize`], so it holds no control
//! characters; wide characters (CJK, most emoji) take two cells, and
//! combining marks none, as `unicode-width` measures them.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The cells `text` takes.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// The cells `ch` takes.
pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// `text` cut to at most `max` cells, ending in `ellipsis` when it was
/// cut. A wide character that would straddle the edge is dropped.
pub fn truncate(text: &str, max: usize, ellipsis: &str) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    let budget = max.saturating_sub(width(ellipsis));
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cells = char_width(ch);
        if used + cells > budget {
            break;
        }
        out.push(ch);
        used += cells;
    }
    if width(ellipsis) <= max {
        out.push_str(ellipsis);
    }
    out
}

/// `text` cut and padded with spaces to exactly `cells` cells.
pub fn fit(text: &str, cells: usize, ellipsis: &str) -> String {
    let mut out = truncate(text, cells, ellipsis);
    let used = width(&out);
    out.extend(std::iter::repeat_n(' ', cells.saturating_sub(used)));
    out
}

/// `text` right-aligned in `cells` cells.
pub fn fit_right(text: &str, cells: usize, ellipsis: &str) -> String {
    let cut = truncate(text, cells, ellipsis);
    let mut out = " ".repeat(cells.saturating_sub(width(&cut)));
    out.push_str(&cut);
    out
}

/// One row of wrapped text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedRow {
    /// The row's text, at most the wrap width.
    pub text: String,
    /// How this row joins the one before it when the text is copied:
    /// `None` when it starts a line of the original, `Some(" ")` when the
    /// wrap replaced a space, `Some("")` when it broke a long word.
    pub joiner: Option<&'static str>,
    /// Where `text` starts in the wrapped string, in bytes.
    pub start: usize,
}

/// Wraps each line of `text` to `cells` cells: at spaces where it can,
/// inside a word longer than a row where it must. Trailing spaces stay with
/// the row they end; the space a wrap replaces is dropped and recorded in
/// [`WrappedRow::joiner`].
pub fn wrap(text: &str, cells: usize) -> Vec<WrappedRow> {
    let cells = cells.max(1);
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        let start = line_start;
        line_start += line.len() + 1;
        let line = line.trim_end();
        if width(line) <= cells {
            rows.push(WrappedRow {
                text: line.to_owned(),
                joiner: None,
                start,
            });
            continue;
        }
        let mut joiner = None;
        let mut rest = line;
        // Where `rest` starts in `text`.
        let offset = |rest: &str| start + line.len() - rest.len();
        while !rest.is_empty() {
            if width(rest) <= cells {
                rows.push(WrappedRow {
                    text: rest.to_owned(),
                    joiner,
                    start: offset(rest),
                });
                break;
            }
            // The longest prefix that fits, then back to its last space.
            let mut fits = 0;
            let mut used = 0;
            for (index, ch) in rest.char_indices() {
                let next = used + char_width(ch);
                if next > cells {
                    break;
                }
                used = next;
                fits = index + ch.len_utf8();
            }
            // At least one character per row, however narrow.
            let fits = fits.max(rest.chars().next().map_or(0, char::len_utf8));
            // A space right after the row breaks there too.
            let window = if rest[fits..].starts_with(' ') {
                fits + 1
            } else {
                fits
            };
            let space = rest[..window].rfind(' ').filter(|&index| index > 0);
            match space {
                Some(space) => {
                    rows.push(WrappedRow {
                        text: rest[..space].trim_end().to_owned(),
                        joiner,
                        start: offset(rest),
                    });
                    rest = &rest[space + 1..];
                    joiner = Some(" ");
                    // Any further spaces at the break are part of the
                    // separator too.
                    while let Some(after) = rest.strip_prefix(' ') {
                        rest = after;
                    }
                }
                None => {
                    rows.push(WrappedRow {
                        text: rest[..fits].to_owned(),
                        joiner,
                        start: offset(rest),
                    });
                    rest = &rest[fits..];
                    joiner = Some("");
                }
            }
        }
    }
    rows
}

/// The byte range of the characters of `text` that cover cells
/// `start..end`. A wide character is in the range when its first cell is.
pub fn cell_range(text: &str, start: usize, end: usize) -> std::ops::Range<usize> {
    let mut column = 0;
    let mut from = text.len();
    let mut to = text.len();
    for (index, ch) in text.char_indices() {
        if column >= start && from == text.len() {
            from = index;
        }
        if column >= end {
            to = index;
            break;
        }
        column += char_width(ch);
    }
    from.min(to)..to
}

/// The cell `byte` of `text` starts at.
pub fn column_of(text: &str, byte: usize) -> usize {
    width(&text[..text.floor_char_boundary(byte)])
}

#[cfg(test)]
mod tests {
    use super::{WrappedRow, cell_range, fit, fit_right, truncate, width, wrap};

    #[test]
    fn widths_count_wide_characters_twice() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本"), 4);
        assert_eq!(width("📬"), 2);
        assert_eq!(width("e\u{301}"), 1);
    }

    #[test]
    fn truncation_is_width_aware() {
        assert_eq!(truncate("Hello world", 8, "…"), "Hello w…");
        assert_eq!(truncate("Hello", 8, "…"), "Hello");
        // The third wide character would straddle the edge.
        assert_eq!(truncate("日本語です", 6, "…"), "日本…");
        assert_eq!(truncate("abcdef", 4, "..."), "a...");
        assert_eq!(fit("ab", 4, "…"), "ab  ");
        assert_eq!(fit("日本語", 5, "…"), "日本…");
        assert_eq!(fit_right("10:42", 9, "…"), "    10:42");
    }

    fn rows(rows: &[(&str, Option<&'static str>, usize)]) -> Vec<WrappedRow> {
        rows.iter()
            .map(|(text, joiner, start)| WrappedRow {
                text: (*text).to_owned(),
                joiner: *joiner,
                start: *start,
            })
            .collect()
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_records_the_joins() {
        assert_eq!(
            wrap("the quick brown fox\nsecond", 10),
            rows(&[
                ("the quick", None, 0),
                ("brown fox", Some(" "), 10),
                ("second", None, 20)
            ])
        );
        assert_eq!(
            wrap("abcdefghijkl mn", 5),
            rows(&[
                ("abcde", None, 0),
                ("fghij", Some(""), 5),
                ("kl mn", Some(""), 10)
            ])
        );
        assert_eq!(wrap("", 5), rows(&[("", None, 0)]));
        assert_eq!(
            wrap("日本語の文章です", 6),
            rows(&[
                ("日本語", None, 0),
                ("の文章", Some(""), 9),
                ("です", Some(""), 18)
            ])
        );
    }

    #[test]
    fn cell_ranges_map_columns_to_bytes() {
        assert_eq!(&"hello world"[cell_range("hello world", 6, 11)], "world");
        assert_eq!(&"日本語"[cell_range("日本語", 2, 4)], "本");
        assert_eq!(&"ab"[cell_range("ab", 1, 10)], "b");
        assert_eq!(&"ab"[cell_range("ab", 5, 10)], "");
    }
}
