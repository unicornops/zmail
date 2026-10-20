//! `window_frame`: the window frame Zmail draws itself when the window
//! system asks for client-side decorations (GNOME on Wayland, X11 with a
//! compositor): a shadow, a 1px border, rounded top corners and resize
//! edges. With server-side decorations (macOS, Windows, KDE, X11 without a
//! compositor) it adds nothing.
//!
//! Ported from `client_side_decorations` in Zed's `workspace` crate and
//! GPUI's `examples/window_shadow.rs` (Zed v1.24.2,
//! <https://github.com/zed-industries/zed>, Copyright Zed Industries, Inc.;
//! `workspace` is GPL-3.0-or-later, used here under GPL-3.0-only, and GPUI
//! is Apache-2.0).
//!
//! The shadow lives in a transparent band around the window
//! (`set_client_inset` tells the compositor how wide it is, so it is not
//! counted as the window's content). The band doubles as the resize
//! handle: pressing in it starts `start_window_resize` on that edge or
//! corner. Tiled edges (a maximised window, or one snapped to a screen
//! edge) get neither shadow, band, border nor rounding.
//!
//! Only the top corners are rounded, matching the title bar. GPUI cannot
//! clip children to a rounded rectangle and the panes paint right down to
//! the window's bottom edge, so rounded bottom corners would show the
//! panes' square corners poking out (GTK 3 and KDE's Breeze draw windows
//! the same way).
//!
//! Every root view calls it, so it also sets the window's rem size from
//! the text size ([`TextScale`]).

use gpui::{
    App, Bounds, CursorStyle, Div, Global, HitboxBehavior, Hsla, IntoElement, MouseButton, Pixels,
    Point, ResizeEdge, Size, Stateful, Tiling, Window, canvas, div, point, prelude::*, px, rgb,
    size, transparent_black,
};

use super::theme::ComponentTheme;
use super::title_bar::WindowChrome;
use crate::styles::TextScale;

/// Width of the transparent band around a client-decorated window that
/// holds its shadow and resize edges.
pub(crate) const FRAME_SHADOW: f32 = 10.0;

/// Width of the frame's border.
pub(crate) const FRAME_BORDER: f32 = 1.0;

/// The resize edge under the pointer, remembered so the window only
/// redraws (to change the cursor) when it changes.
struct HoveredResizeEdge(ResizeEdge);

impl Global for HoveredResizeEdge {}

/// Wrap the window's root element in the client-side decoration frame
/// when the window needs one, and size the window's text. Call from the
/// root view's `render`.
pub(crate) fn window_frame(
    element: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    // Layout runs after `render`, so the whole frame lays out at it.
    window.set_rem_size(TextScale::get(cx).rem_size());
    let chrome = WindowChrome::of(window, cx);
    let theme = ComponentTheme::get(cx);
    let tiling = chrome.tiling();
    let decorated = chrome.client_decorated();
    let resizable = chrome.resizable;
    let shadow = px(FRAME_SHADOW);
    let radius = px(theme.platform.window_radius_for(chrome.maximized));

    window.set_client_inset(if decorated { shadow } else { px(0.0) });

    div()
        .id("window-frame")
        .size_full()
        .bg(transparent_black())
        .when(decorated, |this| {
            this.when(!tiling.top, |this| this.pt(shadow))
                .when(!tiling.bottom, |this| this.pb(shadow))
                .when(!tiling.left, |this| this.pl(shadow))
                .when(!tiling.right, |this| this.pr(shadow))
                .when(resizable, |this| {
                    this.on_mouse_move(move |event, window, cx| {
                        let edge = resize_edge(event.position, shadow, window_size(window), tiling);
                        let previous = cx.try_global::<HoveredResizeEdge>().map(|edge| edge.0);
                        if edge != previous {
                            window.refresh();
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        move |event, window, _| {
                            if let Some(edge) =
                                resize_edge(event.position, shadow, window_size(window), tiling)
                            {
                                window.start_window_resize(edge);
                            }
                        },
                    )
                })
        })
        .child(
            div()
                .size_full()
                .cursor(CursorStyle::Arrow)
                // The window background, here rather than on the root view
                // so it follows the frame's rounded corners.
                .bg(rgb(theme.colors.background))
                .when(decorated, |this| {
                    this.border_color(rgb(theme.colors.border))
                        .when(!(tiling.top || tiling.left), |this| this.rounded_tl(radius))
                        .when(!(tiling.top || tiling.right), |this| {
                            this.rounded_tr(radius)
                        })
                        .when(!tiling.top, |this| this.border_t(px(FRAME_BORDER)))
                        .when(!tiling.bottom, |this| this.border_b(px(FRAME_BORDER)))
                        .when(!tiling.left, |this| this.border_l(px(FRAME_BORDER)))
                        .when(!tiling.right, |this| this.border_r(px(FRAME_BORDER)))
                        .when(!tiling.is_tiled(), |this| {
                            this.shadow(vec![
                                gpui::BoxShadow::new(
                                    px(0.0),
                                    px(0.0),
                                    Hsla {
                                        h: 0.0,
                                        s: 0.0,
                                        l: 0.0,
                                        a: 0.4,
                                    },
                                )
                                .blur_radius(shadow / 2.0),
                            ])
                        })
                })
                // Moving over the window's content is not moving over a
                // resize edge.
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .child(element),
        )
        .when(decorated && resizable, |this| {
            // The resize cursor, drawn over everything in the band.
            this.child(
                canvas(
                    |_, window, _| {
                        window.insert_hitbox(
                            Bounds::new(point(px(0.0), px(0.0)), window_size(window)),
                            HitboxBehavior::Normal,
                        )
                    },
                    move |_, hitbox, window, cx| {
                        let Some(edge) = resize_edge(
                            window.mouse_position(),
                            shadow,
                            window_size(window),
                            tiling,
                        ) else {
                            return;
                        };
                        cx.set_global(HoveredResizeEdge(edge));
                        window.set_cursor_style(resize_cursor(edge), &hitbox);
                    },
                )
                .size_full()
                .absolute(),
            )
        })
}

fn window_size(window: &Window) -> Size<Pixels> {
    window.window_bounds().get_bounds().size
}

fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// The edge or corner at `position` (window coordinates), if it is in the
/// resize band of an untiled edge. Corners are 1.5 bands square, so they
/// are easy to hit.
pub(crate) fn resize_edge(
    position: Point<Pixels>,
    band: Pixels,
    window: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let inner = Bounds::new(Point::default(), window).inset(band * 1.5);
    if inner.contains(&position) {
        return None;
    }

    let corner = size(band * 1.5, band * 1.5);
    let corners = [
        (
            Point::new(px(0.0), px(0.0)),
            !tiling.top,
            ResizeEdge::TopLeft,
        ),
        (
            Point::new(window.width - corner.width, px(0.0)),
            !tiling.top,
            ResizeEdge::TopRight,
        ),
        (
            Point::new(px(0.0), window.height - corner.height),
            !tiling.bottom,
            ResizeEdge::BottomLeft,
        ),
        (
            Point::new(window.width - corner.width, window.height - corner.height),
            !tiling.bottom,
            ResizeEdge::BottomRight,
        ),
    ];
    for (origin, allowed, edge) in corners {
        if allowed && Bounds::new(origin, corner).contains(&position) {
            return Some(edge);
        }
    }

    if !tiling.top && position.y < band {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && position.y > window.height - band {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && position.x < band {
        Some(ResizeEdge::Left)
    } else if !tiling.right && position.x > window.width - band {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Size<Pixels> = Size {
        width: px(800.0),
        height: px(600.0),
    };
    const BAND: Pixels = px(FRAME_SHADOW);

    fn at(x: f32, y: f32, tiling: Tiling) -> Option<ResizeEdge> {
        resize_edge(point(px(x), px(y)), BAND, WINDOW, tiling)
    }

    #[test]
    fn the_band_resizes_from_edges_and_corners() {
        let free = Tiling::default();
        assert_eq!(at(400.0, 300.0, free), None, "the middle");
        assert_eq!(at(400.0, 30.0, free), None, "inside the content");
        assert_eq!(at(400.0, 4.0, free), Some(ResizeEdge::Top));
        assert_eq!(at(400.0, 596.0, free), Some(ResizeEdge::Bottom));
        assert_eq!(at(4.0, 300.0, free), Some(ResizeEdge::Left));
        assert_eq!(at(796.0, 300.0, free), Some(ResizeEdge::Right));
        assert_eq!(at(2.0, 2.0, free), Some(ResizeEdge::TopLeft));
        assert_eq!(at(798.0, 2.0, free), Some(ResizeEdge::TopRight));
        assert_eq!(at(2.0, 598.0, free), Some(ResizeEdge::BottomLeft));
        assert_eq!(at(798.0, 598.0, free), Some(ResizeEdge::BottomRight));
    }

    #[test]
    fn tiled_edges_do_not_resize() {
        let tiled = Tiling::tiled();
        for (x, y) in [(400.0, 4.0), (4.0, 300.0), (2.0, 2.0), (798.0, 598.0)] {
            assert_eq!(at(x, y, tiled), None, "({x}, {y})");
        }
        // Snapped to the left half: the left, top and bottom edges are
        // against the screen; only the right edge resizes.
        let left_half = Tiling {
            top: true,
            bottom: true,
            left: true,
            right: false,
        };
        assert_eq!(at(4.0, 300.0, left_half), None);
        assert_eq!(at(796.0, 300.0, left_half), Some(ResizeEdge::Right));
        assert_eq!(at(798.0, 2.0, left_half), Some(ResizeEdge::Right));
    }
}
