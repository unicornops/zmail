//! `SelectableText`: one paragraph of a message body that the mouse can
//! select, around an [`InteractiveText`] that makes its links clickable.
//!
//! GPUI's text elements have no selection, so this element adds it. It
//! reports presses and drags as byte offsets into the whole body; the
//! reader keeps the selection (one per window, inside one message) and
//! renders it back as a highlight. A paragraph owns the drags that fall
//! within its own lines; the first and last paragraphs also own drags
//! above and below the text, so a drag past either end selects to it.
//!
//! - A press starts a selection; Shift+press extends it.
//! - A double press selects the word, a triple press the paragraph.
//! - A press on a link that does not turn into a drag is the link's
//!   click, which [`InteractiveText`] reports.

use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, Element, ElementId, GlobalElementId,
    Hitbox, HitboxBehavior, InspectorElementId, InteractiveText, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, TextLayout, Window,
};

/// What the pointer did to a paragraph's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectionInput {
    /// A press at this body offset.
    Press {
        offset: usize,
        /// 1 for a single press, 2 for a double press, …
        click_count: usize,
        /// Shift was held: extend the selection.
        extend: bool,
    },
    /// The pointer dragged to this body offset.
    Drag(usize),
    /// The button was released.
    Release,
}

/// Receives a paragraph's [`SelectionInput`].
pub(super) type SelectionHandler = Rc<dyn Fn(SelectionInput, &mut Window, &mut App)>;

/// A selectable paragraph. See the module documentation.
pub(super) struct SelectableText {
    id: ElementId,
    text: AnyElement,
    layout: TextLayout,
    /// Where the paragraph starts in the body, and its length.
    range: Range<usize>,
    first: bool,
    last: bool,
    /// A drag is selecting in this paragraph's message.
    dragging: bool,
    on_input: SelectionHandler,
}

impl SelectableText {
    /// Wraps `text`, whose layout is `layout`, covering `range` of the
    /// body. `first` and `last` mark the body's first and last paragraph.
    pub(super) fn new(
        id: impl Into<ElementId>,
        text: InteractiveText,
        layout: TextLayout,
        range: Range<usize>,
        on_input: SelectionHandler,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into_any_element(),
            layout,
            range,
            first: false,
            last: false,
            dragging: false,
            on_input,
        }
    }

    /// Mark the paragraph as the first and/or last of the body.
    pub(super) fn edges(mut self, first: bool, last: bool) -> Self {
        self.first = first;
        self.last = last;
        self
    }

    /// Whether a drag is selecting in this paragraph's message.
    pub(super) fn dragging(mut self, dragging: bool) -> Self {
        self.dragging = dragging;
        self
    }
}

impl IntoElement for SelectableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The body offset under `position` in a paragraph laid out by `layout`,
/// the nearest one when the position is beside or between characters.
fn offset_at(layout: &TextLayout, range: &Range<usize>, position: Point<Pixels>) -> usize {
    let index = match layout.index_for_position(position) {
        Ok(index) | Err(index) => index,
    };
    range.start + index.min(range.len())
}

impl Element for SelectableText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.text.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        // Inserted before the text's own hitbox, so the link cursor (set
        // on that one) wins over the text cursor.
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.text.prepaint(window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        self.text.paint(window, cx);

        let layout = self.layout.clone();
        let range = self.range.clone();
        let on_input = self.on_input.clone();
        let press_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && press_hitbox.is_hovered(window)
            {
                on_input(
                    SelectionInput::Press {
                        offset: offset_at(&layout, &range, event.position),
                        click_count: event.click_count,
                        extend: event.modifiers.shift,
                    },
                    window,
                    cx,
                );
            }
        });

        if !self.dragging {
            return;
        }
        let layout = self.layout.clone();
        let range = self.range.clone();
        let on_input = self.on_input.clone();
        let (first, last) = (self.first, self.last);
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let y = event.position.y;
            let offset = if y < bounds.top() {
                first.then_some(range.start)
            } else if y >= bounds.bottom() {
                last.then_some(range.end)
            } else {
                Some(offset_at(&layout, &range, event.position))
            };
            if let Some(offset) = offset {
                on_input(SelectionInput::Drag(offset), window, cx);
            }
        });
        let on_input = self.on_input.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                on_input(SelectionInput::Release, window, cx);
            }
        });
    }
}
