//! `Badge`: a small count or label pill, or a dot.

use gpui::{
    App, FontWeight, IntoElement, RenderOnce, SharedString, Window, div, prelude::*, px, rgb,
};

use crate::styles::{BADGE_SIZE, RADIUS_FULL, TYPE_CAPTION, TypeStyleExt, UNREAD_INDICATOR_SIZE};

use super::theme::ComponentTheme;

/// The colour of a badge.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BadgeTone {
    /// Accent fill: unread counts.
    #[default]
    Accent,
    /// Neutral fill: totals and secondary counts.
    Neutral,
    /// Danger fill: errors and failed items.
    Danger,
    /// Success fill.
    Success,
    /// Warning fill.
    Warning,
}

/// The most a count badge shows before switching to "99+".
pub(crate) const BADGE_MAX_COUNT: usize = 99;

/// A pill with a count or short label, or a dot with no text.
#[derive(IntoElement)]
pub(crate) struct Badge {
    label: Option<SharedString>,
    tone: BadgeTone,
}

impl Badge {
    /// A badge showing `count`, capped at "99+".
    pub(crate) fn count(count: usize) -> Self {
        Self::label(count_label(count))
    }

    /// A badge showing a short `label`.
    pub(crate) fn label(label: impl Into<SharedString>) -> Self {
        Self {
            label: Some(label.into()),
            tone: BadgeTone::default(),
        }
    }

    /// An 8px dot with no text (an unread indicator).
    pub(crate) fn dot() -> Self {
        Self {
            label: None,
            tone: BadgeTone::default(),
        }
    }

    /// Set the colour.
    pub(crate) fn tone(mut self, tone: BadgeTone) -> Self {
        self.tone = tone;
        self
    }
}

/// The text of a count badge.
pub(crate) fn count_label(count: usize) -> String {
    if count > BADGE_MAX_COUNT {
        format!("{BADGE_MAX_COUNT}+")
    } else {
        count.to_string()
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let fill = match self.tone {
            BadgeTone::Accent => colors.accent,
            BadgeTone::Neutral => colors.button_background,
            BadgeTone::Danger => colors.danger,
            BadgeTone::Success => colors.success,
            BadgeTone::Warning => colors.warning,
        };
        match self.label {
            None => div()
                .flex_none()
                .size(px(UNREAD_INDICATOR_SIZE))
                .rounded(px(RADIUS_FULL))
                .bg(rgb(fill)),
            Some(label) => div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .min_w(px(BADGE_SIZE))
                .h(px(BADGE_SIZE))
                .px(px(6.0))
                .rounded(px(RADIUS_FULL))
                .bg(rgb(fill))
                .type_style(TYPE_CAPTION)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.text_on(fill)))
                .whitespace_nowrap()
                .child(label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_counts_are_capped() {
        assert_eq!(count_label(0), "0");
        assert_eq!(count_label(99), "99");
        assert_eq!(count_label(100), "99+");
        assert_eq!(count_label(12_345), "99+");
    }
}
