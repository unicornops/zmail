//! Design tokens, spacing constants, and colour helpers for the Zmail UI shell.
//!
//! All magic numbers that previously lived as inline literals in `lib.rs` are
//! consolidated here so that every part of the UI draws from a single source of
//! truth.  `UiColors` is the primary consumer: call `UiColors::from_theme` once
//! at the top of `render()` and thread the result through every sub-function.
//!
//! Tokens 2.0 (UI plan §4, Phase 1 item 6) adds:
//!
//! - a rem-based type scale ([`TYPE_CAPTION`] … [`TYPE_HEADLINE`], applied
//!   with [`TypeStyleExt::type_style`]);
//! - the spacing (`SPACE_*`) and radius (`RADIUS_*`) scales below;
//! - [`DensityTokens`] for comfortable and compact rows;
//! - [`PlatformStyle`], the per-platform deltas from plan §2;
//! - contrast-checked colour roles on [`UiColors`] (`control_border`,
//!   `focus_ring`, `accent_foreground`, `selection_*`), with the WCAG
//!   helpers in [`contrast`] and a test over both bundled themes.
//!
//! The token catalogue intentionally leads the visual rollout; later UI steps
//! consume the remaining entries.

#![allow(dead_code)]

pub mod contrast;
mod density;
mod platform;
mod typography;

// Re-exported for the Phase 1 components and the per-pane views, which
// are the first consumers; until they land some names are unused.
#[allow(unused_imports)]
pub use density::DensityTokens;
#[allow(unused_imports)]
pub use platform::{
    Backdrop, MenuPlacement, Platform, PlatformStyle, PrimaryModifier, ScrollbarBehavior,
    ScrollbarStyle, SearchFieldPlacement, SendButtonPlacement, UiFont,
};
#[allow(unused_imports)]
pub use typography::{
    TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TYPE_HEADLINE, TYPE_SCALE, TYPE_TITLE, TypeStyle,
    TypeStyleExt,
};

use zmail_core::{ThemeAppearance, ThemeSummary};

use self::contrast::{MIN_NON_TEXT_CONTRAST, ensure_contrast, readable_on};

// ---------------------------------------------------------------------------
// Spacing — 4-px grid
// ---------------------------------------------------------------------------

/// 2 px — hairline gaps, such as between the lines of a row.
pub const SPACE_0_5: f32 = 2.0;
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
/// 40 px
pub const SPACE_10: f32 = 40.0;
/// 48 px
pub const SPACE_12: f32 = 48.0;

/// The spacing scale, smallest first. Paddings, gaps and margins use these
/// values only.
pub const SPACING_SCALE: [f32; 10] = [
    SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_5, SPACE_6, SPACE_8, SPACE_10, SPACE_12,
];

// ---------------------------------------------------------------------------
// Layout dimensions
// ---------------------------------------------------------------------------

/// Default width of the left sidebar pane (accounts + mailboxes). The
/// user can drag it between [`SIDEBAR_MIN_WIDTH`] and
/// [`SIDEBAR_MAX_WIDTH`]; `layout::PaneLayout` applies the limits.
pub const SIDEBAR_WIDTH: f32 = 280.0;
/// Narrowest the sidebar gets.
pub const SIDEBAR_MIN_WIDTH: f32 = 200.0;
/// Widest the sidebar gets.
pub const SIDEBAR_MAX_WIDTH: f32 = 420.0;
/// Default width of the message/thread list pane.
pub const MESSAGE_LIST_WIDTH: f32 = 380.0;
/// Narrowest the message list gets: room for a row's date beside a short
/// subject.
pub const MESSAGE_LIST_MIN_WIDTH: f32 = 280.0;
/// Widest the message list gets.
pub const MESSAGE_LIST_MAX_WIDTH: f32 = 640.0;
/// Narrowest the reader / compose pane gets; the sidebar and list give up
/// width before it does.
pub const DETAIL_MIN_WIDTH: f32 = 300.0;
/// Narrowest the toolbar search field gets. It fills the space between
/// the leading buttons and the sync indicator up to
/// [`SEARCH_FIELD_MAX_WIDTH`]; its width never depends on what is typed.
pub const SEARCH_FIELD_MIN_WIDTH: f32 = 160.0;
/// Widest the toolbar search field gets.
pub const SEARCH_FIELD_MAX_WIDTH: f32 = 520.0;
/// Below this reader width the reader shows only its main actions (Reply,
/// Archive, Delete) and moves the rest into its More menu.
pub const READER_COMPACT_ACTIONS_BELOW: f32 = 480.0;
/// Default application window width.
pub const WINDOW_WIDTH: f32 = 1440.0;
/// Default application window height.
pub const WINDOW_HEIGHT: f32 = 920.0;
/// Minimum application window width.
pub const WINDOW_MIN_WIDTH: f32 = 720.0;
/// Minimum application window height.
pub const WINDOW_MIN_HEIGHT: f32 = 480.0;
/// Height of the top toolbar strip.
pub const TOOLBAR_HEIGHT: f32 = 48.0;
/// Minimum height of a toolbar or action button.
pub const TOOLBAR_BUTTON_HEIGHT: f32 = 32.0;
/// Line height of the first (subject) line of a message-list row. The
/// line holds body-size text plus the unread dot and 16px icons.
pub const ROW_TITLE_LINE_HEIGHT: f32 = 20.0;
/// Line height of the second (sender) line of a message-list row. It also
/// holds the unread-count badge, so it is as tall as the badge.
pub const ROW_SENDER_LINE_HEIGHT: f32 = BADGE_SIZE;
/// Line height of the third (preview) line of a message-list row.
pub const ROW_PREVIEW_LINE_HEIGHT: f32 = 18.0;
/// Vertical gap between the lines of a message-list row.
pub const ROW_LINE_GAP: f32 = SPACE_0_5;
/// Top and bottom padding of a message-list row.
pub const ROW_PADDING_Y: f32 = 10.0;
/// Width of the divider drawn under each message-list row.
pub const ROW_BORDER_WIDTH: f32 = 1.0;
/// Height of a single message-list row, derived from its three lines so
/// the content always fits (see the `thread_row_content_fits` test).
pub const LIST_ITEM_HEIGHT: f32 = ROW_PADDING_Y * 2.0
    + ROW_TITLE_LINE_HEIGHT
    + ROW_SENDER_LINE_HEIGHT
    + ROW_PREVIEW_LINE_HEIGHT
    + ROW_LINE_GAP * 2.0
    + ROW_BORDER_WIDTH;
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
/// 12 px — sheets, popovers and other floating surfaces.
pub const RADIUS_XL: f32 = 12.0;
/// 9999 px — full pill / circle rounding.
pub const RADIUS_FULL: f32 = 9999.0;

// Window and control radii differ per platform: see
// `PlatformStyle::window_radius` and `PlatformStyle::control_radius`.

// ---------------------------------------------------------------------------
// Focus ring
// ---------------------------------------------------------------------------

/// Width of the keyboard focus ring (WCAG 2.4.13: at least 2px, drawn in
/// [`UiColors::focus_ring`], which is checked for 3:1 contrast).
pub const FOCUS_RING_WIDTH: f32 = 2.0;
/// Gap between a control's edge and its focus ring.
pub const FOCUS_RING_OFFSET: f32 = 2.0;

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
    /// Background tint for error banners.
    pub const DANGER_BACKGROUND: u32 = 0x3B1F2B;
    /// Danger / error semantic colour (text / icon).
    pub const DANGER: u32 = 0xFCA5A5;
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
    pub const BORDER: u32 = 0xCBD5E1;
    /// Primary text colour.
    pub const FOREGROUND: u32 = 0x1E293B;
    /// Muted / secondary text colour.
    pub const MUTED: u32 = 0x4B5768;
    /// Accent / highlight colour.
    pub const ACCENT: u32 = 0x1D4ED8;
    /// Background for a selected list row.
    pub const SELECTED_BACKGROUND: u32 = 0xBFDBFE;
    /// Background for a hovered interactive element.
    pub const HOVER_BACKGROUND: u32 = 0xF1F5F9;
    /// Background for non-selected toolbar / action buttons.
    pub const BUTTON_BACKGROUND: u32 = 0xE2E8F0;
    /// Background tint for warning / error status bars.
    pub const WARNING_BACKGROUND: u32 = 0xFEF3C7;
    /// Background tint for success / info status bars.
    pub const SUCCESS_BACKGROUND: u32 = 0xDCFCE7;
    /// Background tint for error banners.
    pub const DANGER_BACKGROUND: u32 = 0xFEE2E2;
    /// Danger / error semantic colour (text / icon).
    pub const DANGER: u32 = 0xB91C1C;
    /// Warning semantic colour (text / icon).
    pub const WARNING: u32 = 0x92400E;
    /// Success semantic colour (text / icon).
    pub const SUCCESS: u32 = 0x166534;
    /// Info semantic colour (text / icon).
    pub const INFO: u32 = 0x1D4ED8;
}

// ---------------------------------------------------------------------------
// UiColors — computed palette resolved from a ThemeSummary at render time
// ---------------------------------------------------------------------------

/// A fully-resolved set of UI colours derived from a [`ThemeSummary`].
///
/// Construct once at the top of `render()` via [`UiColors::from_theme`] and
/// pass a reference down to every sub-render function.
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
    /// Tinted background for error banners.
    pub danger_background: u32,
    /// Danger / error foreground colour for icons and text.
    pub danger: u32,
    /// Warning foreground colour for icons and text.
    pub warning: u32,
    /// Success foreground colour for icons and text.
    pub success: u32,
    /// Info foreground colour for icons and text.
    pub info: u32,
    /// Boundary of buttons, inputs, checkboxes and other controls: at
    /// least 3:1 against the background and surface. Derived from
    /// `border`, darkened (or lightened) towards the foreground only as
    /// far as needed. Decorative dividers keep using `border`.
    pub control_border: u32,
    /// Keyboard focus ring, drawn [`FOCUS_RING_WIDTH`] wide. The theme's
    /// accent; the contrast test checks it reaches 3:1 on every fill.
    pub focus_ring: u32,
    /// Text and icons drawn on an `accent` fill (badges, primary buttons).
    pub accent_foreground: u32,
    /// Fill of a selected row while its list has keyboard focus (plan §2:
    /// "a full accent row when the list has focus, grey when it doesn't").
    /// The unfocused selection keeps `selected_background`.
    pub selection_background: u32,
    /// Text and icons on `selection_background`.
    pub selection_foreground: u32,
}

impl UiColors {
    /// Resolve a complete [`UiColors`] from a [`ThemeSummary`], falling back
    /// to the light or dark palette constants that match the theme
    /// appearance for any token that is absent or unparseable.
    ///
    /// Zed themes mostly use `#RRGGBBAA`. Translucent tokens are blended
    /// over what they are drawn on: the app background for the background
    /// itself, panes and text; the surface for row and button fills.
    pub fn from_theme(theme: &ThemeSummary) -> Self {
        let is_light = matches!(theme.appearance, ThemeAppearance::Light);
        let pick = |light: u32, dark: u32| if is_light { light } else { dark };

        let fallback_background = pick(light::BACKGROUND, dark::BACKGROUND);
        let background = theme_color(
            theme.background.as_deref(),
            fallback_background,
            fallback_background,
        );
        let over_background = |token: &Option<String>, light: u32, dark: u32| {
            theme_color(token.as_deref(), pick(light, dark), background)
        };
        let surface = over_background(&theme.surface, light::SURFACE, dark::SURFACE);
        let over_surface = |token: &Option<String>, light: u32, dark: u32| {
            theme_color(token.as_deref(), pick(light, dark), surface)
        };

        let border = over_background(&theme.border, light::BORDER, dark::BORDER);
        let foreground = over_background(&theme.foreground, light::FOREGROUND, dark::FOREGROUND);
        let accent = over_background(&theme.accent, light::ACCENT, dark::ACCENT);
        let accent_foreground = readable_on(accent, &[background, foreground]);

        Self {
            background,
            surface,
            border,
            foreground,
            muted: over_background(&theme.muted_foreground, light::MUTED, dark::MUTED),
            accent,
            selected_background: over_surface(
                &theme.selected_background,
                light::SELECTED_BACKGROUND,
                dark::SELECTED_BACKGROUND,
            ),
            hover_background: over_surface(
                &theme.hover_background,
                light::HOVER_BACKGROUND,
                dark::HOVER_BACKGROUND,
            ),
            button_background: over_surface(
                &theme.button_background,
                light::BUTTON_BACKGROUND,
                dark::BUTTON_BACKGROUND,
            ),
            warning_background: over_background(
                &theme.warning_background,
                light::WARNING_BACKGROUND,
                dark::WARNING_BACKGROUND,
            ),
            success_background: over_background(
                &theme.success_background,
                light::SUCCESS_BACKGROUND,
                dark::SUCCESS_BACKGROUND,
            ),
            danger_background: over_background(
                &theme.danger_background,
                light::DANGER_BACKGROUND,
                dark::DANGER_BACKGROUND,
            ),
            danger: over_background(&theme.danger, light::DANGER, dark::DANGER),
            warning: over_background(&theme.warning, light::WARNING, dark::WARNING),
            success: over_background(&theme.success, light::SUCCESS, dark::SUCCESS),
            info: over_background(&theme.info, light::INFO, dark::INFO),
            control_border: ensure_contrast(
                border,
                &[background, surface],
                MIN_NON_TEXT_CONTRAST,
                foreground,
            ),
            focus_ring: accent,
            accent_foreground,
            selection_background: accent,
            selection_foreground: accent_foreground,
        }
    }
}

impl UiColors {
    /// The bundled dark palette, for components drawn before the workspace
    /// has published the active theme's colours (for example in tests).
    pub fn fallback() -> Self {
        Self::from_theme(&ThemeSummary {
            family_name: String::new(),
            theme_name: String::new(),
            appearance: ThemeAppearance::Dark,
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
}

/// The workspace publishes the active palette as a GPUI global so
/// components that are their own entities (such as `TextInput`) draw with
/// the current theme.
impl gpui::Global for UiColors {}

// ---------------------------------------------------------------------------
// Colour parsing helpers
// ---------------------------------------------------------------------------

/// Parse a CSS hex colour (`#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`) into
/// its `0xRRGGBB` value and alpha byte.
pub fn parse_hex_color(hex: &str) -> Option<(u32, u8)> {
    let digits = hex.trim().strip_prefix('#')?;
    if !digits.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let expanded: String = match digits.len() {
        3 | 4 => digits.chars().flat_map(|ch| [ch, ch]).collect(),
        6 | 8 => digits.to_owned(),
        _ => return None,
    };
    let rgb = u32::from_str_radix(&expanded[..6], 16).ok()?;
    let alpha = match expanded.get(6..8) {
        Some(alpha) => u8::from_str_radix(alpha, 16).ok()?,
        None => 0xFF,
    };
    Some((rgb, alpha))
}

/// Composite `rgb` at `alpha` over the opaque colour `base`.
pub fn blend_over(rgb: u32, alpha: u8, base: u32) -> u32 {
    let alpha = u32::from(alpha);
    let channel = |shift: u32| {
        let top = (rgb >> shift) & 0xFF;
        let bottom = (base >> shift) & 0xFF;
        // Round to nearest.
        ((top * alpha + bottom * (255 - alpha) + 127) / 255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Resolve a theme token to an opaque `0xRRGGBB` colour for GPUI's `rgb()`:
/// translucent colours are blended over `base`, and missing or invalid
/// tokens use `fallback`.
pub fn theme_color(hex: Option<&str>, fallback: u32, base: u32) -> u32 {
    match hex.and_then(parse_hex_color) {
        Some((rgb, alpha)) => blend_over(rgb, alpha, base),
        None => fallback,
    }
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
            button_background: None,
            danger_background: None,
            warning_background: None,
            success_background: None,
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
            button_background: None,
            danger_background: None,
            warning_background: None,
            success_background: None,
        }
    }

    /// GPUI's default line height is the golden ratio times the font size,
    /// which is what made the old 72px rows overflow (~79px of content).
    /// Rows now set explicit line heights; this checks they hold the text
    /// and the inline elements, and that the derived height adds up.
    #[test]
    fn thread_row_content_fits() {
        // Root font size (1rem) in the shell.
        const REM: f32 = 16.0;
        // Smallest line height that does not clip glyphs at a font size.
        let min_line = |font_size: f32| font_size * 1.2;

        // Line 1: body text, unread dot and 16px flag/attachment icons.
        assert!(ROW_TITLE_LINE_HEIGHT >= min_line(REM));
        const { assert!(ROW_TITLE_LINE_HEIGHT >= ICON_SIZE_SM) };
        const { assert!(ROW_TITLE_LINE_HEIGHT >= UNREAD_INDICATOR_SIZE) };
        // Line 2: `text_sm` sender plus the unread-count badge.
        assert!(ROW_SENDER_LINE_HEIGHT >= min_line(REM * 0.875));
        const { assert!(ROW_SENDER_LINE_HEIGHT >= BADGE_SIZE) };
        // Line 3: `text_sm` preview.
        assert!(ROW_PREVIEW_LINE_HEIGHT >= min_line(REM * 0.875));

        let content = ROW_TITLE_LINE_HEIGHT
            + ROW_SENDER_LINE_HEIGHT
            + ROW_PREVIEW_LINE_HEIGHT
            + ROW_LINE_GAP * 2.0;
        let available = LIST_ITEM_HEIGHT - ROW_PADDING_Y * 2.0 - ROW_BORDER_WIDTH;
        assert!(
            content <= available,
            "row content ({content}px) must fit the row ({available}px available)"
        );
        // The avatar sits beside the three lines.
        assert!(AVATAR_SIZE <= available);
    }

    #[test]
    fn parses_opaque_and_translucent_hex() {
        assert_eq!(parse_hex_color("#111827"), Some((0x111827, 0xFF)));
        assert_eq!(parse_hex_color("#5ba3f5"), Some((0x5BA3F5, 0xFF)));
        assert_eq!(parse_hex_color("#282c33ff"), Some((0x282C33, 0xFF)));
        assert_eq!(parse_hex_color("#d072771a"), Some((0xD07277, 0x1A)));
        assert_eq!(parse_hex_color("#FFF"), Some((0xFFFFFF, 0xFF)));
        assert_eq!(parse_hex_color("#0F08"), Some((0x00FF00, 0x88)));
    }

    #[test]
    fn rejects_malformed_hex() {
        assert_eq!(parse_hex_color("not-a-color"), None);
        assert_eq!(parse_hex_color("111827"), None);
        assert_eq!(parse_hex_color("#12345"), None);
        assert_eq!(parse_hex_color("#GGGGGG"), None);
        assert_eq!(theme_color(Some("#12345"), 0xDEADBE, 0), 0xDEADBE);
        assert_eq!(theme_color(None, 0xDEADBE, 0), 0xDEADBE);
    }

    #[test]
    fn blends_alpha_over_the_base() {
        assert_eq!(blend_over(0xFFFFFF, 0xFF, 0x000000), 0xFFFFFF);
        assert_eq!(blend_over(0xFFFFFF, 0x00, 0x123456), 0x123456);
        // 50% white over black is mid grey.
        assert_eq!(blend_over(0xFFFFFF, 0x80, 0x000000), 0x808080);
        assert_eq!(theme_color(Some("#FF000080"), 0, 0x0000FF), 0x80007F);
    }

    #[test]
    fn real_zed_theme_tokens_resolve_instead_of_falling_back() {
        // Values from Zed's One Dark: every token carries an alpha byte.
        let theme = ThemeSummary {
            background: Some("#282c33ff".to_owned()),
            surface: Some("#2f343eff".to_owned()),
            border: Some("#464b57ff".to_owned()),
            foreground: Some("#dce0e5ff".to_owned()),
            muted_foreground: Some("#a9afbcff".to_owned()),
            accent: Some("#74ade8ff".to_owned()),
            selected_background: Some("#454a56ff".to_owned()),
            hover_background: Some("#363c4600".to_owned()),
            danger: Some("#d07277ff".to_owned()),
            danger_background: Some("#d072771a".to_owned()),
            ..minimal_theme()
        };
        let colors = UiColors::from_theme(&theme);
        assert_eq!(colors.background, 0x282C33);
        assert_eq!(colors.surface, 0x2F343E);
        assert_eq!(colors.foreground, 0xDCE0E5);
        assert_eq!(colors.accent, 0x74ADE8);
        assert_eq!(colors.danger, 0xD07277);
        // A fully transparent hover leaves the surface showing.
        assert_eq!(colors.hover_background, colors.surface);
        // 10% red over the background is a faint red tint.
        assert_eq!(
            colors.danger_background,
            blend_over(0xD07277, 0x1A, 0x282C33)
        );
    }

    #[test]
    fn status_backgrounds_come_from_the_theme() {
        let theme = ThemeSummary {
            button_background: Some("#101010".to_owned()),
            warning_background: Some("#202020".to_owned()),
            success_background: Some("#303030".to_owned()),
            danger_background: Some("#404040".to_owned()),
            ..minimal_theme()
        };
        let colors = UiColors::from_theme(&theme);
        assert_eq!(colors.button_background, 0x101010);
        assert_eq!(colors.warning_background, 0x202020);
        assert_eq!(colors.success_background, 0x303030);
        assert_eq!(colors.danger_background, 0x404040);
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
