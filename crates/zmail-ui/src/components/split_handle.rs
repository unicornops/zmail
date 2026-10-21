//! `SplitHandle`: the draggable divider between two panes.
//!
//! The handle does not own the width. The view passes the current width
//! in and gets the new, clamped width back through `on_resize` while the
//! pointer drags, then stores it (and persists it in the UI preferences'
//! pane widths). Arrow keys resize it too (Shift for bigger steps), Home
//! and End jump to the limits, and a double click restores the default.
//!
//! ```text
//! div().flex()
//!     .child(sidebar.w(px(self.sidebar_width)))
//!     .child(
//!         SplitHandle::new("sidebar-split", self.sidebar_width)
//!             .range(SIDEBAR_MIN_WIDTH, 420.0)
//!             .default_width(SIDEBAR_WIDTH)
//!             .on_resize(cx.listener(|this, width: &f32, _, cx| { this.sidebar_width = *width; cx.notify(); })),
//!     )
//!     .child(list)
//! ```

use std::rc::Rc;

use gpui::{
    AccessibleAction, App, Context, CursorStyle, DragMoveEvent, ElementId, IntoElement,
    KeyDownEvent, Orientation, Render, RenderOnce, Role, SharedString, Window, div, prelude::*, px,
    rgb,
};

use crate::a11y::AccessibleExt;

use super::theme::{ComponentTheme, FocusRingExt};

/// Width of the handle's hit area.
pub(crate) const SPLIT_HANDLE_WIDTH: f32 = 6.0;
/// Pixels moved by an arrow key.
pub(crate) const KEY_STEP: f32 = 8.0;
/// Pixels moved by Shift and an arrow key.
pub(crate) const KEY_STEP_LARGE: f32 = 48.0;

/// Which side of the handle the resized pane is on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneSide {
    /// The pane before (left of) the handle: dragging right widens it.
    #[default]
    Leading,
    /// The pane after (right of) the handle: dragging right narrows it.
    Trailing,
}

/// The drag payload. Each handle only follows drags it started.
#[derive(Debug, Clone)]
pub(crate) struct SplitDrag {
    id: ElementId,
}

/// The (empty) view GPUI draws under the pointer while dragging.
struct SplitDragPreview;

impl Render for SplitDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Clamp `width` to `min..=max`. A `max` below `min` is treated as `min`.
pub(crate) fn clamp_width(width: f32, min: f32, max: f32) -> f32 {
    width.min(max).max(min)
}

/// The width after the pointer moved `delta` pixels to the right of the
/// handle's centre.
pub(crate) fn dragged_width(width: f32, delta: f32, side: PaneSide, min: f32, max: f32) -> f32 {
    let signed = match side {
        PaneSide::Leading => delta,
        PaneSide::Trailing => -delta,
    };
    clamp_width((width + signed).round(), min, max)
}

/// Receives a pane's new width.
pub(crate) type ResizeHandler = Rc<dyn Fn(&f32, &mut Window, &mut App)>;

/// A vertical, resizable pane divider.
#[derive(IntoElement)]
pub(crate) struct SplitHandle {
    id: ElementId,
    width: f32,
    min: f32,
    max: f32,
    default_width: Option<f32>,
    side: PaneSide,
    label: SharedString,
    on_resize: Option<ResizeHandler>,
}

impl SplitHandle {
    /// A handle for a pane currently `width` pixels wide.
    pub(crate) fn new(id: impl Into<ElementId>, width: f32) -> Self {
        Self {
            id: id.into(),
            width,
            min: 0.0,
            max: f32::MAX,
            default_width: None,
            side: PaneSide::Leading,
            label: "Resize pane".into(),
            on_resize: None,
        }
    }

    /// The accessible name, such as "Resize sidebar" (default "Resize
    /// pane").
    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    /// The narrowest and widest the pane may get.
    pub(crate) fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// The width a double click restores.
    pub(crate) fn default_width(mut self, width: f32) -> Self {
        self.default_width = Some(width);
        self
    }

    /// Which side of the handle the pane is on (default leading).
    pub(crate) fn side(mut self, side: PaneSide) -> Self {
        self.side = side;
        self
    }

    /// Called with the new, clamped width whenever it changes.
    pub(crate) fn on_resize(
        mut self,
        handler: impl Fn(&f32, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_resize = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SplitHandle {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let Self {
            id,
            width,
            min,
            max,
            default_width,
            side,
            label,
            on_resize,
        } = self;
        let emit = move |new: f32, window: &mut Window, cx: &mut App| {
            if new != width
                && let Some(handler) = &on_resize
            {
                handler(&new, window, cx);
            }
        };
        let emit = Rc::new(emit);
        let drag_id = id.clone();
        let on_move = emit.clone();
        let on_key = emit.clone();
        let on_double = emit.clone();
        let on_increment = emit.clone();
        let on_decrement = emit;
        let accent = colors.focus_ring;

        div()
            .id(id.clone())
            .group("split-handle")
            .flex_none()
            .flex()
            .justify_center()
            .w(px(SPLIT_HANDLE_WIDTH))
            // Overlap the panes a little so the line sits on their border
            // and the hit area is wide without taking layout space.
            .mx(px(-(SPLIT_HANDLE_WIDTH - 1.0) / 2.0))
            .h_full()
            .cursor(CursorStyle::ResizeColumn)
            .tab_index(0)
            .focus_ring(&theme)
            .accessible(Role::Splitter, label)
            .aria_orientation(Orientation::Vertical)
            .aria_numeric_value(f64::from(width))
            .aria_min_numeric_value(f64::from(min))
            .aria_max_numeric_value(f64::from(max.max(min)))
            .aria_numeric_value_step(f64::from(KEY_STEP))
            // Assistive technology widens and narrows the pane itself.
            .on_a11y_action(AccessibleAction::Increment, move |_, window, cx| {
                on_increment(clamp_width(width + KEY_STEP, min, max), window, cx);
            })
            .on_a11y_action(AccessibleAction::Decrement, move |_, window, cx| {
                on_decrement(clamp_width(width - KEY_STEP, min, max), window, cx);
            })
            .on_drag(SplitDrag { id: id.clone() }, |_, _, _, cx| {
                cx.new(|_| SplitDragPreview)
            })
            .on_drag_move(move |event: &DragMoveEvent<SplitDrag>, window, cx| {
                if event.drag(cx).id != drag_id {
                    return;
                }
                let delta = f32::from(event.event.position.x - event.bounds.center().x);
                on_move(dragged_width(width, delta, side, min, max), window, cx);
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let stroke = &event.keystroke;
                let step = if stroke.modifiers.shift {
                    KEY_STEP_LARGE
                } else {
                    KEY_STEP
                };
                // Arrow keys move the divider; the pane follows.
                let new = match stroke.key.as_str() {
                    "left" => dragged_width(width, -step, side, min, max),
                    "right" => dragged_width(width, step, side, min, max),
                    "home" => clamp_width(min, min, max),
                    "end" => clamp_width(max, min, max),
                    _ => return,
                };
                cx.stop_propagation();
                on_key(new, window, cx);
            })
            .when_some(default_width, |this, default| {
                this.on_click(move |event, window, cx| {
                    if event.click_count() == 2 {
                        on_double(clamp_width(default, min, max), window, cx);
                    }
                })
            })
            .child(
                div()
                    .w(px(1.0))
                    .h_full()
                    .bg(rgb(colors.border))
                    .group_hover("split-handle", move |style| {
                        style.w(px(2.0)).bg(rgb(accent))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, TestAppContext, point,
    };

    use super::*;

    #[test]
    fn widths_are_clamped_to_the_range() {
        assert_eq!(clamp_width(150.0, 200.0, 400.0), 200.0);
        assert_eq!(clamp_width(500.0, 200.0, 400.0), 400.0);
        assert_eq!(clamp_width(300.0, 200.0, 400.0), 300.0);
        // An inverted range pins to the minimum.
        assert_eq!(clamp_width(300.0, 200.0, 100.0), 200.0);
    }

    #[test]
    fn drag_direction_depends_on_the_pane_side() {
        assert_eq!(
            dragged_width(280.0, 20.0, PaneSide::Leading, 200.0, 400.0),
            300.0
        );
        assert_eq!(
            dragged_width(280.0, 20.0, PaneSide::Trailing, 200.0, 400.0),
            260.0
        );
        assert_eq!(
            dragged_width(280.0, -500.0, PaneSide::Leading, 200.0, 400.0),
            200.0
        );
        assert_eq!(
            dragged_width(280.0, 500.0, PaneSide::Leading, 200.0, 400.0),
            400.0
        );
        // Sub-pixel moves round to whole pixels.
        assert_eq!(
            dragged_width(280.0, 0.4, PaneSide::Leading, 0.0, 400.0),
            280.0
        );
    }

    struct Panes {
        width: f32,
        reported: Rc<RefCell<Vec<f32>>>,
    }

    impl Render for Panes {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .size_full()
                .child(div().w(px(self.width)).h_full())
                .child(
                    SplitHandle::new("split", self.width)
                        .range(200.0, 400.0)
                        .default_width(280.0)
                        .on_resize(cx.listener(|panes, width: &f32, _, cx| {
                            panes.width = *width;
                            panes.reported.borrow_mut().push(*width);
                            cx.notify();
                        })),
                )
                .child(div().flex_1().h_full())
        }
    }

    #[gpui::test]
    fn dragging_and_keys_resize_within_the_limits(cx: &mut TestAppContext) {
        let reported = Rc::new(RefCell::new(Vec::new()));
        let (panes, cx) = cx.add_window_view(|_, _| Panes {
            width: 280.0,
            reported: reported.clone(),
        });
        cx.run_until_parked();

        let y = px(100.0);
        let start = point(px(280.0), y);
        let button = Some(MouseButton::Left);
        cx.simulate_event(MouseDownEvent {
            position: start,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        // Far past the maximum: clamped to 400.
        for x in [300.0, 350.0, 900.0] {
            cx.simulate_event(MouseMoveEvent {
                position: point(px(x), y),
                pressed_button: button,
                modifiers: Modifiers::default(),
            });
        }
        assert_eq!(panes.read_with(cx, |panes, _| panes.width), 400.0);
        // Far past the minimum: clamped to 200.
        cx.simulate_event(MouseMoveEvent {
            position: point(px(-50.0), y),
            pressed_button: button,
            modifiers: Modifiers::default(),
        });
        cx.simulate_event(MouseUpEvent {
            position: point(px(-50.0), y),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        assert_eq!(panes.read_with(cx, |panes, _| panes.width), 200.0);
        assert!(
            reported
                .borrow()
                .iter()
                .all(|w| (200.0..=400.0).contains(w))
        );

        // The handle is focusable; arrow keys nudge it.
        cx.update(|window, cx| window.focus_next(cx));
        cx.simulate_keystrokes("right shift-right");
        assert_eq!(
            panes.read_with(cx, |panes, _| panes.width),
            200.0 + KEY_STEP + KEY_STEP_LARGE
        );
        cx.simulate_keystrokes("end");
        assert_eq!(panes.read_with(cx, |panes, _| panes.width), 400.0);
        cx.simulate_keystrokes("home left");
        assert_eq!(panes.read_with(cx, |panes, _| panes.width), 200.0);
    }
}
