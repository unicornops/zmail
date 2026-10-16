//! `SkeletonRow`: a grey placeholder shaped like a message-list row, shown
//! while the first page of a mailbox loads.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, ElementId, IntoElement, RenderOnce, Window, div, prelude::*,
    pulsating_between, px, relative, rgb,
};

use crate::styles::{DensityTokens, RADIUS_FULL, RADIUS_SM, SPACE_1, blend_over};

use super::theme::ComponentTheme;

/// A placeholder row with an avatar circle and text bars.
#[derive(IntoElement)]
pub(crate) struct SkeletonRow {
    id: ElementId,
    density: DensityTokens,
    animated: bool,
}

impl SkeletonRow {
    /// A comfortable-density placeholder row.
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            density: DensityTokens::COMFORTABLE,
            animated: true,
        }
    }

    /// Match the row layout of `density`.
    pub(crate) fn density(mut self, density: DensityTokens) -> Self {
        self.density = density;
        self
    }

    /// Turn the pulse off (reduced motion, screenshots).
    pub(crate) fn animated(mut self, animated: bool) -> Self {
        self.animated = animated;
        self
    }
}

impl RenderOnce for SkeletonRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let bar_color = blend_over(colors.foreground, 0x1F, colors.background);
        let density = self.density;
        let line_height = density.message_row_line_height;
        let bar = move |width: f32| {
            div()
                .h(px(line_height * 0.6))
                .w(relative(width))
                .rounded(px(RADIUS_SM))
                .bg(rgb(bar_color))
        };
        let lines: Vec<f32> = [0.55, 0.85, 0.7]
            .into_iter()
            .take(usize::from(density.message_row_lines).max(1))
            .collect();

        let row = div()
            .flex()
            .items_center()
            .gap(px(density.row_gap))
            .w_full()
            .h(px(density.message_row_height))
            .px(px(density.message_row_padding_x))
            .when(density.show_avatar, |this| {
                this.child(
                    div()
                        .flex_none()
                        .size(px(density.avatar_size))
                        .rounded(px(RADIUS_FULL))
                        .bg(rgb(bar_color)),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .justify_center()
                    .gap(px(density.message_row_line_gap.max(SPACE_1)))
                    .children(lines.into_iter().map(bar)),
            );

        if self.animated {
            row.with_animation(
                self.id,
                Animation::new(Duration::from_millis(1200))
                    .repeat()
                    .with_easing(pulsating_between(0.45, 1.0)),
                |row, delta| row.opacity(delta),
            )
            .into_any_element()
        } else {
            row.into_any_element()
        }
    }
}
