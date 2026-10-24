//! A message body: paragraphs at a readable measure, selectable, with
//! clickable links.
//!
//! The body is split into paragraphs at blank lines and laid out with
//! space between them, no wider than about 72 characters. A long
//! paragraph is laid out in pieces of a few lines each, and the pieces of
//! a long body are spread over several items of the conversation list
//! ([`BodyModel::chunks`]), so opening a huge message shapes only the part
//! on screen. Each piece is a [`SelectableText`] around an
//! `InteractiveText`: the reader keeps one selection, as byte offsets into
//! the whole body, so a drag can run across paragraphs, pieces and list
//! items, and copying keeps the line breaks between them.

use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, ClipboardItem, Context, HighlightStyle, Hsla, InteractiveText, IntoElement,
    SharedString, StyledText, UnderlineStyle, Window, div, prelude::*, px, rems, rgb,
};
use unicode_segmentation::UnicodeSegmentation;
use zmail_core::ThreadMessage;

use super::ReaderView;
use super::links::{BodyLink, body_links};
use super::quoted::folded_tail_start;
use super::selectable::{SelectableText, SelectionInput};
use crate::components::text_input;
use crate::styles::UiColors;
use crate::tooltip::text_tooltip;

/// The widest a body gets: about 72 characters of body text.
const BODY_MEASURE_REM: f32 = 39.;

/// Line height of body text, roomier than the UI's for reading.
const BODY_LINE_HEIGHT_REM: f32 = 1.375;

/// A paragraph longer than this many bytes is laid out in pieces of
/// whole lines (or, for a longer line, words) of about this size.
const PIECE_BYTES: usize = 2_000;

/// About how many bytes of body text one conversation list item lays out.
const CHUNK_BYTES: usize = 6_000;

/// A message body prepared for display. Built once per message and kept
/// while the conversation is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyModel {
    /// The whole body.
    pub(crate) text: SharedString,
    /// Where the folded quote or signature starts, if anything folds.
    pub(crate) fold_start: Option<usize>,
    /// The paragraphs, as byte ranges of `text`, in order.
    pub(crate) paragraphs: Vec<Range<usize>>,
    /// The pieces the paragraphs are laid out in, in order: a paragraph is
    /// one piece unless it is longer than [`PIECE_BYTES`].
    pub(crate) pieces: Vec<Piece>,
    /// The links, in order.
    pub(crate) links: Vec<BodyLink>,
}

/// A run of a paragraph's lines, laid out as one text element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    /// The piece's text, as a byte range of the body.
    pub(crate) range: Range<usize>,
    /// The index of its paragraph.
    pub(crate) paragraph: usize,
    /// Whether it starts its paragraph.
    pub(crate) starts_paragraph: bool,
}

impl BodyModel {
    pub(super) fn new(message: &ThreadMessage) -> Self {
        let text = message.rendered_body.clone();
        let fold_start = folded_tail_start(&text);
        let paragraphs = paragraphs(&text, fold_start);
        let links = body_links(&text, &message.body_links);
        Self {
            pieces: pieces(&text, &paragraphs, &links),
            paragraphs,
            links,
            fold_start,
            text: text.into(),
        }
    }

    /// How many pieces are above the fold: they come first.
    pub(super) fn visible_pieces(&self) -> usize {
        let fold = self.fold_start.unwrap_or(self.text.len());
        self.pieces
            .partition_point(|piece| piece.range.start < fold)
    }

    /// The pieces each conversation list item lays out, as index ranges
    /// of [`Self::pieces`]: those above the fold, and those below it when
    /// `unfolded`, about [`CHUNK_BYTES`] of text per item. A short body is
    /// one chunk.
    pub(super) fn chunks(&self, unfolded: bool) -> Vec<Range<usize>> {
        let shown = if unfolded {
            self.pieces.len()
        } else {
            self.visible_pieces()
        };
        let mut chunks = Vec::new();
        let mut start = 0;
        let mut bytes = 0;
        for (index, piece) in self.pieces[..shown].iter().enumerate() {
            bytes += piece.range.len();
            if bytes >= CHUNK_BYTES && index + 1 < shown {
                chunks.push(start..index + 1);
                start = index + 1;
                bytes = 0;
            }
        }
        chunks.push(start..shown);
        chunks
    }

    /// The paragraphs above the fold, or below it.
    pub(super) fn paragraphs(&self, folded: bool) -> impl Iterator<Item = &Range<usize>> {
        let fold = self.fold_start.unwrap_or(self.text.len());
        self.paragraphs
            .iter()
            .filter(move |paragraph| (paragraph.start >= fold) == folded)
    }

    /// The end of the text above the fold.
    pub(super) fn visible_end(&self) -> usize {
        self.paragraphs(false)
            .last()
            .map_or(0, |paragraph| paragraph.end)
    }
}

/// Splits `text` into paragraphs at blank lines, and at `fold_start`.
/// Each range covers its lines without the line break after the last.
fn paragraphs(text: &str, fold_start: Option<usize>) -> Vec<Range<usize>> {
    let mut paragraphs = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0;
    let mut offset = 0;
    for line in text.split('\n') {
        let line_end = offset + line.len();
        if Some(offset) == fold_start
            && let Some(paragraph_start) = start.take()
        {
            paragraphs.push(paragraph_start..end);
        }
        if line.trim().is_empty() {
            if let Some(paragraph_start) = start.take() {
                paragraphs.push(paragraph_start..end);
            }
        } else {
            start.get_or_insert(offset);
            end = line_end;
        }
        offset = line_end + 1;
    }
    if let Some(paragraph_start) = start {
        paragraphs.push(paragraph_start..end);
    }
    paragraphs
}

/// Splits the paragraphs longer than [`PIECE_BYTES`] into pieces at line
/// breaks, or, for a line longer than that, at a space (or anywhere, for
/// a run without one). A cut never falls inside a link. The line break or
/// space a cut falls on belongs to no piece; the selection still copies it.
fn pieces(text: &str, paragraphs: &[Range<usize>], links: &[BodyLink]) -> Vec<Piece> {
    let inside_link = |offset: usize| {
        let after = links.partition_point(|link| link.range.start < offset);
        after > 0 && links[after - 1].range.end > offset
    };
    let mut pieces = Vec::new();
    for (paragraph_index, paragraph) in paragraphs.iter().enumerate() {
        let mut start = paragraph.start;
        while paragraph.end - start > PIECE_BYTES {
            let window = start..text.floor_char_boundary(start + PIECE_BYTES);
            let cut_at = |separator: char| {
                text[window.clone()]
                    .rmatch_indices(separator)
                    .map(|(index, _)| start + index)
                    .find(|&cut| cut > start && !inside_link(cut))
            };
            let (end, next) = match cut_at('\n').or_else(|| cut_at(' ')) {
                Some(cut) => (cut, cut + 1),
                None if window.end > start => (window.end, window.end),
                // A single character longer than a piece: impossible for
                // UTF-8, but never loop.
                None => break,
            };
            pieces.push(Piece {
                range: start..end,
                paragraph: paragraph_index,
                starts_paragraph: start == paragraph.start,
            });
            start = next;
        }
        pieces.push(Piece {
            range: start..paragraph.end,
            paragraph: paragraph_index,
            starts_paragraph: start == paragraph.start,
        });
    }
    pieces
}

/// The highlight runs for one piece: its links, and the part of the
/// selection inside it, both relative to the piece. Sorted and
/// non-overlapping, as `StyledText` needs them.
fn highlights(
    links: &[Range<usize>],
    selection: Option<Range<usize>>,
    link_style: HighlightStyle,
    selection_style: HighlightStyle,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut edges: Vec<usize> = links
        .iter()
        .chain(selection.iter())
        .flat_map(|range| [range.start, range.end])
        .collect();
    edges.sort_unstable();
    edges.dedup();
    edges
        .windows(2)
        .filter_map(|pair| {
            let segment = pair[0]..pair[1];
            let in_link = links
                .iter()
                .any(|link| link.start <= segment.start && segment.end <= link.end);
            let selected = selection
                .as_ref()
                .is_some_and(|range| range.start <= segment.start && segment.end <= range.end);
            let style = match (in_link, selected) {
                (false, false) => return None,
                (true, false) => link_style,
                (false, true) => selection_style,
                (true, true) => HighlightStyle {
                    underline: link_style.underline,
                    ..selection_style
                },
            };
            Some((segment, style))
        })
        .collect()
}

/// What the body of one message renders with.
pub(super) struct BodyContext<'a> {
    pub(super) message_id: i64,
    /// The message's position in the conversation.
    pub(super) index: usize,
    pub(super) model: &'a BodyModel,
    /// The pieces this list item lays out (one chunk of the body).
    pub(super) pieces: Range<usize>,
    /// The selection in this message, if it is here.
    pub(super) selection: Option<Range<usize>>,
    /// A drag is selecting in this message.
    pub(super) dragging: bool,
    pub(super) colors: UiColors,
}

/// The pieces `pieces` (indices into [`BodyModel::pieces`], all above the
/// fold or all below it, as `folded` says), selectable, with their links.
/// A piece that starts a paragraph has space above it, unless it is the
/// first here.
pub(super) fn render_pieces(
    body: &BodyContext<'_>,
    pieces: Range<usize>,
    folded: bool,
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &body.colors;
    let reader = cx.entity().downgrade();
    let message_id = body.message_id;
    let message_index = body.index;
    let on_input = {
        let reader = reader.clone();
        Rc::new(move |input: SelectionInput, _: &mut Window, cx: &mut App| {
            let _ = reader.update(cx, |reader, cx| {
                reader.selection_input(message_id, input, cx);
            });
        })
    };
    let accent: Hsla = rgb(colors.accent).into();
    let link_style = HighlightStyle {
        color: Some(accent),
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(accent),
            wavy: false,
        }),
        ..HighlightStyle::default()
    };
    let selection_style = HighlightStyle {
        color: Some(rgb(colors.selection_foreground).into()),
        background_color: Some(rgb(colors.selection_background).into()),
        ..HighlightStyle::default()
    };
    // The side of the fold these pieces are on: a drag past its first or
    // last piece selects to that end.
    let visible = body.model.visible_pieces();
    let side = if folded {
        visible..body.model.pieces.len()
    } else {
        0..visible
    };
    let first_here = pieces.start;

    div()
        .flex()
        .flex_col()
        .w_full()
        .max_w(rems(BODY_MEASURE_REM))
        .text_sm()
        .line_height(rems(BODY_LINE_HEIGHT_REM))
        .when(folded, |this| this.text_color(rgb(colors.muted)))
        .children(pieces.map(|piece_index| {
            let piece = &body.model.pieces[piece_index];
            let range = piece.range.clone();
            let text = &body.model.text[range.clone()];
            // Links inside this piece, by their index in the body.
            let links: Vec<(usize, Range<usize>)> = body
                .model
                .links
                .iter()
                .enumerate()
                .filter(|(_, link)| range.start <= link.range.start && link.range.end <= range.end)
                .map(|(link_index, link)| {
                    (
                        link_index,
                        link.range.start - range.start..link.range.end - range.start,
                    )
                })
                .collect();
            let local_links: Vec<Range<usize>> =
                links.iter().map(|(_, range)| range.clone()).collect();
            let selection = body.selection.as_ref().and_then(|selection| {
                let start = selection.start.max(range.start);
                let end = selection.end.min(range.end);
                (start < end).then(|| start - range.start..end - range.start)
            });
            let styled = StyledText::new(SharedString::from(text.to_owned())).with_highlights(
                highlights(&local_links, selection, link_style, selection_style),
            );
            let layout = styled.layout().clone();
            let link_targets: Vec<(Range<usize>, String)> = links
                .iter()
                .map(|(link_index, range)| {
                    (range.clone(), body.model.links[*link_index].target.clone())
                })
                .collect();
            let tooltip_colors = *colors;
            let link_indices: Vec<usize> = links.iter().map(|(index, _)| *index).collect();
            let reader = reader.clone();
            // Unique per message and piece (bodies stay well under 4 GB).
            let element_id = (message_id as u64) << 32 | range.start as u64;
            let interactive = InteractiveText::new(("reader-paragraph-text", element_id), styled)
                .on_click(local_links, move |clicked, window, cx| {
                    let link = link_indices[clicked];
                    let _ = reader.update(cx, |reader, cx| {
                        reader.link_clicked(message_id, link, window, cx);
                    });
                })
                .tooltip(move |index, window, cx| {
                    link_targets
                        .iter()
                        .find(|(range, _)| range.contains(&index))
                        .map(|(_, target)| text_tooltip(target.clone(), tooltip_colors)(window, cx))
                });
            let paragraph_index = piece.paragraph;
            let starts_paragraph = piece.starts_paragraph;
            div()
                .debug_selector(move || {
                    if starts_paragraph {
                        format!("reader-paragraph-{message_index}-{paragraph_index}")
                    } else {
                        format!("reader-piece-{message_index}-{piece_index}")
                    }
                })
                .when(starts_paragraph && piece_index != first_here, |this| {
                    this.mt_3()
                })
                .child(
                    SelectableText::new(
                        ("reader-paragraph", element_id),
                        interactive,
                        layout,
                        range,
                        on_input.clone(),
                    )
                    .edges(piece_index == side.start, piece_index + 1 == side.end)
                    .dragging(body.dragging),
                )
        }))
        .into_any_element()
}

/// The reader's text selection: inside one message's body, as byte
/// offsets into it. `anchor` is where it started, `head` where it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BodySelection {
    pub(crate) message_id: i64,
    pub(crate) anchor: usize,
    pub(crate) head: usize,
}

impl BodySelection {
    /// The selected range, start first.
    pub(crate) fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

impl ReaderView {
    /// The body of `message`, prepared once while the conversation is open.
    pub(super) fn body_model(&mut self, message: &ThreadMessage) -> Rc<BodyModel> {
        self.bodies
            .entry(message.id)
            .or_insert_with(|| Rc::new(BodyModel::new(message)))
            .clone()
    }

    /// The selected range of `message_id`'s body, if the selection is
    /// there.
    pub(super) fn selection_in(&self, message_id: i64) -> Option<Range<usize>> {
        self.selection
            .filter(|selection| selection.message_id == message_id)
            .map(|selection| selection.range())
            .filter(|range| !range.is_empty())
    }

    /// Whether some text is selected.
    pub(super) fn has_selection(&self) -> bool {
        self.selection
            .is_some_and(|selection| selection.anchor != selection.head)
    }

    /// The selected text.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let body = self.bodies.get(&selection.message_id)?;
        let range = selection.range();
        let text = body
            .text
            .get(range.start.min(body.text.len())..range.end.min(body.text.len()))?;
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// A press, drag or release on a paragraph of `message_id`'s body.
    pub(super) fn selection_input(
        &mut self,
        message_id: i64,
        input: SelectionInput,
        cx: &mut Context<Self>,
    ) {
        let before = (self.selection, self.dragging);
        match input {
            SelectionInput::Press {
                offset,
                click_count,
                extend,
            } => {
                self.press_selected = true;
                let same_message = self
                    .selection
                    .is_some_and(|selection| selection.message_id == message_id);
                let (anchor, head) = match click_count {
                    _ if extend && same_message => {
                        let anchor = self.selection.map_or(offset, |selection| selection.anchor);
                        (anchor, offset)
                    }
                    1 => (offset, offset),
                    2 => self.word_at(message_id, offset),
                    _ => self.paragraph_at(message_id, offset),
                };
                self.selection = Some(BodySelection {
                    message_id,
                    anchor,
                    head,
                });
                self.dragging = click_count == 1 || extend;
            }
            SelectionInput::Drag(offset) => {
                if let Some(selection) = self.selection.as_mut()
                    && self.dragging
                    && selection.message_id == message_id
                {
                    selection.head = offset;
                }
            }
            SelectionInput::Release => self.dragging = false,
        }
        if (self.selection, self.dragging) != before {
            cx.notify();
        }
    }

    /// The word around `offset`, as an (anchor, head) pair.
    fn word_at(&self, message_id: i64, offset: usize) -> (usize, usize) {
        let Some(body) = self.bodies.get(&message_id) else {
            return (offset, offset);
        };
        body.text
            .split_word_bound_indices()
            .map(|(start, word)| (start, start + word.len()))
            .find(|(start, end)| *start <= offset && offset < *end)
            .unwrap_or((offset, offset))
    }

    /// The paragraph around `offset`, as an (anchor, head) pair.
    fn paragraph_at(&self, message_id: i64, offset: usize) -> (usize, usize) {
        self.bodies
            .get(&message_id)
            .and_then(|body| {
                body.paragraphs
                    .iter()
                    .find(|paragraph| paragraph.start <= offset && offset <= paragraph.end)
            })
            .map_or((offset, offset), |paragraph| {
                (paragraph.start, paragraph.end)
            })
    }

    /// The current message, when it is expanded.
    fn current_expanded_message(&self) -> Option<ThreadMessage> {
        let index = self.conversation.current()?;
        let message = self.state.thread.as_ref()?.messages.get(index)?;
        self.conversation
            .is_expanded(message.id)
            .then(|| message.clone())
    }

    /// Selects paragraph `paragraph` of the current message (the
    /// screenshot fixture's selection).
    pub(crate) fn select_paragraph(&mut self, paragraph: usize, cx: &mut Context<Self>) {
        let Some(message) = self.current_expanded_message() else {
            return;
        };
        let body = self.body_model(&message);
        if let Some(range) = body.paragraphs.get(paragraph) {
            self.selection = Some(BodySelection {
                message_id: message.id,
                anchor: range.start,
                head: range.end,
            });
            cx.notify();
        }
    }

    /// Clicks the current message's first link (the screenshot fixture's
    /// open-link dialog).
    pub(crate) fn open_first_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.current_expanded_message() else {
            return;
        };
        if !self.body_model(&message).links.is_empty() {
            self.link_clicked(message.id, 0, window, cx);
        }
    }

    /// A press anywhere in the reader that no paragraph took clears the
    /// selection.
    pub(super) fn press_outside_text(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.press_selected) && self.selection.take().is_some() {
            self.dragging = false;
            cx.notify();
        }
    }

    /// Secondary+C and Edit → Copy: copies the selection. Without one the
    /// command goes on to whoever else handles it.
    pub(super) fn copy(&mut self, _: &text_input::Copy, _: &mut Window, cx: &mut Context<Self>) {
        match self.selected_text() {
            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            // Text selected in a message's web view.
            None if self.copy_from_web() => {}
            None => cx.propagate(),
        }
    }

    /// Secondary+A and Edit → Select All: selects the current message's
    /// text above the fold.
    pub(super) fn select_all(
        &mut self,
        _: &text_input::SelectAll,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.current_expanded_message() else {
            cx.propagate();
            return;
        };
        if self.select_all_in_web(&message) {
            return;
        }
        let body = self.body_model(&message);
        self.selection = Some(BodySelection {
            message_id: message.id,
            anchor: 0,
            head: body.visible_end(),
        });
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use gpui::HighlightStyle;

    use super::{BodyLink, CHUNK_BYTES, PIECE_BYTES, highlights, paragraphs, pieces};

    #[test]
    fn paragraphs_split_at_blank_lines_and_the_fold() {
        let text = "Hi Sam,\n\nLine one\nline two\n\n\n-- \nGrace";
        let ranges = paragraphs(text, None);
        let parts: Vec<&str> = ranges.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(parts, ["Hi Sam,", "Line one\nline two", "-- \nGrace"]);

        let fold = text.find("line two").expect("the line is there");
        let ranges = paragraphs(text, Some(fold));
        let parts: Vec<&str> = ranges.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(parts, ["Hi Sam,", "Line one", "line two", "-- \nGrace"]);
    }

    #[test]
    fn a_long_paragraph_is_cut_into_pieces_at_line_breaks_spaces_and_not_links() {
        let line = "word ".repeat(30);
        let lines = vec![line.trim_end(); 40].join("\n");
        let long_line = "word ".repeat(1_000);
        let text = format!("Short.\n\n{lines}\n\n{}", long_line.trim_end());
        let paragraphs = paragraphs(&text, None);
        assert_eq!(paragraphs.len(), 3);
        let link_start = text.find("word\nword").expect("a line break");
        let links = vec![BodyLink {
            range: link_start..link_start + 9,
            target: "https://example.com".to_owned(),
        }];
        let pieces = pieces(&text, &paragraphs, &links);

        // The short paragraph is one piece.
        assert_eq!(pieces[0].range, paragraphs[0]);
        assert!(pieces[0].starts_paragraph);
        for piece in &pieces {
            let piece_text = &text[piece.range.clone()];
            assert!(piece.range.len() <= PIECE_BYTES, "{}", piece.range.len());
            assert!(!piece_text.starts_with(['\n', ' ']), "{piece_text:?}");
            assert!(
                !(piece.range.start < links[0].range.end && links[0].range.start < piece.range.end)
                    || (piece.range.start <= links[0].range.start
                        && links[0].range.end <= piece.range.end),
                "no piece cuts the link"
            );
        }
        // Each paragraph's pieces cover it, cut at line breaks or spaces.
        for (index, paragraph) in paragraphs.iter().enumerate() {
            let of_paragraph: Vec<_> = pieces
                .iter()
                .filter(|piece| piece.paragraph == index)
                .collect();
            assert!(of_paragraph[0].starts_paragraph);
            assert!(
                of_paragraph[1..]
                    .iter()
                    .all(|piece| !piece.starts_paragraph)
            );
            assert_eq!(of_paragraph[0].range.start, paragraph.start);
            assert_eq!(
                of_paragraph.last().map(|piece| piece.range.end),
                Some(paragraph.end)
            );
            for pair in of_paragraph.windows(2) {
                let gap = &text[pair[0].range.end..pair[1].range.start];
                assert!(gap == "\n" || gap == " ", "{gap:?}");
            }
        }
        assert!(pieces.iter().filter(|piece| piece.paragraph == 1).count() > 1);
        assert!(pieces.iter().filter(|piece| piece.paragraph == 2).count() > 1);
    }

    #[test]
    fn a_long_body_is_chunked_and_a_short_one_is_not() {
        let model = |text: &str| super::BodyModel {
            text: text.to_owned().into(),
            fold_start: text.find("\n> ").map(|at| at + 1),
            paragraphs: Vec::new(),
            pieces: Vec::new(),
            links: Vec::new(),
        };
        let with_pieces = |text: &str| {
            let mut body = model(text);
            body.paragraphs = paragraphs(text, body.fold_start);
            body.pieces = pieces(text, &body.paragraphs, &[]);
            body
        };
        let short = with_pieces("Hi.\n\nThanks.");
        let chunks = short.chunks(false);
        assert_eq!((chunks.len(), chunks[0].clone()), (1, 0..2));

        let paragraph = "word ".repeat(100);
        let mut text = vec![paragraph.trim_end(); 40].join("\n\n");
        text.push_str("\n> quoted\n> more");
        let long = with_pieces(&text);
        let visible = long.visible_pieces();
        assert_eq!(visible, 40);
        let chunks = long.chunks(false);
        assert!(chunks.len() > 2);
        assert_eq!(chunks.first().map(|chunk| chunk.start), Some(0));
        assert_eq!(chunks.last().map(|chunk| chunk.end), Some(visible));
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        for chunk in &chunks[..chunks.len() - 1] {
            let bytes: usize = long.pieces[chunk.clone()]
                .iter()
                .map(|piece| piece.range.len())
                .sum();
            assert!((CHUNK_BYTES..CHUNK_BYTES + PIECE_BYTES).contains(&bytes));
        }
        // Unfolded, the quoted pieces follow in the last chunk.
        assert_eq!(
            long.chunks(true).last().map(|chunk| chunk.end),
            Some(long.pieces.len())
        );
    }

    #[test]
    fn highlights_split_where_links_and_the_selection_overlap() {
        let link = HighlightStyle {
            font_weight: Some(gpui::FontWeight::BOLD),
            ..HighlightStyle::default()
        };
        let selected = HighlightStyle {
            fade_out: Some(0.5),
            ..HighlightStyle::default()
        };
        let runs = highlights(&[2..6, 10..12], Some(4..11), link, selected);
        let ranges: Vec<_> = runs.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, [2..4, 4..6, 6..10, 10..11, 11..12]);
        assert_eq!(runs[0].1, link);
        assert_eq!(runs[2].1, selected);
        assert_eq!(runs[1].1.fade_out, Some(0.5), "a selected link is selected");
        assert!(highlights(&[], None, link, selected).is_empty());
    }
}
