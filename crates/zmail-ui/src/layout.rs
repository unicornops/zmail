//! The responsive pane layout: which panes show at a window width, and
//! how wide each one is.
//!
//! | Window width | Layout |
//! |---|---|
//! | 1100px and up | sidebar, message list and detail side by side ([`LayoutMode::Wide`]) |
//! | 760–1099px | list and detail; the sidebar opens over them ([`LayoutMode::Overlay`]) |
//! | below 760px | one pane at a time, with a back button ([`LayoutMode::Single`]) |
//!
//! With the reading pane turned off (Settings → Appearance), the list and
//! the detail pane take turns in the space beside the sidebar at every
//! width, as they do in the single-pane layout
//! ([`PaneLayout::without_reading_pane`]).
//!
//! The user's sidebar and list widths (dragged with the split handles and
//! saved in the UI preferences) are clamped to what the window can fit
//! while the detail pane keeps [`DETAIL_MIN_WIDTH`]. The saved widths
//! themselves are kept, so a pane gets its width back when the window
//! grows again.

use crate::styles::{
    DETAIL_MIN_WIDTH, MESSAGE_LIST_MAX_WIDTH, MESSAGE_LIST_MIN_WIDTH, SIDEBAR_MAX_WIDTH,
    SIDEBAR_MIN_WIDTH,
};

/// Below this window width the sidebar stops being a pane and opens as an
/// overlay instead.
pub(crate) const SIDEBAR_OVERLAY_BELOW: f32 = 1100.0;
/// Below this window width the window shows one pane at a time.
pub(crate) const SINGLE_PANE_BELOW: f32 = 760.0;
/// Room the sidebar overlay leaves uncovered at the right edge, so the
/// list behind it stays visible (and clickable to close it).
const OVERLAY_RIGHT_GAP: f32 = 48.0;

/// How the panes are arranged at a window width. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutMode {
    /// Three panes side by side.
    Wide,
    /// List and detail; the sidebar is an overlay.
    Overlay,
    /// One pane at a time.
    Single,
}

impl LayoutMode {
    /// The layout for a window `width` logical pixels wide.
    pub(crate) fn for_width(width: f32) -> Self {
        if width < SINGLE_PANE_BELOW {
            Self::Single
        } else if width < SIDEBAR_OVERLAY_BELOW {
            Self::Overlay
        } else {
            Self::Wide
        }
    }
}

/// Which pane a stacked layout ([`PaneLayout::stacked`]) shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SinglePane {
    /// The message list.
    #[default]
    List,
    /// The reader, compose or account setup, under a back button.
    Detail,
}

/// The pane widths for one window width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PaneLayout {
    pub(crate) mode: LayoutMode,
    /// The sidebar's width when it is a pane, `None` when it is hidden or
    /// an overlay.
    pub(crate) sidebar: Option<f32>,
    /// The widest the sidebar may be dragged.
    pub(crate) sidebar_max: f32,
    /// The sidebar's width as an overlay.
    pub(crate) overlay_sidebar: f32,
    /// The message list's width (the whole window in single-pane mode).
    pub(crate) list: f32,
    /// The widest the list may be dragged.
    pub(crate) list_max: f32,
    /// What is left for the detail pane.
    pub(crate) detail: f32,
    /// The list and the detail pane share one space and take turns: the
    /// single-pane layout, or any layout without a reading pane.
    pub(crate) stacked: bool,
}

impl PaneLayout {
    /// Lays out a window `viewport` pixels wide with the user's preferred
    /// `sidebar` and `list` widths. `sidebar_hidden` is the user's choice
    /// to hide the sidebar pane in the wide layout.
    pub(crate) fn compute(viewport: f32, sidebar: f32, list: f32, sidebar_hidden: bool) -> Self {
        let mode = LayoutMode::for_width(viewport);
        let overlay_sidebar = clamp(sidebar, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
            .min(viewport - OVERLAY_RIGHT_GAP)
            .max(0.0);
        if mode == LayoutMode::Single {
            return Self {
                mode,
                sidebar: None,
                sidebar_max: SIDEBAR_MAX_WIDTH,
                overlay_sidebar,
                list: viewport,
                list_max: MESSAGE_LIST_MAX_WIDTH,
                detail: viewport,
                stacked: true,
            };
        }

        let sidebar_max = clamp(
            viewport - MESSAGE_LIST_MIN_WIDTH - DETAIL_MIN_WIDTH,
            SIDEBAR_MIN_WIDTH,
            SIDEBAR_MAX_WIDTH,
        );
        let sidebar = (mode == LayoutMode::Wide && !sidebar_hidden)
            .then(|| clamp(sidebar, SIDEBAR_MIN_WIDTH, sidebar_max));
        let rest = viewport - sidebar.unwrap_or(0.0);
        let list_max = clamp(
            rest - DETAIL_MIN_WIDTH,
            MESSAGE_LIST_MIN_WIDTH,
            MESSAGE_LIST_MAX_WIDTH,
        );
        let list = clamp(list, MESSAGE_LIST_MIN_WIDTH, list_max);
        Self {
            mode,
            sidebar,
            sidebar_max,
            overlay_sidebar,
            list,
            list_max,
            detail: (rest - list).max(0.0),
            stacked: false,
        }
    }

    /// This layout with no reading pane: the list and the detail pane
    /// each get all the room beside the sidebar, one at a time.
    pub(crate) fn without_reading_pane(mut self) -> Self {
        let content = self.list + self.detail;
        if !self.stacked {
            self.list = content;
            self.detail = content;
            self.stacked = true;
        }
        self
    }
}

/// `value` within `min..=max`; a `max` below `min` gives `min`.
fn clamp(value: f32, min: f32, max: f32) -> f32 {
    value.min(max).max(min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styles::{MESSAGE_LIST_WIDTH, SIDEBAR_WIDTH};

    fn layout(viewport: f32) -> PaneLayout {
        PaneLayout::compute(viewport, SIDEBAR_WIDTH, MESSAGE_LIST_WIDTH, false)
    }

    #[test]
    fn breakpoints_pick_the_layout() {
        assert_eq!(LayoutMode::for_width(720.0), LayoutMode::Single);
        assert_eq!(LayoutMode::for_width(759.0), LayoutMode::Single);
        assert_eq!(LayoutMode::for_width(760.0), LayoutMode::Overlay);
        assert_eq!(LayoutMode::for_width(1099.0), LayoutMode::Overlay);
        assert_eq!(LayoutMode::for_width(1100.0), LayoutMode::Wide);
        assert_eq!(LayoutMode::for_width(1920.0), LayoutMode::Wide);
    }

    #[test]
    fn the_default_widths_fit_every_layout() {
        let wide = layout(1440.0);
        assert_eq!(wide.sidebar, Some(SIDEBAR_WIDTH));
        assert_eq!(wide.list, MESSAGE_LIST_WIDTH);
        assert_eq!(wide.detail, 1440.0 - SIDEBAR_WIDTH - MESSAGE_LIST_WIDTH);

        let overlay = layout(900.0);
        assert_eq!(overlay.sidebar, None);
        assert_eq!(overlay.overlay_sidebar, SIDEBAR_WIDTH);
        assert_eq!(overlay.detail, 900.0 - MESSAGE_LIST_WIDTH);

        let single = layout(720.0);
        assert_eq!(
            (single.sidebar, single.list, single.detail),
            (None, 720.0, 720.0)
        );

        for viewport in [760.0, 900.0, 1100.0, 1440.0, 1920.0] {
            assert!(layout(viewport).detail >= DETAIL_MIN_WIDTH, "{viewport}");
        }
    }

    #[test]
    fn saved_widths_are_clamped_to_the_window_but_kept() {
        // Wide panes in a window that only just fits three.
        let tight = PaneLayout::compute(1100.0, 420.0, 640.0, false);
        assert_eq!(tight.sidebar, Some(420.0));
        assert_eq!(tight.list, 1100.0 - 420.0 - DETAIL_MIN_WIDTH);
        assert_eq!(tight.list, tight.list_max);
        assert_eq!(tight.detail, DETAIL_MIN_WIDTH);

        // The same preferences in a big window get their full width.
        let roomy = PaneLayout::compute(1920.0, 420.0, 640.0, false);
        assert_eq!((roomy.sidebar, roomy.list), (Some(420.0), 640.0));

        // Out-of-range preferences are pinned to the limits.
        let silly = PaneLayout::compute(1440.0, 5.0, 5000.0, false);
        assert_eq!(silly.sidebar, Some(SIDEBAR_MIN_WIDTH));
        assert_eq!(silly.list, MESSAGE_LIST_MAX_WIDTH);
    }

    #[test]
    fn without_a_reading_pane_list_and_detail_share_the_room() {
        let off = layout(1440.0).without_reading_pane();
        assert!(off.stacked);
        assert_eq!(off.sidebar, Some(SIDEBAR_WIDTH));
        assert_eq!(
            (off.list, off.detail),
            (1440.0 - SIDEBAR_WIDTH, 1440.0 - SIDEBAR_WIDTH)
        );

        let overlay = layout(900.0).without_reading_pane();
        assert_eq!(
            (overlay.sidebar, overlay.list, overlay.detail),
            (None, 900.0, 900.0)
        );

        assert_eq!(layout(720.0).without_reading_pane(), layout(720.0));
        assert!(!layout(1440.0).stacked);
    }

    #[test]
    fn a_hidden_sidebar_gives_its_width_to_the_detail_pane() {
        let hidden = PaneLayout::compute(1440.0, SIDEBAR_WIDTH, MESSAGE_LIST_WIDTH, true);
        assert_eq!(hidden.sidebar, None);
        assert_eq!(hidden.detail, 1440.0 - MESSAGE_LIST_WIDTH);
    }
}
