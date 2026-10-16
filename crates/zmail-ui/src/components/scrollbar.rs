//! `Scrollbar`: an overlay scrollbar for a scrolling `div` or
//! `uniform_list`, following `PlatformStyle::scrollbar`.
//!
//! - macOS and Linux: drawn over the content, hidden while idle, shown
//!   while scrolling or hovered, and wider while hovered or dragged. On
//!   macOS the system "Show scroll bars: Always" setting keeps it visible.
//! - Windows: always visible as a thin line that widens, with a track,
//!   on hover.
//!
//! Dragging the thumb scrolls; clicking the track jumps there. Scrolling
//! from the keyboard is the scrolled view's job (arrow keys, Page Up/Down).
//!
//! ```text
//! div().relative().size_full()
//!     .child(div().id("body").size_full().overflow_y_scroll().track_scroll(&self.scroll).child(...))
//!     .child(Scrollbar::vertical("body-scrollbar", &self.scroll))
//! ```

use std::{cell::RefCell, rc::Rc, time::Duration};

use gpui::{
    App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, Length, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Position, ScrollHandle, Size, Style, Task, UniformListScrollHandle,
    Window, fill, point, px, rgb, size,
};

use crate::styles::{Platform, ScrollbarBehavior, contrast::mix};

use super::theme::ComponentTheme;

/// How long an auto-hiding scrollbar stays after scrolling stops.
pub(crate) const SCROLLBAR_SHOW_FOR: Duration = Duration::from_millis(1000);
/// Shortest the thumb gets, so it stays easy to grab in long lists.
pub(crate) const MIN_THUMB_LENGTH: f32 = 24.0;
/// Space between the thumb and the edges of the scroll area.
const EDGE_PADDING: f32 = 2.0;

/// Where the thumb sits on a track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ThumbGeometry {
    /// Distance from the top of the track to the top of the thumb.
    pub offset: f32,
    /// Thumb length.
    pub length: f32,
}

/// Thumb position and size for a `viewport`-tall view scrolled `scrolled`
/// pixels down out of `max_scroll`, on a `track`-long track. `None` when
/// nothing scrolls.
pub(crate) fn thumb_geometry(
    track: f32,
    viewport: f32,
    max_scroll: f32,
    scrolled: f32,
) -> Option<ThumbGeometry> {
    if max_scroll <= 0.0 || track <= 0.0 || viewport <= 0.0 {
        return None;
    }
    let content = viewport + max_scroll;
    let length = (track * viewport / content)
        .max(MIN_THUMB_LENGTH)
        .min(track);
    let progress = (scrolled / max_scroll).clamp(0.0, 1.0);
    Some(ThumbGeometry {
        offset: progress * (track - length),
        length,
    })
}

/// The scroll offset that puts the thumb's top at `thumb_offset`.
pub(crate) fn scroll_for_thumb(
    thumb_offset: f32,
    geometry: ThumbGeometry,
    track: f32,
    max_scroll: f32,
) -> f32 {
    let travel = track - geometry.length;
    if travel <= 0.0 {
        return 0.0;
    }
    (thumb_offset / travel).clamp(0.0, 1.0) * max_scroll
}

#[derive(Default)]
struct ScrollbarState {
    last_scrolled: Option<f32>,
    last_activity: Option<std::time::Instant>,
    hovered: bool,
    /// Pointer distance from the thumb's top while dragging it.
    drag_grab: Option<f32>,
    hide_task: Option<Task<()>>,
}

/// A vertical overlay scrollbar. Place it as the last child of a
/// `.relative()` wrapper around the scrolling element.
pub(crate) struct Scrollbar {
    id: ElementId,
    handle: ScrollHandle,
}

impl Scrollbar {
    /// A scrollbar for a `div` tracked with `handle`.
    pub(crate) fn vertical(id: impl Into<ElementId>, handle: &ScrollHandle) -> Self {
        Self {
            id: id.into(),
            handle: handle.clone(),
        }
    }

    /// A scrollbar for a `uniform_list` tracked with `handle`.
    pub(crate) fn for_uniform_list(
        id: impl Into<ElementId>,
        handle: &UniformListScrollHandle,
    ) -> Self {
        let base = handle.0.borrow().base_handle.clone();
        Self {
            id: id.into(),
            handle: base,
        }
    }
}

impl IntoElement for Scrollbar {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// What prepaint hands to paint.
pub(crate) struct ScrollbarPrepaint {
    hitbox: Hitbox,
    state: Rc<RefCell<ScrollbarState>>,
    theme: ComponentTheme,
    auto_hide: bool,
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = ScrollbarPrepaint;

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
        let theme = ComponentTheme::get(cx);
        let mut inset = Style::default().inset;
        inset.top = px(0.0).into();
        inset.bottom = px(0.0).into();
        inset.right = px(0.0).into();
        let style = Style {
            position: Position::Absolute,
            inset,
            size: Size {
                width: px(theme.platform.scrollbar.hover_thickness + EDGE_PADDING * 2.0).into(),
                height: Length::Auto,
            },
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = ComponentTheme::get(cx);
        let auto_hide = theme.platform.scrollbar.behavior == ScrollbarBehavior::OverlayAutoHide
            && (theme.platform.platform != Platform::MacOs || cx.should_auto_hide_scrollbars());
        let state = match id {
            Some(id) => {
                window.with_element_state(id, |state: Option<Rc<RefCell<ScrollbarState>>>, _| {
                    let state = state.unwrap_or_default();
                    (state.clone(), state)
                })
            }
            None => Rc::default(),
        };
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        ScrollbarPrepaint {
            hitbox,
            state,
            theme,
            auto_hide,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let handle = self.handle.clone();
        let viewport = f32::from(handle.bounds().size.height);
        let max_scroll = f32::from(handle.max_offset().y);
        let scrolled = -f32::from(handle.offset().y);
        let track_top = bounds.top() + px(EDGE_PADDING);
        let track = f32::from(bounds.size.height) - EDGE_PADDING * 2.0;
        let Some(geometry) = thumb_geometry(track, viewport, max_scroll, scrolled) else {
            return;
        };

        let theme = prepaint.theme;
        let style = theme.platform.scrollbar;
        let state = prepaint.state.clone();
        let now = cx.background_executor().now();
        let visible = {
            let mut state = state.borrow_mut();
            if state.last_scrolled != Some(scrolled) {
                // The first frame only records the position.
                if state.last_scrolled.is_some() {
                    state.last_activity = Some(now);
                }
                state.last_scrolled = Some(scrolled);
            }
            let active = state.hovered || state.drag_grab.is_some();
            let recent = state
                .last_activity
                .is_some_and(|at| now.saturating_duration_since(at) < SCROLLBAR_SHOW_FOR);
            if prepaint.auto_hide && !active && recent {
                // Repaint when the delay runs out so the thumb disappears.
                let remaining = state
                    .last_activity
                    .map(|at| SCROLLBAR_SHOW_FOR.saturating_sub(now.saturating_duration_since(at)))
                    .unwrap_or_default();
                state.hide_task = Some(window.spawn(cx, async move |cx| {
                    cx.background_executor().timer(remaining).await;
                    cx.update(|window, _| window.refresh()).ok();
                }));
            }
            !prepaint.auto_hide || active || recent
        };

        let (hovered, dragging) = {
            let state = state.borrow();
            (state.hovered, state.drag_grab.is_some())
        };
        let active = hovered || dragging;
        let thickness = if active {
            style.hover_thickness
        } else {
            style.thickness
        };
        let colors = theme.colors;

        if visible {
            if active && style.behavior == ScrollbarBehavior::ThinWidenOnHover {
                let track_bounds = Bounds::new(
                    point(
                        bounds.right() - px(style.hover_thickness + EDGE_PADDING),
                        track_top,
                    ),
                    size(px(style.hover_thickness), px(track)),
                );
                window.paint_quad(
                    fill(
                        track_bounds,
                        rgb(mix(colors.background, colors.foreground, 0.06)),
                    )
                    .corner_radii(px(style.hover_thickness / 2.0)),
                );
            }
            let thumb_color = mix(
                colors.background,
                colors.muted,
                if active { 0.9 } else { 0.6 },
            );
            let thumb = Bounds::new(
                point(
                    bounds.right() - px(thickness + EDGE_PADDING),
                    track_top + px(geometry.offset),
                ),
                size(px(thickness), px(geometry.length)),
            );
            window.paint_quad(fill(thumb, rgb(thumb_color)).corner_radii(px(thickness / 2.0)));
        }

        // Hover tracking and dragging.
        let hitbox = prepaint.hitbox.clone();
        window.on_mouse_event({
            let state = state.clone();
            let handle = handle.clone();
            move |event: &MouseMoveEvent, phase, window, _cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let mut state = state.borrow_mut();
                let hovered = hitbox.is_hovered(window);
                let mut changed = hovered != state.hovered;
                state.hovered = hovered;
                if let Some(grab) = state.drag_grab {
                    if event.pressed_button == Some(MouseButton::Left) {
                        let thumb_offset = f32::from(event.position.y - track_top) - grab;
                        let new = scroll_for_thumb(thumb_offset, geometry, track, max_scroll);
                        handle.set_offset(point(handle.offset().x, px(-new)));
                    } else {
                        state.drag_grab = None;
                    }
                    changed = true;
                }
                if changed {
                    window.refresh();
                }
            }
        });

        let hitbox = prepaint.hitbox.clone();
        window.on_mouse_event({
            let state = state.clone();
            let handle = handle.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !visible
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let along = f32::from(event.position.y - track_top);
                let grab = if (geometry.offset..geometry.offset + geometry.length).contains(&along)
                {
                    along - geometry.offset
                } else {
                    // A click on the track centres the thumb there.
                    let grab = geometry.length / 2.0;
                    let new = scroll_for_thumb(along - grab, geometry, track, max_scroll);
                    handle.set_offset(point(handle.offset().x, px(-new)));
                    grab
                };
                state.borrow_mut().drag_grab = Some(grab);
                cx.stop_propagation();
                window.refresh();
            }
        });

        window.on_mouse_event({
            let state = state.clone();
            move |_: &MouseUpEvent, phase, window, _cx| {
                if phase == DispatchPhase::Bubble && state.borrow_mut().drag_grab.take().is_some() {
                    window.refresh();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_scroll_means_no_thumb() {
        assert_eq!(thumb_geometry(400.0, 400.0, 0.0, 0.0), None);
        assert_eq!(thumb_geometry(0.0, 400.0, 100.0, 0.0), None);
    }

    #[test]
    fn thumb_is_proportional_and_moves_with_the_offset() {
        // Half the content visible: thumb is half the track.
        let top = thumb_geometry(400.0, 400.0, 400.0, 0.0).expect("scrolls");
        assert_eq!(
            top,
            ThumbGeometry {
                offset: 0.0,
                length: 200.0
            }
        );
        let bottom = thumb_geometry(400.0, 400.0, 400.0, 400.0).expect("scrolls");
        assert_eq!(bottom.offset, 200.0);
        let middle = thumb_geometry(400.0, 400.0, 400.0, 200.0).expect("scrolls");
        assert_eq!(middle.offset, 100.0);
        // Overscroll is clamped.
        assert_eq!(
            thumb_geometry(400.0, 400.0, 400.0, 900.0).map(|g| g.offset),
            Some(200.0)
        );
    }

    #[test]
    fn long_content_keeps_a_grabbable_thumb() {
        let geometry = thumb_geometry(400.0, 400.0, 1_000_000.0, 0.0).expect("scrolls");
        assert_eq!(geometry.length, MIN_THUMB_LENGTH);
    }

    #[test]
    fn dragging_the_thumb_maps_back_to_an_offset() {
        let geometry = thumb_geometry(400.0, 400.0, 400.0, 0.0).expect("scrolls");
        assert_eq!(scroll_for_thumb(0.0, geometry, 400.0, 400.0), 0.0);
        assert_eq!(scroll_for_thumb(100.0, geometry, 400.0, 400.0), 200.0);
        assert_eq!(scroll_for_thumb(500.0, geometry, 400.0, 400.0), 400.0);
        assert_eq!(scroll_for_thumb(-20.0, geometry, 400.0, 400.0), 0.0);
    }
}
