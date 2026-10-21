//! High contrast (UI plan §4, Phase 8): the platform's contrast settings
//! applied to the theme palette.
//!
//! - **Windows contrast themes** (High Contrast, or Windows 11's Aquatic,
//!   Desert, Dusk and Night sky) replace every colour with the system
//!   colours the user picked, read from `HKCU\Control Panel\Colors`.
//! - **macOS Increase Contrast** keeps the theme but pushes secondary text
//!   and borders towards the foreground until they reach stronger ratios.
//! - Linux has no setting GPUI or the toolkit-free app can read yet, so it
//!   stays on the theme.
//!
//! GPUI reports neither setting, so [`platform_contrast`] asks the OS
//! directly, through safe APIs only, and treats any failed read as no
//! request. [`apply`] is the pure mapping, tested here on every platform;
//! the OS reads compile only on their own platform (in CI).

use gpui::{App, Global};

use super::UiColors;
use super::contrast::{
    MIN_NON_TEXT_CONTRAST, MIN_TEXT_CONTRAST, contrast_ratio, ensure_contrast, mix,
    relative_luminance,
};

/// Contrast for secondary text under Increase Contrast: WCAG AAA.
const INCREASED_TEXT_CONTRAST: f32 = 7.0;

/// How far a contrast theme's hover fill moves from the window colour
/// towards the text colour.
const HOVER_TINT: f32 = 0.2;

/// The same for an unfocused selection, a step stronger than hover.
const SELECTED_TINT: f32 = 0.3;

/// The system colours of a Windows contrast theme, as `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SystemColors {
    /// `Window`: the background of content.
    pub(crate) window: u32,
    /// `WindowText`: text on `window`.
    pub(crate) window_text: u32,
    /// `Hilight`: selected items and the focus.
    pub(crate) highlight: u32,
    /// `HilightText`: text on `highlight`.
    pub(crate) highlight_text: u32,
    /// `GrayText`: disabled text.
    pub(crate) gray_text: u32,
    /// `ButtonFace`: the fill of buttons.
    pub(crate) button_face: u32,
    /// `ButtonText`: text on `button_face`.
    pub(crate) button_text: u32,
    /// `HotTrackingColor`: links.
    pub(crate) hot_tracking: u32,
}

impl SystemColors {
    /// Windows' "High Contrast Black" theme, for the screenshot fixture.
    pub(crate) const HIGH_CONTRAST_BLACK: Self = Self {
        window: 0x000000,
        window_text: 0xFFFFFF,
        highlight: 0x1AEBFF,
        highlight_text: 0x000000,
        gray_text: 0x3FF23F,
        button_face: 0x000000,
        button_text: 0xFFFFFF,
        hot_tracking: 0xFFFF00,
    };

    /// Read the colours from `value`, which looks a registry value up by
    /// name (`"Window"`, `"WindowText"`, …). `None` when any is missing
    /// or unreadable: half a palette would be worse than the theme.
    pub(crate) fn from_registry(value: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let color = |name: &str| value(name).as_deref().and_then(parse_registry_rgb);
        Some(Self {
            window: color("Window")?,
            window_text: color("WindowText")?,
            highlight: color("Hilight")?,
            highlight_text: color("HilightText")?,
            gray_text: color("GrayText")?,
            button_face: color("ButtonFace")?,
            button_text: color("ButtonText")?,
            hot_tracking: color("HotTrackingColor")?,
        })
    }
}

/// What the platform asks of the palette.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Contrast {
    /// No request: the theme as it is.
    #[default]
    Standard,
    /// macOS Increase Contrast: stronger secondary text and borders.
    Increased,
    /// A Windows contrast theme: its system colours replace the theme's.
    System(SystemColors),
}

/// The active [`Contrast`], as a global the workspace keeps up to date.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContrastSetting {
    pub(crate) contrast: Contrast,
    /// Set by the screenshot fixture: keep this contrast whatever the
    /// platform says.
    pub(crate) simulated: bool,
}

impl Global for ContrastSetting {}

impl ContrastSetting {
    /// The active contrast, or [`Contrast::Standard`] before it is set.
    pub(crate) fn get(cx: &App) -> Contrast {
        cx.try_global::<Self>()
            .map(|setting| setting.contrast)
            .unwrap_or_default()
    }

    /// Re-read the platform's setting unless a fixture pinned one.
    /// Returns whether it changed.
    pub(crate) fn refresh(cx: &mut App) -> bool {
        let current = cx.try_global::<Self>().copied().unwrap_or_default();
        if current.simulated {
            return false;
        }
        let contrast = platform_contrast();
        if contrast == current.contrast && cx.has_global::<Self>() {
            return false;
        }
        cx.set_global(Self {
            contrast,
            simulated: false,
        });
        true
    }

    /// Pin `contrast`, ignoring the platform (the screenshot fixture).
    pub(crate) fn simulate(contrast: Contrast, cx: &mut App) {
        cx.set_global(Self {
            contrast,
            simulated: true,
        });
    }
}

/// `colors` adjusted for `contrast`.
pub(crate) fn apply(colors: UiColors, contrast: Contrast) -> UiColors {
    match contrast {
        Contrast::Standard => colors,
        Contrast::Increased => increased(colors),
        Contrast::System(system) => system_colors(system),
    }
}

/// Whether a palette is light, for the few things that depend on it (the
/// modal backdrop): its background is lighter than mid-grey.
pub(crate) fn is_light(colors: &UiColors) -> bool {
    relative_luminance(colors.background) > 0.5
}

/// Increase Contrast: secondary text reaches 7:1 and every border 4.5:1
/// against the background and surface, darkened (or lightened) towards
/// the foreground only as far as needed. Fills and accents stay.
fn increased(colors: UiColors) -> UiColors {
    let grounds = [colors.background, colors.surface];
    let toward = colors.foreground;
    let border = ensure_contrast(colors.border, &grounds, MIN_TEXT_CONTRAST, toward);
    UiColors {
        muted: ensure_contrast(colors.muted, &grounds, INCREASED_TEXT_CONTRAST, toward),
        border,
        control_border: border,
        ..colors
    }
}

/// A Windows contrast theme: every token is one of the user's system
/// colours, as Windows' own controls draw them. Status colours collapse
/// to the text colour; banners and badges still differ by their icons.
fn system_colors(system: SystemColors) -> UiColors {
    let SystemColors {
        window,
        window_text,
        highlight,
        highlight_text,
        gray_text: _,
        button_face,
        button_text: _,
        hot_tracking: _,
    } = system;
    // The focus ring is the highlight colour when it stands out from the
    // window, as in every built-in contrast theme; the text colour if not.
    let focus_ring = if contrast_ratio(highlight, window) >= MIN_NON_TEXT_CONTRAST {
        highlight
    } else {
        window_text
    };
    UiColors {
        background: window,
        surface: window,
        border: window_text,
        foreground: window_text,
        muted: window_text,
        accent: highlight,
        // Fills that keep the window text on them (hovered rows, a
        // selection in a list without focus): the window tinted towards
        // its text, which stays far above 4.5:1 in every contrast theme.
        selected_background: mix(window, window_text, SELECTED_TINT),
        hover_background: mix(window, window_text, HOVER_TINT),
        button_background: button_face,
        warning_background: window,
        success_background: window,
        danger_background: window,
        info_background: window,
        danger: window_text,
        warning: window_text,
        success: window_text,
        info: window_text,
        control_border: window_text,
        focus_ring,
        accent_foreground: highlight_text,
        selection_background: highlight,
        selection_foreground: highlight_text,
    }
}

/// A registry colour, `"R G B"` in decimal (`"255 255 0"`).
fn parse_registry_rgb(value: &str) -> Option<u32> {
    let mut channels = value.split_whitespace().map(|part| part.parse::<u8>().ok());
    let (Some(Some(red)), Some(Some(green)), Some(Some(blue)), None) = (
        channels.next(),
        channels.next(),
        channels.next(),
        channels.next(),
    ) else {
        return None;
    };
    Some(u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue))
}

/// Whether `HighContrast\Flags` (a decimal string) has `HCF_HIGHCONTRASTON`.
fn high_contrast_on(flags: &str) -> bool {
    const HCF_HIGHCONTRASTON: u32 = 0x1;
    flags
        .trim()
        .parse::<u32>()
        .is_ok_and(|flags| flags & HCF_HIGHCONTRASTON != 0)
}

/// What the platform asks for now. Any failed read counts as no request.
pub(crate) fn platform_contrast() -> Contrast {
    #[cfg(target_os = "windows")]
    {
        windows_contrast()
    }
    #[cfg(target_os = "macos")]
    {
        macos_contrast()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Contrast::Standard
    }
}

/// A contrast theme is on when `HighContrast\Flags` says so; its colours
/// are the user's system colours.
#[cfg(target_os = "windows")]
fn windows_contrast() -> Contrast {
    use windows_registry::CURRENT_USER;

    let on = CURRENT_USER
        .open(r"Control Panel\Accessibility\HighContrast")
        .and_then(|key| key.get_string("Flags"))
        .is_ok_and(|flags| high_contrast_on(&flags));
    if !on {
        return Contrast::Standard;
    }
    let Ok(colors) = CURRENT_USER.open(r"Control Panel\Colors") else {
        return Contrast::Standard;
    };
    SystemColors::from_registry(|name| colors.get_string(name).ok())
        .map_or(Contrast::Standard, Contrast::System)
}

/// System Settings → Accessibility → Display → Increase contrast.
#[cfg(target_os = "macos")]
fn macos_contrast() -> Contrast {
    use objc2_app_kit::NSWorkspace;

    if NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast() {
        Contrast::Increased
    } else {
        Contrast::Standard
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zmail_core::ThemeAppearance;

    use super::*;
    use crate::components::ComponentTheme;

    fn palettes() -> [UiColors; 2] {
        [ThemeAppearance::Light, ThemeAppearance::Dark]
            .map(|appearance| ComponentTheme::fallback(appearance).colors)
    }

    #[test]
    fn registry_colours_parse_as_decimal_triples() {
        assert_eq!(parse_registry_rgb("255 255 0"), Some(0xFFFF00));
        assert_eq!(parse_registry_rgb(" 26 235 255 "), Some(0x1AEBFF));
        assert_eq!(parse_registry_rgb("0 0 0"), Some(0x000000));
        assert_eq!(parse_registry_rgb("256 0 0"), None);
        assert_eq!(parse_registry_rgb("1 2"), None);
        assert_eq!(parse_registry_rgb("1 2 3 4"), None);
        assert_eq!(parse_registry_rgb("red"), None);
    }

    #[test]
    fn the_flags_value_says_whether_a_contrast_theme_is_on() {
        assert!(high_contrast_on("127"));
        assert!(high_contrast_on("1"));
        assert!(!high_contrast_on("126"));
        assert!(!high_contrast_on("0"));
        assert!(!high_contrast_on(""));
    }

    #[test]
    fn a_full_set_of_registry_colours_reads_and_a_partial_one_does_not() {
        let values: HashMap<&str, &str> = [
            ("Window", "0 0 0"),
            ("WindowText", "255 255 255"),
            ("Hilight", "26 235 255"),
            ("HilightText", "0 0 0"),
            ("GrayText", "63 242 63"),
            ("ButtonFace", "0 0 0"),
            ("ButtonText", "255 255 255"),
            ("HotTrackingColor", "255 255 0"),
        ]
        .into_iter()
        .collect();
        let lookup = |name: &str| values.get(name).map(|value| (*value).to_owned());
        assert_eq!(
            SystemColors::from_registry(lookup),
            Some(SystemColors::HIGH_CONTRAST_BLACK)
        );
        let partial = |name: &str| (name != "Hilight").then(|| lookup(name)).flatten();
        assert_eq!(SystemColors::from_registry(partial), None);
    }

    #[test]
    fn standard_contrast_leaves_the_palette_alone() {
        for colors in palettes() {
            assert_eq!(apply(colors, Contrast::Standard), colors);
        }
    }

    #[test]
    fn system_colours_replace_every_token() {
        let system = SystemColors::HIGH_CONTRAST_BLACK;
        for colors in palettes() {
            let mapped = apply(colors, Contrast::System(system));
            assert_eq!(mapped.background, system.window);
            assert_eq!(mapped.surface, system.window);
            assert_eq!(mapped.foreground, system.window_text);
            assert_eq!(mapped.muted, system.window_text);
            assert_eq!(mapped.border, system.window_text);
            assert_eq!(mapped.selection_background, system.highlight);
            assert_eq!(mapped.selection_foreground, system.highlight_text);
            assert_eq!(mapped.accent_foreground, system.highlight_text);
            assert_eq!(mapped.button_background, system.button_face);
            assert_eq!(mapped.focus_ring, system.highlight);
            // Nothing of the theme is left.
            assert_eq!(mapped, apply(palettes()[0], Contrast::System(system)));
        }
    }

    #[test]
    fn a_highlight_too_close_to_the_window_rings_focus_in_the_text_colour() {
        let system = SystemColors {
            highlight: 0x101010,
            ..SystemColors::HIGH_CONTRAST_BLACK
        };
        assert_eq!(
            apply(palettes()[0], Contrast::System(system)).focus_ring,
            system.window_text
        );
    }

    #[test]
    fn built_in_contrast_themes_keep_text_readable() {
        // High Contrast Black and White, and palettes like Windows 11's
        // Night sky and Desert.
        let themes = [
            SystemColors::HIGH_CONTRAST_BLACK,
            SystemColors {
                window: 0x000000,
                window_text: 0xFFFFFF,
                highlight: 0xD6B4FD,
                highlight_text: 0x2B2B2B,
                gray_text: 0xA6A6A6,
                button_face: 0x000000,
                button_text: 0xFFEE32,
                hot_tracking: 0x8080FF,
            },
            SystemColors {
                window: 0xFFFAEF,
                window_text: 0x3D3D3D,
                highlight: 0x903909,
                highlight_text: 0xFFF5E3,
                gray_text: 0x6D6D6D,
                button_face: 0xFFFAEF,
                button_text: 0x202020,
                hot_tracking: 0x1C5E75,
            },
            SystemColors {
                window: 0xFFFFFF,
                window_text: 0x000000,
                highlight: 0x37006E,
                highlight_text: 0xFFFFFF,
                gray_text: 0x600000,
                button_face: 0xFFFFFF,
                button_text: 0x000000,
                hot_tracking: 0x00009F,
            },
        ];
        for system in themes {
            let colors = apply(palettes()[0], Contrast::System(system));
            for (text, fill) in [
                (colors.foreground, colors.background),
                (colors.muted, colors.surface),
                (colors.foreground, colors.hover_background),
                (colors.foreground, colors.selected_background),
                (colors.selection_foreground, colors.selection_background),
                (colors.accent_foreground, colors.accent),
            ] {
                assert!(
                    contrast_ratio(text, fill) >= MIN_TEXT_CONTRAST,
                    "{system:?}: {text:06X} on {fill:06X}"
                );
            }
        }
    }

    #[test]
    fn increased_contrast_strengthens_secondary_text_and_borders_only() {
        for colors in palettes() {
            let mapped = apply(colors, Contrast::Increased);
            for ground in [mapped.background, mapped.surface] {
                assert!(contrast_ratio(mapped.muted, ground) >= INCREASED_TEXT_CONTRAST);
                assert!(contrast_ratio(mapped.border, ground) >= MIN_TEXT_CONTRAST);
                assert!(contrast_ratio(mapped.control_border, ground) >= MIN_TEXT_CONTRAST);
            }
            assert_eq!(mapped.background, colors.background);
            assert_eq!(mapped.foreground, colors.foreground);
            assert_eq!(mapped.accent, colors.accent);
            assert_eq!(mapped.selection_background, colors.selection_background);
        }
    }

    #[test]
    fn lightness_follows_the_background() {
        let [light, dark] = palettes();
        assert!(is_light(&light));
        assert!(!is_light(&dark));
        assert!(!is_light(&apply(
            light,
            Contrast::System(SystemColors::HIGH_CONTRAST_BLACK)
        )));
    }
}
