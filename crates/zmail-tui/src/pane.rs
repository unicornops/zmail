//! Panes: what each part of the screen drew, kept until the next frame so
//! that a click, a drag or a copy works on exactly what is on screen.
//!
//! Selection is Zellij's: it stays inside the pane it started in, a copy
//! takes the pane's drawn (and so already filtered) text, wrapped lines
//! rejoin as the line they came from, and trailing padding is dropped.

use std::ops::Range;

use ratatui::{layout::Rect, text::Span};

use crate::text::{cell_range, char_width};

/// The panes that can take a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneId {
    /// The Main Menu.
    Menu,
    /// The Folder List.
    Folders,
    /// The Message Index.
    Index,
    /// Message Text, full screen.
    Message,
    /// The preview pane beside the Message Index.
    Preview,
    /// Setup.
    Setup,
    /// Help.
    Help,
    /// The save prompt's folder list, or Compose's address book or file
    /// browser (`^T`).
    Picker,
    /// The postponed messages.
    Postponed,
    /// The Outbox.
    Outbox,
}

/// A line a pane draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaneLine {
    /// Its styled text.
    pub spans: Vec<Span<'static>>,
    /// How it joins the line before when copied: `None` on a new line,
    /// `Some(" ")` or `Some("")` when wrapping split one line.
    pub joiner: Option<&'static str>,
    /// The list item a click on it acts on.
    pub target: Option<usize>,
    /// Link references on it: their cells and the link each opens.
    pub links: Vec<(Range<usize>, usize)>,
}

impl PaneLine {
    /// A line of one span.
    pub fn new(span: Span<'static>) -> Self {
        Self {
            spans: vec![span],
            ..Self::default()
        }
    }

    /// Its text, as drawn.
    pub fn text(&self) -> String {
        self.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}

/// A pane as last drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawnPane {
    /// Which pane.
    pub id: PaneId,
    /// Where it is on screen.
    pub area: Rect,
    /// The first line shown.
    pub scroll: usize,
    /// All its lines, shown or not.
    pub lines: Vec<PaneLine>,
}

impl DrawnPane {
    /// The line and cell under screen position (`x`, `y`), clamped to the
    /// pane's lines (rows above or below it give the first or last line
    /// shown, for dragging past an edge).
    pub fn point_at(&self, x: u16, y: u16) -> Point {
        let row = usize::from(y.saturating_sub(self.area.y))
            .min(usize::from(self.area.height.saturating_sub(1)));
        let line = (self.scroll + row).min(self.lines.len().saturating_sub(1));
        let column = usize::from(x.saturating_sub(self.area.x))
            .min(usize::from(self.area.width.saturating_sub(1)));
        Point { line, column }
    }

    /// Whether screen position (`x`, `y`) is inside the pane.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.area.x
            && x < self.area.x + self.area.width
            && y >= self.area.y
            && y < self.area.y + self.area.height
    }

    /// How many lines fit.
    pub fn height(&self) -> usize {
        usize::from(self.area.height)
    }
}

/// A cell of a pane's text: a line (of all its lines) and a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Point {
    /// The line.
    pub line: usize,
    /// The cell in the line.
    pub column: usize,
}

/// A selection in one pane, from where it started to where it is now,
/// both cells included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The pane.
    pub pane: PaneId,
    /// Where it started.
    pub anchor: Point,
    /// Where it ends now.
    pub head: Point,
}

impl Selection {
    /// Its first and last cells, in reading order.
    pub fn ordered(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether cell `column` of `line` is selected.
    pub fn contains(&self, line: usize, column: usize) -> bool {
        let (start, end) = self.ordered();
        let point = Point { line, column };
        start <= point && point <= end
    }
}

/// The text of `lines` from `start` to `end` (both cells included):
/// trailing padding dropped from each line, and lines a wrap split joined
/// again as they were.
pub fn selected_text(lines: &[PaneLine], start: Point, end: Point) -> String {
    let mut out = String::new();
    for index in start.line..=end.line.min(lines.len().saturating_sub(1)) {
        let Some(line) = lines.get(index) else {
            break;
        };
        let text = line.text();
        let from = if index == start.line { start.column } else { 0 };
        let to = if index == end.line {
            end.column + 1
        } else {
            usize::MAX
        };
        let piece = text[cell_range(&text, from, to)].trim_end();
        if index > start.line {
            out.push_str(line.joiner.unwrap_or("\n"));
        }
        out.push_str(piece);
    }
    out
}

/// The cells of the word at `column` of `text`: a run of characters
/// other than spaces. `None` on a space.
pub fn word_at(text: &str, column: usize) -> Option<(usize, usize)> {
    let mut cells = Vec::new();
    for ch in text.chars() {
        let width = char_width(ch).max(1);
        for _ in 0..width {
            cells.push(ch);
        }
    }
    if cells.get(column).is_none_or(|ch| ch.is_whitespace()) {
        return None;
    }
    let mut start = column;
    while start > 0 && !cells[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = column;
    while end + 1 < cells.len() && !cells[end + 1].is_whitespace() {
        end += 1;
    }
    Some((start, end))
}

/// The first and last of the pane lines that make up the line `line` is
/// part of (lines a wrap split).
pub fn logical_line(lines: &[PaneLine], line: usize) -> (usize, usize) {
    let mut first = line;
    while first > 0 && lines.get(first).is_some_and(|line| line.joiner.is_some()) {
        first -= 1;
    }
    let mut last = line;
    while lines
        .get(last + 1)
        .is_some_and(|line| line.joiner.is_some())
    {
        last += 1;
    }
    (first, last)
}

#[cfg(test)]
mod tests {
    use ratatui::text::Span;

    use super::{PaneLine, Point, logical_line, selected_text, word_at};

    fn line(text: &str, joiner: Option<&'static str>) -> PaneLine {
        PaneLine {
            spans: vec![Span::raw(text.to_owned())],
            joiner,
            ..PaneLine::default()
        }
    }

    #[test]
    fn copying_rejoins_wrapped_lines_and_drops_padding() {
        let lines = vec![
            line("The quick brown     ", None),
            line("fox jumps over the  ", Some(" ")),
            line("lazydogwithaverylong", Some(" ")),
            line("word.               ", Some("")),
            line("Next line           ", None),
        ];
        let all = selected_text(
            &lines,
            Point::default(),
            Point {
                line: 4,
                column: 19,
            },
        );
        assert_eq!(
            all,
            "The quick brown fox jumps over the lazydogwithaverylongword.\nNext line"
        );
        // A selection inside lines takes only the cells covered.
        let part = selected_text(
            &lines,
            Point { line: 0, column: 4 },
            Point { line: 1, column: 2 },
        );
        assert_eq!(part, "quick brown fox");
    }

    #[test]
    fn wide_characters_are_copied_whole() {
        let lines = vec![line("日本語 text", None)];
        let text = selected_text(
            &lines,
            Point { line: 0, column: 2 },
            Point { line: 0, column: 5 },
        );
        assert_eq!(text, "本語");
    }

    #[test]
    fn words_and_logical_lines_are_found() {
        assert_eq!(word_at("hello brave world", 8), Some((6, 10)));
        assert_eq!(word_at("hello brave world", 5), None);
        assert_eq!(word_at("日本 ab", 1), Some((0, 3)));
        let lines = vec![
            line("a", None),
            line("b", None),
            line("c", Some(" ")),
            line("d", Some("")),
            line("e", None),
        ];
        assert_eq!(logical_line(&lines, 2), (1, 3));
        assert_eq!(logical_line(&lines, 0), (0, 0));
        assert_eq!(logical_line(&lines, 4), (4, 4));
    }
}
