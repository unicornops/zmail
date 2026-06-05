//! Design tokens, spacing constants, and colour helpers for the Zmail UI shell.
//!
//! All magic numbers that previously lived as inline literals in `lib.rs` are
//! consolidated here so that every part of the UI draws from a single source of
//! truth.  `UiColors` is the primary consumer: call `UiColors::from_theme` once
//! at the top of `render()` and thread the result through every sub-function.
//!
//! The token catalogue intentionally leads the visual rollout; later UI steps
//! consume the remaining entries.

#![allow(dead_code)]

use zmail_core::{ThemeAppearance, ThemeSummary};

// ---------------------------------------------------------------------------
// Spacing — 4-px grid
// ---------------------------------------------------------------------------

/// 4 px
pub const SPACE_1: f32 = 4.0;
/// 8 px
pub const SPACE_2: f32 = 8.0;
/// 12 px
pub const SPACE_3: f32 = 12.0;
/// 16 px
pub const SPACE_4: f32 = 16.0;
/// 20 px
pub const SPACE_5: f32 = 20.0;
/// 24 px
pub const SPACE_6: f32 = 24.0;
/// 32 px
pub const SPACE_8: f32 = 32.0;

// ---------------------------------------------------------------------------
// Layout dimensions
// ---------------------------------------------------------------------------

/// Width of the left sidebar pane (accounts + mailboxes).
pub const SIDEBAR_WIDTH: f32 = 280.0;
/// Width of the message/thread list pane.
pub const MESSAGE_LIST_WIDTH: f32 = 380.0;
/// Default application window width.
pub const WINDOW_WIDTH: f32 = 1440.0;
/// Default application window height.
pub const WINDOW_HEIGHT: f32 = 920.0;
/// Height of the top toolbar strip.
pub const TOOLBAR_HEIGHT: f32 = 48.0;
/// Minimum height of a toolbar or action button.
pub const TOOLBAR_BUTTON_HEIGHT: f32 = 32.0;
/// Height of a single message-list row.
pub const LIST_ITEM_HEIGHT: f32 = 72.0;
/// Height of a single mailbox row in the sidebar.
pub const MAILBOX_ITEM_HEIGHT: f32 = 40.0;
/// Diameter of a sender avatar circle.
pub const AVATAR_SIZE: f32 = 32.0;
/// Width and height of compact circular icon-only controls.
pub const ICON_BUTTON_SIZE: f32 = 32.0;
/// Diameter of the empty-state icon container.
pub const EMPTY_STATE_ICON_FRAME_SIZE: f32 = 72.0;
/// Minimum height of a single-line input card body.
pub const INPUT_HEIGHT_SINGLE: f32 = 52.0;
/// Minimum height of a multiline input card body.
pub const INPUT_HEIGHT_MULTILINE: f32 = 132.0;
/// Diameter of the unread indicator dot.
pub const UNREAD_INDICATOR_SIZE: f32 = 8.0;
/// Width of the leading accent strip on status banners.
pub const STATUS_BANNER_STRIP_WIDTH: f32 = 4.0;
/// Width of a compact action-bar divider.
pub const ACTION_DIVIDER_WIDTH: f32 = 1.0;
/// Height of a compact action-bar divider.
pub const ACTION_DIVIDER_HEIGHT: f32 = 20.0;
/// Small icon size (inline, badges).
pub const ICON_SIZE_SM: f32 = 16.0;
/// Medium icon size (list rows, reader).
pub const ICON_SIZE_MD: f32 = 20.0;
/// Large icon size (toolbar, empty-states).
pub const ICON_SIZE_LG: f32 = 24.0;
/// Diameter of an unread-count badge pill.
pub const BADGE_SIZE: f32 = 20.0;

// ---------------------------------------------------------------------------
// Border radii
// ---------------------------------------------------------------------------

/// 4 px — tight rounding for small chips.
pub const RADIUS_SM: f32 = 4.0;
/// 6 px — standard card / button rounding.
pub const RADIUS_MD: f32 = 6.0;
/// 8 px — larger panel rounding.
pub const RADIUS_LG: f32 = 8.0;
/// 9999 px — full pill / circle rounding.
pub const RADIUS_FULL: f32 = 9999.0;

// ---------------------------------------------------------------------------
// Dark fallback palette
// ---------------------------------------------------------------------------

/// Hard-coded dark-theme fallback colours used when a theme token is absent.
pub mod dark {
    /// App background.
    pub const BACKGROUND: u32 = 0x111827;
    /// Surface colour for cards and secondary panes.
    pub const SURFACE: u32 = 0x162033;
    /// Border colour.
    pub const BORDER: u32 = 0x2D3B55;
    /// Primary text colour.
    pub const FOREGROUND: u32 = 0xE5EEF9;
    /// Muted / secondary text colour.
    pub const MUTED: u32 = 0x9FB0C7;
    /// Accent / highlight colour.
    pub const ACCENT: u32 = 0x5BA3F5;
    /// Background for a selected list row.
    pub const SELECTED_BACKGROUND: u32 = 0x203559;
    /// Background for a hovered interactive element.
    pub const HOVER_BACKGROUND: u32 = 0x1C2E4B;
    /// Background for non-selected toolbar / action buttons.
    pub const BUTTON_BACKGROUND: u32 = 0x1C2E4B;
    /// Background tint for warning / error status bars.
    pub const WARNING_BACKGROUND: u32 = 0x253550;
    /// Background tint for success / info status bars.
    pub const SUCCESS_BACKGROUND: u32 = 0x162B20;
    /// Danger / error semantic colour (text / icon).
    pub const DANGER: u32 = 0xF87171;
    /// Warning semantic colour (text / icon).
    pub const WARNING: u32 = 0xF59E0B;
    /// Success semantic colour (text / icon).
    pub const SUCCESS: u32 = 0x34D399;
    /// Info semantic colour (text / icon).
    pub const INFO: u32 = 0x60A5FA;
}

// ---------------------------------------------------------------------------
// Light fallback palette
// ---------------------------------------------------------------------------

/// Hard-coded light-theme fallback colours used when a light theme token is
/// absent.
pub mod light {
    /// App background.
    pub const BACKGROUND: u32 = 0xFFFFFF;
    /// Surface colour for cards and secondary panes.
    pub const SURFACE: u32 = 0xF8FAFC;
    /// Border colour.
    pub const BORDER: u32 = 0xE2E8F0;
    /// Primary text colour.
    pub const FOREGROUND: u32 = 0x1E293B;
    /// Muted / secondary text colour.
    pub const MUTED: u32 = 0x64748B;
    /// Accent / highlight colour.
    pub const ACCENT: u32 = 0x2563EB;
    /// Background for a selected list row.
    pub const SELECTED_BACKGROUND: u32 = 0xDBEAFE;
    /// Background for a hovered interactive element.
    pub const HOVER_BACKGROUND: u32 = 0xF1F5F9;
    /// Background for non-selected toolbar / action buttons.
    pub const BUTTON_BACKGROUND: u32 = 0xE2E8F0;
    /// Background tint for warning / error status bars.
    pub const WARNING_BACKGROUND: u32 = 0xFEF3C7;
    /// Background tint for success / info status bars.
    pub const SUCCESS_BACKGROUND: u32 = 0xDCFCE7;
    /// Danger / error semantic colour (text / icon).
    pub const DANGER: u32 = 0xEF4444;
    /// Warning semantic colour (text / icon).
    pub const WARNING: u32 = 0xD97706;
    /// Success semantic colour (text / icon).
    pub const SUCCESS: u32 = 0x16A34A;
    /// Info semantic colour (text / icon).
    pub const INFO: u32 = 0x2563EB;
}

// ---------------------------------------------------------------------------
// UiColors — computed palette resolved from a ThemeSummary at render time
// ---------------------------------------------------------------------------

/// A fully-resolved set of UI colours derived from a [`ThemeSummary`].
///
/// Construct once at the top of `render()` via [`UiColors::from_theme`] and
/// pass a reference down to every sub-render function, replacing the previous
/// pattern of ad-hoc `let background = hex_to_rgb(...)` locals.
///
/// All fields are plain `u32` ARGB values compatible with GPUI's `rgb()`.
/// The struct is `Copy` so closures can capture it by value without cloning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiColors {
    /// App-level background colour.
    pub background: u32,
    /// Secondary surface colour (cards, panels, sidebar).
    pub surface: u32,
    /// Border / divider colour.
    pub border: u32,
    /// Primary text colour.
    pub foreground: u32,
    /// Muted / secondary text colour.
    pub muted: u32,
    /// Accent colour (links, badges, active indicators).
    pub accent: u32,
    /// Background applied to the currently-selected list row.
    pub selected_background: u32,
    /// Background applied when an element is hovered.
    pub hover_background: u32,
    /// Background for non-selected toolbar and action buttons.
    pub button_background: u32,
    /// Tinted background for warning / error status banners.
    pub warning_background: u32,
    /// Tinted background for success / info status banners.
    pub success_background: u32,
    /// Danger / error foreground colour for icons and text.
    pub danger: u32,
    /// Warning foreground colour for icons and text.
    pub warning: u32,
    /// Success foreground colour for icons and text.
    pub success: u32,
    /// Info foreground colour for icons and text.
    pub info: u32,
}

impl UiColors {
    /// Resolve a complete [`UiColors`] from a [`ThemeSummary`], falling back
    /// to the light or dark palette constants that match the theme
    /// appearance for any token that is absent.
    pub fn from_theme(theme: &ThemeSummary) -> Self {
        let is_light = matches!(theme.appearance, ThemeAppearance::Light);
        let background = hex_to_rgb(
            theme.background.as_deref(),
            if is_light {
                light::BACKGROUND
            } else {
                dark::BACKGROUND
            },
        );
        let surface = hex_to_rgb(
            theme.surface.as_deref(),
            if is_light {
                light::SURFACE
            } else {
                dark::SURFACE
            },
        );
        let border = hex_to_rgb(
            theme.border.as_deref(),
            if is_light {
                light::BORDER
            } else {
                dark::BORDER
            },
        );
        let foreground = hex_to_rgb(
            theme.foreground.as_deref(),
            if is_light {
                light::FOREGROUND
            } else {
                dark::FOREGROUND
            },
        );
        let muted = hex_to_rgb(
            theme.muted_foreground.as_deref(),
            if is_light { light::MUTED } else { dark::MUTED },
        );
        let accent = hex_to_rgb(
            theme.accent.as_deref(),
            if is_light {
                light::ACCENT
            } else {
                dark::ACCENT
            },
        );
        let selected_background = hex_to_rgb(
            theme.selected_background.as_deref(),
            if is_light {
                light::SELECTED_BACKGROUND
            } else {
                dark::SELECTED_BACKGROUND
            },
        );
        let hover_background = hex_to_rgb(
            theme.hover_background.as_deref(),
            if is_light {
                light::HOVER_BACKGROUND
            } else {
                dark::HOVER_BACKGROUND
            },
        );

        let button_background = if is_light {
            light::BUTTON_BACKGROUND
        } else {
            dark::BUTTON_BACKGROUND
        };
        let warning_background = if is_light {
            light::WARNING_BACKGROUND
        } else {
            dark::WARNING_BACKGROUND
        };
        let success_background = if is_light {
            light::SUCCESS_BACKGROUND
        } else {
            dark::SUCCESS_BACKGROUND
        };

        let danger = hex_to_rgb(
            theme.danger.as_deref(),
            if is_light {
                light::DANGER
            } else {
                dark::DANGER
            },
        );
        let warning = hex_to_rgb(
            theme.warning.as_deref(),
            if is_light {
                light::WARNING
            } else {
                dark::WARNING
            },
        );
        let success = hex_to_rgb(
            theme.success.as_deref(),
            if is_light {
                light::SUCCESS
            } else {
                dark::SUCCESS
            },
        );
        let info = hex_to_rgb(
            theme.info.as_deref(),
            if is_light { light::INFO } else { dark::INFO },
        );

        Self {
            background,
            surface,
            border,
            foreground,
            muted,
            accent,
            selected_background,
            hover_background,
            button_background,
            warning_background,
            success_background,
            danger,
            warning,
            success,
            info,
        }
    }
}

// ---------------------------------------------------------------------------
// Colour parsing helper
// ---------------------------------------------------------------------------

/// Parse a CSS hex colour string such as `"#RRGGBB"` into the `u32` ARGB
/// value that GPUI's `rgb()` macro expects.
///
/// Returns `fallback` for any input that is `None`, not six hex digits, or
/// otherwise unparseable.
pub fn hex_to_rgb(hex: Option<&str>, fallback: u32) -> u32 {
    let Some(hex) = hex else {
        return fallback;
    };
    let trimmed = hex.trim_start_matches('#');
    if trimmed.len() != 6 {
        return fallback;
    }
    u32::from_str_radix(trimmed, 16).unwrap_or(fallback)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use zmail_core::{ThemeAppearance, ThemeSummary};

    fn minimal_theme() -> ThemeSummary {
        ThemeSummary {
            family_name: "Test".to_owned(),
            theme_name: "Test Dark".to_owned(),
            appearance: ThemeAppearance::Dark,
            background: Some("#111827".to_owned()),
            surface: Some("#162033".to_owned()),
            border: Some("#2D3B55".to_owned()),
            foreground: Some("#E5EEF9".to_owned()),
            muted_foreground: Some("#9FB0C7".to_owned()),
            accent: Some("#5BA3F5".to_owned()),
            selected_background: None,
            hover_background: None,
            danger: None,
            warning: None,
            success: None,
            info: None,
        }
    }

    fn minimal_light_theme() -> ThemeSummary {
        ThemeSummary {
            family_name: "Test".to_owned(),
            theme_name: "Test Light".to_owned(),
            appearance: ThemeAppearance::Light,
            background: Some("#FFFFFF".to_owned()),
            surface: Some("#F8FAFC".to_owned()),
            border: Some("#E2E8F0".to_owned()),
            foreground: Some("#1E293B".to_owned()),
            muted_foreground: Some("#64748B".to_owned()),
            accent: Some("#2563EB".to_owned()),
            selected_background: None,
            hover_background: None,
            danger: None,
            warning: None,
            success: None,
            info: None,
        }
    }

    #[test]
    fn hex_to_rgb_parses_valid_hex() {
        assert_eq!(hex_to_rgb(Some("#111827"), 0), 0x111827);
        assert_eq!(hex_to_rgb(Some("#5BA3F5"), 0), 0x5BA3F5);
    }

    #[test]
    fn hex_to_rgb_uses_fallback_for_none() {
        assert_eq!(hex_to_rgb(None, 0xDEADBE), 0xDEADBE);
    }

    #[test]
    fn hex_to_rgb_uses_fallback_for_invalid() {
        assert_eq!(hex_to_rgb(Some("not-a-color"), 0xDEADBE), 0xDEADBE);
        assert_eq!(hex_to_rgb(Some("#FFF"), 0xDEADBE), 0xDEADBE);
    }

    #[test]
    fn ui_colors_from_theme_uses_theme_tokens() {
        let theme = minimal_theme();
        let colors = UiColors::from_theme(&theme);
        assert_eq!(colors.background, 0x111827);
        assert_eq!(colors.surface, 0x162033);
        assert_eq!(colors.border, 0x2D3B55);
        assert_eq!(colors.foreground, 0xE5EEF9);
        assert_eq!(colors.muted, 0x9FB0C7);
        assert_eq!(colors.accent, 0x5BA3F5);
    }

    #[test]
    fn ui_colors_from_theme_falls_back_for_missing_tokens() {
        let theme = minimal_theme();
        let colors = UiColors::from_theme(&theme);
        // These tokens are absent in the minimal theme, so dark fallbacks apply.
        assert_eq!(colors.selected_background, dark::SELECTED_BACKGROUND);
        assert_eq!(colors.hover_background, dark::HOVER_BACKGROUND);
        assert_eq!(colors.danger, dark::DANGER);
        assert_eq!(colors.warning, dark::WARNING);
        assert_eq!(colors.success, dark::SUCCESS);
        assert_eq!(colors.info, dark::INFO);
    }

    #[test]
    fn ui_colors_from_light_theme_use_light_fallbacks() {
        let theme = minimal_light_theme();
        let colors = UiColors::from_theme(&theme);
        assert_eq!(colors.selected_background, light::SELECTED_BACKGROUND);
        assert_eq!(colors.hover_background, light::HOVER_BACKGROUND);
        assert_eq!(colors.button_background, light::BUTTON_BACKGROUND);
        assert_eq!(colors.warning_background, light::WARNING_BACKGROUND);
        assert_eq!(colors.success_background, light::SUCCESS_BACKGROUND);
        assert_eq!(colors.danger, light::DANGER);
        assert_eq!(colors.warning, light::WARNING);
        assert_eq!(colors.success, light::SUCCESS);
        assert_eq!(colors.info, light::INFO);
    }
}
