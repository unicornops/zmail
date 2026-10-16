//! A minimal text tooltip.
//!
//! GPUI shows tooltips as views, so this wraps a string in a small entity.
//! The full tooltip component (with key bindings and delays) is part of
//! the Phase 1 component set in the UI plan.

use gpui::{
    AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, rgb,
};

use crate::styles::UiColors;

/// A single-line tooltip label.
pub(crate) struct TextTooltip {
    text: SharedString,
    colors: UiColors,
}

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(self.colors.surface))
            .border_1()
            .border_color(rgb(self.colors.border))
            .text_sm()
            .text_color(rgb(self.colors.foreground))
            .whitespace_nowrap()
            .child(self.text.clone())
    }
}

/// Build a tooltip callback for `.tooltip(...)` that shows `text`.
pub(crate) fn text_tooltip(
    text: impl Into<SharedString>,
    colors: UiColors,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_window, cx| {
        cx.new(|_| TextTooltip {
            text: text.clone(),
            colors,
        })
        .into()
    }
}
