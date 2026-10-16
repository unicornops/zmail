//! `Tooltip`: a short label shown after hovering a control, optionally with
//! the keyboard shortcut for the same command.

use std::time::Duration;

use gpui::{
    AnyView, App, Context, IntoElement, Render, SharedString, StatefulInteractiveElement, Window,
    div, prelude::*, px, rgb,
};

use crate::styles::{RADIUS_MD, TYPE_BODY, TypeStyleExt};

use super::kbd::Kbd;
use super::theme::ComponentTheme;

/// How long the pointer rests on a control before its tooltip appears.
pub(crate) const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

/// A tooltip view: a label and an optional shortcut key cap.
pub(crate) struct Tooltip {
    label: SharedString,
    shortcut: Option<SharedString>,
    theme: ComponentTheme,
}

impl Tooltip {
    /// A tooltip builder for `.tooltip(...)` that shows `label`.
    pub(crate) fn text(
        label: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Self::build(label.into(), None)
    }

    /// A tooltip builder that shows `label` and the `shortcut` key cap
    /// (GPUI binding syntax, e.g. `"secondary-r"`).
    pub(crate) fn with_shortcut(
        label: impl Into<SharedString>,
        shortcut: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Self::build(label.into(), Some(shortcut.into()))
    }

    fn build(
        label: SharedString,
        shortcut: Option<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        move |_window, cx| {
            let theme = ComponentTheme::get(cx);
            let label = label.clone();
            let shortcut = shortcut.clone();
            cx.new(|_| Tooltip {
                label,
                shortcut,
                theme,
            })
            .into()
        }
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(px(RADIUS_MD))
            .bg(rgb(colors.surface))
            .border_1()
            .border_color(rgb(colors.border))
            .shadow_md()
            .type_style(TYPE_BODY)
            .text_color(rgb(colors.foreground))
            .whitespace_nowrap()
            .child(self.label.clone())
            .when_some(self.shortcut.clone(), |this, shortcut| {
                this.child(Kbd::new(shortcut).platform(self.theme.platform.platform))
            })
    }
}

/// Builds a tooltip view when the pointer has rested long enough.
type TooltipBuilder = dyn Fn(&mut Window, &mut App) -> AnyView;

/// Attach a delayed [`Tooltip`] to any stateful element.
pub(crate) trait TooltipExt: StatefulInteractiveElement + Sized {
    /// Show `label` (and `shortcut`, if any) after [`TOOLTIP_DELAY`].
    fn with_tooltip(self, label: impl Into<SharedString>, shortcut: Option<SharedString>) -> Self {
        let label = label.into();
        let build: Box<TooltipBuilder> = match shortcut {
            Some(shortcut) => Box::new(Tooltip::with_shortcut(label, shortcut)),
            None => Box::new(Tooltip::text(label)),
        };
        self.tooltip(build).tooltip_show_delay(TOOLTIP_DELAY)
    }
}

impl<T: StatefulInteractiveElement> TooltipExt for T {}
