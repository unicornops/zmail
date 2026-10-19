//! `Banner`: an inline notice with a tone, an icon, an optional title, one
//! plain sentence, actions and optional details.
//!
//! The tones follow plan §2 ("short, tiered banners with one plain
//! sentence"): `Neutral` is a quiet one-line notice, and `Info`, `Caution`
//! and `Danger` are the sender-safety tiers. Every tone has its own icon
//! and the tiers carry a title, so the tier never relies on colour alone;
//! `Danger` also draws a full border in the danger colour.
//!
//! When a banner has details, it adds a "Show details" toggle. Whether the
//! details are open is element state keyed by the banner's id, so the view
//! that draws the banner keeps no state for it.

use gpui::{
    AnyElement, App, ElementId, FontWeight, IntoElement, ParentElement, RenderOnce, Role,
    SharedString, Window, div, prelude::*, px, rgb,
};

use crate::styles::{
    ICON_SIZE_MD, ICON_SIZE_SM, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, STATUS_BANNER_STRIP_WIDTH,
    TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TypeStyleExt, UiColors,
};
use crate::widgets::icon;

use super::button::{Button, ButtonSize};
use super::theme::ComponentTheme;

/// How much a banner asks for attention.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BannerTone {
    /// A quiet notice, such as blocked remote content.
    #[default]
    Neutral,
    /// Worth knowing; rarely a problem.
    Info,
    /// Check before acting.
    Caution,
    /// Likely harmful.
    Danger,
}

impl BannerTone {
    /// The tone's icon. Each tone has a different shape: shield, circle,
    /// triangle and octagon.
    pub(crate) fn default_icon(self) -> &'static str {
        match self {
            Self::Neutral => "shield",
            Self::Info => "info",
            Self::Caution => "triangle-alert",
            Self::Danger => "octagon-alert",
        }
    }

    /// The banner fill and the colour of its icon, title and strip.
    pub(crate) fn colors(self, colors: &UiColors) -> (u32, u32) {
        match self {
            Self::Neutral => (colors.surface, colors.muted),
            Self::Info => (colors.info_background, colors.info),
            Self::Caution => (colors.warning_background, colors.warning),
            Self::Danger => (colors.danger_background, colors.danger),
        }
    }
}

/// An inline notice. See the module documentation.
#[derive(IntoElement)]
pub(crate) struct Banner {
    id: ElementId,
    tone: BannerTone,
    title: Option<SharedString>,
    message: SharedString,
    actions: Vec<AnyElement>,
    details: Vec<(SharedString, SharedString)>,
}

impl Banner {
    /// A neutral banner saying `message`.
    pub(crate) fn new(id: impl Into<ElementId>, message: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            tone: BannerTone::default(),
            title: None,
            message: message.into(),
            actions: Vec::new(),
            details: Vec::new(),
        }
    }

    /// Set the tone.
    pub(crate) fn tone(mut self, tone: BannerTone) -> Self {
        self.tone = tone;
        self
    }

    /// A short bold title above the sentence.
    pub(crate) fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Add a button (or other element) to the action row.
    pub(crate) fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    /// Add one "label: value" line of evidence, shown under "Show
    /// details".
    pub(crate) fn detail(
        mut self,
        label: impl Into<SharedString>,
        value: impl Into<SharedString>,
    ) -> Self {
        self.details.push((label.into(), value.into()));
        self
    }
}

impl RenderOnce for Banner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let (fill, tone_color) = self.tone.colors(&colors);
        let icon_name = self.tone.default_icon();
        let has_details = !self.details.is_empty();
        let details_open = has_details.then(|| {
            window.use_keyed_state(ElementId::from((self.id.clone(), "details")), cx, |_, _| {
                false
            })
        });
        let showing_details = details_open.as_ref().is_some_and(|state| *state.read(cx));
        let label = match &self.title {
            Some(title) => SharedString::from(format!("{title}. {}", self.message)),
            None => self.message.clone(),
        };
        let toggle_id = ElementId::from((self.id.clone(), "toggle-details"));
        // Test hooks: `{id}`, `{id}-details` and `{id}-toggle-details`.
        let selector = self.id.to_string();
        let one_line = self.title.is_none() && self.actions.is_empty() && !has_details;

        let root_selector = selector.clone();
        div()
            .id(self.id)
            .debug_selector(|| root_selector)
            .role(Role::Status)
            .aria_label(label)
            .flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .rounded(px(RADIUS_MD))
            .bg(rgb(fill))
            .map(|this| {
                if self.tone == BannerTone::Danger {
                    this.border_2().border_color(rgb(colors.danger))
                } else {
                    this.border_1().border_color(rgb(colors.border))
                }
            })
            .when(self.tone != BannerTone::Neutral, |this| {
                this.child(
                    div()
                        .flex_none()
                        .w(px(STATUS_BANNER_STRIP_WIDTH))
                        .bg(rgb(tone_color)),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .when(one_line, |this| this.items_center())
                    .gap(px(SPACE_3))
                    .px(px(SPACE_3))
                    .py(px(if one_line { SPACE_2 } else { SPACE_3 }))
                    .child(icon(
                        icon_name,
                        if one_line { ICON_SIZE_SM } else { ICON_SIZE_MD },
                        tone_color,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(SPACE_1))
                            .when_some(self.title, |this, title| {
                                this.child(
                                    div()
                                        .type_style(TYPE_BODY_STRONG)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(tone_color))
                                        .child(title),
                                )
                            })
                            .child(
                                div()
                                    .type_style(TYPE_BODY)
                                    .text_color(rgb(colors.foreground))
                                    .child(self.message),
                            )
                            .when(showing_details, |this| {
                                let selector = format!("{selector}-details");
                                this.child(
                                    render_details(&self.details, &colors)
                                        .debug_selector(|| selector),
                                )
                            })
                            .when(!self.actions.is_empty() || has_details, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap(px(SPACE_2))
                                        .pt(px(SPACE_1))
                                        .children(self.actions)
                                        .when_some(details_open, |this, state| {
                                            let selector = format!("{selector}-toggle-details");
                                            this.child(
                                                div().debug_selector(|| selector).child(
                                                    Button::new(
                                                        toggle_id,
                                                        if showing_details {
                                                            "Hide details"
                                                        } else {
                                                            "Show details"
                                                        },
                                                    )
                                                    .size(ButtonSize::Small)
                                                    .on_click(move |_, _, cx| {
                                                        state.update(cx, |open, cx| {
                                                            *open = !*open;
                                                            cx.notify();
                                                        });
                                                    }),
                                                ),
                                            )
                                        }),
                                )
                            }),
                    ),
            )
    }
}

/// The evidence lines: a muted label over each value, so
/// long addresses wrap instead of being cut off.
fn render_details(details: &[(SharedString, SharedString)], colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_2))
        .pt(px(SPACE_1))
        .children(details.iter().map(|(label, value)| {
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(label.clone()),
                )
                .child(
                    div()
                        .type_style(TYPE_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(colors.foreground))
                        .child(value.clone()),
                )
        }))
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext};
    use zmail_core::ThemeAppearance;

    use super::*;
    use crate::styles::contrast::{MIN_NON_TEXT_CONTRAST, MIN_TEXT_CONTRAST, contrast_ratio};

    #[test]
    fn every_tone_has_its_own_icon() {
        let icons = [
            BannerTone::Neutral,
            BannerTone::Info,
            BannerTone::Caution,
            BannerTone::Danger,
        ]
        .map(BannerTone::default_icon);
        for (index, icon) in icons.iter().enumerate() {
            assert!(
                !icons[index + 1..].contains(icon),
                "{icon} is used by more than one tone"
            );
        }
    }

    #[test]
    fn banner_text_and_icons_are_readable_in_both_themes() {
        for appearance in [ThemeAppearance::Light, ThemeAppearance::Dark] {
            let colors = ComponentTheme::fallback(appearance).colors;
            for tone in [
                BannerTone::Neutral,
                BannerTone::Info,
                BannerTone::Caution,
                BannerTone::Danger,
            ] {
                let (fill, tone_color) = tone.colors(&colors);
                for (what, color) in [("message", colors.foreground), ("muted", colors.muted)] {
                    let ratio = contrast_ratio(color, fill);
                    assert!(
                        ratio >= MIN_TEXT_CONTRAST,
                        "{appearance:?} {tone:?} {what}: {ratio:.2}:1"
                    );
                }
                // The title is text; the neutral tone has none and only
                // draws its icon in the muted colour.
                let ratio = contrast_ratio(tone_color, fill);
                assert!(
                    ratio >= MIN_TEXT_CONTRAST,
                    "{appearance:?} {tone:?} title/icon: {ratio:.2}:1"
                );
            }
            // The danger border is a state indicator.
            let ratio = contrast_ratio(colors.danger, colors.danger_background);
            assert!(ratio >= MIN_NON_TEXT_CONTRAST);
        }
    }

    struct Root;

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(480.)).child(
                Banner::new("banner", "Replies go to evil.example.")
                    .tone(BannerTone::Caution)
                    .title("Replies go elsewhere")
                    .detail("Reply-To", "evil@evil.example"),
            )
        }
    }

    #[gpui::test]
    fn show_details_toggles_the_evidence(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| Root);
        cx.run_until_parked();
        assert!(cx.debug_bounds("banner-details").is_none());
        let toggle = cx
            .debug_bounds("banner-toggle-details")
            .expect("the toggle is drawn")
            .center();
        cx.simulate_click(toggle, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("banner-details").is_some());
    }

    #[test]
    fn the_details_hook_names_follow_the_banner_id() {
        let id = ElementId::from(("sender-safety", 7usize));
        assert_eq!(id.to_string(), "sender-safety-7");
    }
}
