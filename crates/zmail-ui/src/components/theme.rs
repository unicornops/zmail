//! Shared styling for the component set: the active colours and platform
//! style, plus the focus ring every focusable component draws.
//!
//! Components read a [`ComponentTheme`] global instead of taking colours as
//! arguments, so a view writes `Button::new("save", "Save").primary()`
//! rather than threading `UiColors` through every call. The view that owns
//! the window calls [`ComponentTheme::sync`] whenever its theme changes.
//! Without a global the components fall back to the built-in dark palette.

use gpui::{App, BoxShadow, Global, InteractiveElement, Styled, point, px, rgb};
use zmail_core::{ThemeAppearance, ThemeSummary};

use crate::styles::contrast::{MIN_TEXT_CONTRAST, contrast_ratio, readable_on};
use crate::styles::{
    FOCUS_RING_OFFSET, FOCUS_RING_WIDTH, Platform, PlatformStyle, UiColors, blend_over,
};

/// The colours and platform style the components render with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ComponentTheme {
    /// The resolved theme palette.
    pub colors: UiColors,
    /// Whether the palette is a light theme.
    pub is_light: bool,
    /// The per-platform style tokens.
    pub platform: PlatformStyle,
}

impl Global for ComponentTheme {}

impl ComponentTheme {
    /// A theme for `colors` on the current platform.
    pub(crate) fn new(colors: UiColors, is_light: bool) -> Self {
        Self {
            colors,
            is_light,
            platform: PlatformStyle::current(),
        }
    }

    /// The theme resolved from a theme summary.
    pub(crate) fn from_summary(theme: &ThemeSummary) -> Self {
        Self::new(
            UiColors::from_theme(theme),
            matches!(theme.appearance, ThemeAppearance::Light),
        )
    }

    /// The built-in palette for `appearance`, used when no theme is set.
    pub(crate) fn fallback(appearance: ThemeAppearance) -> Self {
        Self::from_summary(&ThemeSummary {
            family_name: String::new(),
            theme_name: String::new(),
            appearance,
            background: None,
            surface: None,
            border: None,
            foreground: None,
            muted_foreground: None,
            accent: None,
            selected_background: None,
            hover_background: None,
            danger: None,
            warning: None,
            success: None,
            info: None,
            button_background: None,
            danger_background: None,
            warning_background: None,
            success_background: None,
        })
    }

    /// Use another platform's conventions (for the gallery and tests).
    pub(crate) fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = PlatformStyle::for_platform(platform);
        self
    }

    /// The active theme, or the dark fallback when none has been set.
    pub(crate) fn get(cx: &App) -> Self {
        cx.try_global::<Self>()
            .copied()
            .unwrap_or_else(|| Self::fallback(ThemeAppearance::Dark))
    }

    /// Install `theme` as the active theme. Does nothing when it is
    /// already active, so calling this from `render()` is cheap.
    pub(crate) fn set(theme: Self, cx: &mut App) {
        if cx.try_global::<Self>() != Some(&theme) {
            cx.set_global(theme);
        }
    }

    /// Install the theme resolved from `summary`.
    pub(crate) fn sync(summary: &ThemeSummary, cx: &mut App) {
        Self::set(Self::from_summary(summary), cx);
    }

    /// Text colour for content drawn on `fill`: the theme's foreground or
    /// background, whichever is readable (falling back to white or black).
    pub(crate) fn text_on(&self, fill: u32) -> u32 {
        if fill == self.colors.accent {
            return self.colors.accent_foreground;
        }
        let candidates = [self.colors.foreground, self.colors.background];
        let text = readable_on(fill, &candidates);
        if contrast_ratio(text, fill) >= MIN_TEXT_CONTRAST {
            text
        } else {
            // Neither passes: take the better of the two theme colours
            // over plain white or black, which `readable_on` might pick.
            candidates
                .into_iter()
                .chain([text])
                .max_by(|a, b| contrast_ratio(*a, fill).total_cmp(&contrast_ratio(*b, fill)))
                .unwrap_or(text)
        }
    }

    /// A hover tint of `fill`: its text colour blended over it at 8%.
    pub(crate) fn hover_tint(&self, fill: u32) -> u32 {
        blend_over(self.text_on(fill), 0x14, fill)
    }

    /// A pressed tint of `fill`: its text colour blended over it at 16%.
    pub(crate) fn active_tint(&self, fill: u32) -> u32 {
        blend_over(self.text_on(fill), 0x29, fill)
    }

    /// The box shadows that draw the focus ring: a [`FOCUS_RING_OFFSET`]
    /// gap in the background colour, then a [`FOCUS_RING_WIDTH`] ring in
    /// [`UiColors::focus_ring`].
    pub(crate) fn focus_ring_shadows(&self) -> Vec<BoxShadow> {
        let ring = |spread: f32, color: u32| BoxShadow {
            color: rgb(color).into(),
            offset: point(px(0.0), px(0.0)),
            blur_radius: px(0.0),
            spread_radius: px(spread),
            inset: false,
        };
        // Shadows paint in order, so the ring goes first and the gap
        // covers its inner part.
        vec![
            ring(FOCUS_RING_OFFSET + FOCUS_RING_WIDTH, self.colors.focus_ring),
            ring(FOCUS_RING_OFFSET, self.colors.background),
        ]
    }
}

/// Focus-ring styling shared by every focusable component.
///
/// GPUI has no CSS `outline`, so the ring is a pair of unblurred box
/// shadows outside the control's bounds. It never changes layout, and its
/// pixels sit over whatever surrounds the control, which the tokens
/// contrast test checks the ring colour against (3:1). It shows on
/// keyboard focus only (`:focus-visible`), not after a mouse click.
pub(crate) trait FocusRingExt: InteractiveElement + Styled + Sized {
    /// Show the focus ring while this element has keyboard focus.
    fn focus_ring(self, theme: &ComponentTheme) -> Self {
        let shadows = theme.focus_ring_shadows();
        self.focus_visible(move |style| style.shadow(shadows))
    }
}

impl<T: InteractiveElement + Styled> FocusRingExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styles::contrast::MIN_NON_TEXT_CONTRAST;

    #[test]
    fn focus_ring_is_two_pixels_outside_a_gap() {
        let theme = ComponentTheme::fallback(ThemeAppearance::Light);
        let shadows = theme.focus_ring_shadows();
        assert_eq!(shadows.len(), 2);
        let ring_width = shadows[0].spread_radius - shadows[1].spread_radius;
        assert!(ring_width >= px(2.0));
        assert!(shadows.iter().all(|shadow| shadow.blur_radius == px(0.0)));
    }

    /// The ring sits on the gap colour on one side and the surroundings
    /// on the other; both must reach 3:1.
    #[test]
    fn focus_ring_contrasts_with_its_gap_and_surroundings() {
        for appearance in [ThemeAppearance::Light, ThemeAppearance::Dark] {
            let colors = ComponentTheme::fallback(appearance).colors;
            for (name, around) in [
                ("background", colors.background),
                ("surface", colors.surface),
            ] {
                let ratio = contrast_ratio(colors.focus_ring, around);
                assert!(
                    ratio >= MIN_NON_TEXT_CONTRAST,
                    "{appearance:?} ring on {name}: {ratio:.2}:1"
                );
            }
        }
    }

    #[test]
    fn text_on_picks_a_readable_colour() {
        for appearance in [ThemeAppearance::Light, ThemeAppearance::Dark] {
            let theme = ComponentTheme::fallback(appearance);
            let colors = theme.colors;
            for fill in [
                colors.accent,
                colors.danger,
                colors.background,
                colors.button_background,
            ] {
                let ratio = contrast_ratio(theme.text_on(fill), fill);
                assert!(
                    ratio >= MIN_TEXT_CONTRAST,
                    "{appearance:?} text on #{fill:06X}: {ratio:.2}:1"
                );
            }
        }
    }
}
