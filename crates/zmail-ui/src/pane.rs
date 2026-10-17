//! Shared pieces of the workspace panes.

use gpui::{Action, App, ClickEvent, Div, FocusHandle, Styled, Window, rgb};

use crate::styles::UiColors;

/// The panes Tab and F6 move between, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    Sidebar,
    MessageList,
    /// Whichever of the reader, compose and account setup is showing.
    Detail,
}

impl Pane {
    /// Every pane, in focus order.
    pub(crate) const ALL: [Pane; 3] = [Pane::Sidebar, Pane::MessageList, Pane::Detail];

    /// The pane after (or, with `forward` false, before) this one,
    /// wrapping around.
    pub(crate) fn step(self, forward: bool) -> Pane {
        let index = Self::ALL
            .iter()
            .position(|pane| *pane == self)
            .unwrap_or_default();
        let count = Self::ALL.len();
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        Self::ALL[next]
    }
}

/// Marks a pane's header while the pane has keyboard focus: its bottom
/// border takes the focus-ring colour (contrast-checked at 3:1). The colour
/// changes in place, so focus never shifts the layout. (A full focus ring
/// around the pane is part of the component work.)
pub(crate) fn pane_focus_indicator(header: Div, focused: bool, colors: &UiColors) -> Div {
    if focused {
        header.border_color(rgb(colors.focus_ring))
    } else {
        header
    }
}

/// A click handler that dispatches `action` as if from `focus_handle`: the
/// action bubbles from that pane up to the workspace, exactly like the key
/// binding does, even when keyboard focus is somewhere else.
pub(crate) fn dispatch_from(
    focus_handle: &FocusHandle,
    action: impl Action,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let focus_handle = focus_handle.clone();
    move |_, window, cx| focus_handle.dispatch_action(&action, window, cx)
}

/// How often a pane rendered and queried the store, for the tests that
/// check a pane re-renders on its own.
#[cfg(test)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderCounters {
    pub(crate) renders: usize,
    pub(crate) queries: usize,
}

#[cfg(test)]
mod tests {
    use super::Pane;

    #[test]
    fn panes_cycle_both_ways() {
        assert_eq!(Pane::Sidebar.step(true), Pane::MessageList);
        assert_eq!(Pane::MessageList.step(true), Pane::Detail);
        assert_eq!(Pane::Detail.step(true), Pane::Sidebar);
        assert_eq!(Pane::Sidebar.step(false), Pane::Detail);
        assert_eq!(Pane::Detail.step(false), Pane::MessageList);
    }
}
