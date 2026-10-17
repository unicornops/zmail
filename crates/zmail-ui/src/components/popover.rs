//! `Popover`: a floating card, and the helpers that place one above the
//! rest of the window.
//!
//! GPUI draws `deferred` elements after everything else, and `anchored`
//! keeps them inside the window, so a popover never gets clipped by the
//! pane it was opened from.
//!
//! ```text
//! // Below a trigger (the trigger's wrapper must be `.relative()`):
//! div().relative().child(trigger).when(open, |this| this.child(popover_below(content)))
//! // At the pointer, for context menus:
//! overlay_at(event.position, menu.clone())
//! ```

use gpui::{
    Anchor, AnyElement, App, IntoElement, ParentElement, Pixels, Point, RenderOnce, Window,
    anchored, deferred, div, prelude::*, px, rgb,
};

use crate::styles::{RADIUS_LG, SPACE_1, SPACE_2};

use super::theme::ComponentTheme;

/// Distance kept between a popover and the window edges.
const WINDOW_MARGIN: f32 = SPACE_2;

/// A floating card for menus, pickers and other transient content.
#[derive(IntoElement)]
pub(crate) struct Popover {
    children: Vec<AnyElement>,
    min_width: Option<Pixels>,
}

impl Popover {
    /// An empty popover card.
    pub(crate) fn new() -> Self {
        Self {
            children: Vec::new(),
            min_width: None,
        }
    }

    /// Set the minimum width.
    pub(crate) fn min_width(mut self, width: Pixels) -> Self {
        self.min_width = Some(width);
        self
    }
}

impl ParentElement for Popover {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Popover {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        div()
            .flex()
            .flex_col()
            .p(px(SPACE_1))
            .when_some(self.min_width, |this, width| this.min_w(width))
            .rounded(px(RADIUS_LG))
            .bg(rgb(colors.surface))
            .border_1()
            .border_color(rgb(colors.border))
            .shadow_lg()
            .text_color(rgb(colors.foreground))
            // Clicks and hovers stop here instead of reaching the pane
            // underneath.
            .occlude()
            .children(self.children)
    }
}

/// Float `content` below the top-left corner of the nearest `.relative()`
/// ancestor's bottom edge (the trigger), flipping or shifting to stay in
/// the window.
pub(crate) fn popover_below(content: impl IntoElement) -> impl IntoElement {
    div().absolute().top_full().left_0().child(
        deferred(
            anchored()
                .anchor(Anchor::TopLeft)
                .snap_to_window_with_margin(px(WINDOW_MARGIN))
                .child(div().mt(px(SPACE_1)).child(content)),
        )
        .with_priority(1),
    )
}

/// Like [`popover_below`], but aligned to the trigger's trailing edge, for
/// triggers at the end of a bar.
pub(crate) fn popover_below_end(content: impl IntoElement) -> impl IntoElement {
    div().absolute().top_full().right_0().child(
        deferred(
            anchored()
                .anchor(Anchor::TopRight)
                .snap_to_window_with_margin(px(WINDOW_MARGIN))
                .child(div().mt(px(SPACE_1)).child(content)),
        )
        .with_priority(1),
    )
}

/// Float `content` with its top-left corner at `position` (window
/// coordinates), shifted to stay in the window. Used for context menus.
pub(crate) fn overlay_at(position: Point<Pixels>, content: impl IntoElement) -> impl IntoElement {
    deferred(
        anchored()
            .position(position)
            .anchor(Anchor::TopLeft)
            .snap_to_window_with_margin(px(WINDOW_MARGIN))
            .child(content),
    )
    .with_priority(1)
}
