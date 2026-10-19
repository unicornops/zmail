//! A message body: paragraphs at a readable measure, selectable, with
//! clickable links.
//!
//! The body is split into paragraphs at blank lines and laid out with
//! space between them, no wider than about 72 characters. Each paragraph
//! is a [`SelectableText`] around an `InteractiveText`: the reader keeps
//! one selection, as byte offsets into the whole body, so a drag can run
//! across paragraphs and copying keeps the blank lines between them.

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
    /// The links, in order.
    pub(crate) links: Vec<BodyLink>,
}

impl BodyModel {
    pub(super) fn new(message: &ThreadMessage) -> Self {
        let text = message.rendered_body.clone();
        // The rendered text came from the stored HTML only when there is
        // no plain-text body.
        let html = message
            .body_text
            .is_none()
            .then_some(message.sanitized_html.as_deref())
            .flatten();
        let fold_start = folded_tail_start(&text);
        Self {
            paragraphs: paragraphs(&text, fold_start),
            links: body_links(&text, html),
            fold_start,
            text: text.into(),
        }
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

/// The highlight runs for one paragraph: its links, and the part of the
/// selection inside it, both relative to the paragraph. Sorted and
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
    /// The selection in this message, if it is here.
    pub(super) selection: Option<Range<usize>>,
    /// A drag is selecting in this message.
    pub(super) dragging: bool,
    pub(super) colors: UiColors,
}

/// The paragraphs above the fold (`folded` false) or below it, selectable,
/// with their links.
pub(super) fn render_paragraphs(
    body: &BodyContext<'_>,
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
    let paragraphs: Vec<&Range<usize>> = body.model.paragraphs(folded).collect();
    let all = &body.model.paragraphs;
    let count = paragraphs.len();

    div()
        .flex()
        .flex_col()
        .gap_3()
        .w_full()
        .max_w(rems(BODY_MEASURE_REM))
        .text_sm()
        .line_height(rems(BODY_LINE_HEIGHT_REM))
        .when(folded, |this| this.text_color(rgb(colors.muted)))
        .children(
            paragraphs
                .into_iter()
                .enumerate()
                .map(|(index, paragraph)| {
                    let text = &body.model.text[paragraph.clone()];
                    // Links inside this paragraph, by their index in the body.
                    let links: Vec<(usize, Range<usize>)> = body
                        .model
                        .links
                        .iter()
                        .enumerate()
                        .filter(|(_, link)| {
                            paragraph.start <= link.range.start && link.range.end <= paragraph.end
                        })
                        .map(|(link_index, link)| {
                            (
                                link_index,
                                link.range.start - paragraph.start
                                    ..link.range.end - paragraph.start,
                            )
                        })
                        .collect();
                    let local_links: Vec<Range<usize>> =
                        links.iter().map(|(_, range)| range.clone()).collect();
                    let selection = body.selection.as_ref().and_then(|selection| {
                        let start = selection.start.max(paragraph.start);
                        let end = selection.end.min(paragraph.end);
                        (start < end).then(|| start - paragraph.start..end - paragraph.start)
                    });
                    let styled =
                        StyledText::new(SharedString::from(text.to_owned())).with_highlights(
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
                    let interactive = InteractiveText::new(
                        (
                            "reader-paragraph-text",
                            (message_id as u64) << 16 | paragraph.start as u64,
                        ),
                        styled,
                    )
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
                            .map(|(_, target)| {
                                text_tooltip(target.clone(), tooltip_colors)(window, cx)
                            })
                    });
                    let paragraph_index = all
                        .iter()
                        .position(|candidate| candidate == paragraph)
                        .unwrap_or_default();
                    div()
                        .debug_selector(move || {
                            format!("reader-paragraph-{message_index}-{paragraph_index}")
                        })
                        .child(
                            SelectableText::new(
                                (
                                    "reader-paragraph",
                                    (message_id as u64) << 16 | paragraph.start as u64,
                                ),
                                interactive,
                                layout,
                                paragraph.clone(),
                                on_input.clone(),
                            )
                            .edges(index == 0, index + 1 == count)
                            .dragging(body.dragging),
                        )
                }),
        )
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

    use super::{highlights, paragraphs};

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
