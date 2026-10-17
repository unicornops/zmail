//! Per-platform style deltas (UI plan §2, "Per-platform deltas").
//!
//! Everything that differs between macOS, Windows and Linux lives in one
//! [`PlatformStyle`] value, chosen at compile time with `cfg!(target_os)`.
//! Views read it through [`PlatformStyle::current`]; they never branch on
//! the OS themselves. Each platform's value is also available as a
//! constant so tests and screenshots can render any platform's style.

/// A desktop platform Zmail ships on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS.
    MacOs,
    /// Windows 11.
    Windows,
    /// Linux and other Unix desktops (GNOME, KDE and others).
    Linux,
}

impl Platform {
    /// The platform this binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// The modifier the default keymap's `secondary-` bindings use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryModifier {
    /// Command (⌘), on macOS.
    Cmd,
    /// Control, on Windows and Linux.
    Ctrl,
}

impl PrimaryModifier {
    /// How the modifier is written in tooltips and `Kbd` hints.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cmd => "⌘",
            Self::Ctrl => "Ctrl",
        }
    }
}

/// The UI font family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiFont {
    /// The system UI font via GPUI's `.SystemUIFont` alias (SF Pro on
    /// macOS, Segoe UI Variable on Windows).
    System,
    /// A font the app bundles and registers with
    /// `text_system().add_fonts()` before first render.
    Bundled(&'static str),
}

impl UiFont {
    /// The family name to pass to GPUI's `font_family`.
    pub const fn family(self) -> &'static str {
        match self {
            Self::System => ".SystemUIFont",
            Self::Bundled(family) => family,
        }
    }
}

/// Where the app's command menu lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuPlacement {
    /// The native menu bar, via `cx.set_menus`.
    MenuBar,
    /// An overflow (⋯) button at the end of the custom title bar.
    TitleBarOverflow,
    /// A hamburger button at the end of the header bar.
    HeaderBarHamburger,
}

/// Where the compose window's Send button sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendButtonPlacement {
    /// Trailing end of the window toolbar.
    ToolbarTrailing,
    /// Bottom-left of the compose body.
    ComposeBottomLeading,
    /// Trailing end of the header bar.
    HeaderBarTrailing,
}

/// Where the search field sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFieldPlacement {
    /// Trailing end of the toolbar.
    ToolbarTrailing,
    /// Centred in the title bar.
    TitleBarCentre,
    /// In the header bar, above the message list.
    HeaderBarAboveList,
}

/// The sidebar's backdrop material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// A solid theme colour.
    Opaque,
    /// Optional vibrancy (`WindowBackgroundAppearance::Blurred`).
    Blurred,
}

/// How scrollbars behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarBehavior {
    /// Drawn over content and hidden when idle (macOS follows the system's
    /// `should_auto_hide_scrollbars` setting at runtime).
    OverlayAutoHide,
    /// Always shown as a thin line that widens on hover.
    ThinWidenOnHover,
}

/// Scrollbar tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollbarStyle {
    /// Show/hide behaviour.
    pub behavior: ScrollbarBehavior,
    /// Thumb thickness at rest, in logical pixels.
    pub thickness: f32,
    /// Thumb thickness while hovered or dragged.
    pub hover_thickness: f32,
}

/// Style tokens that differ per platform. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlatformStyle {
    /// The platform these tokens are for.
    pub platform: Platform,
    /// Corner radius of the window, when the app draws its own frame.
    pub window_radius: f32,
    /// Whether the window corners go square when maximised.
    pub square_corners_when_maximized: bool,
    /// Corner radius of buttons, inputs and other controls.
    pub control_radius: f32,
    /// Height of standard buttons and single-line inputs.
    pub control_height: f32,
    /// Height of the title bar / header bar / unified toolbar.
    pub title_bar_height: f32,
    /// Modifier used by `secondary-` key bindings.
    pub primary_modifier: PrimaryModifier,
    /// UI font family.
    pub ui_font: UiFont,
    /// Where the command menu lives.
    pub menu: MenuPlacement,
    /// Where compose's Send button sits.
    pub send_button: SendButtonPlacement,
    /// Where the search field sits.
    pub search_field: SearchFieldPlacement,
    /// The sidebar backdrop.
    pub sidebar_backdrop: Backdrop,
    /// Scrollbar behaviour and size.
    pub scrollbar: ScrollbarStyle,
}

impl PlatformStyle {
    /// macOS: unified toolbar beside the traffic lights, native menu bar.
    pub const MACOS: Self = Self {
        platform: Platform::MacOs,
        window_radius: 10.0,
        square_corners_when_maximized: false,
        control_radius: 6.0,
        control_height: 28.0,
        title_bar_height: 52.0,
        primary_modifier: PrimaryModifier::Cmd,
        ui_font: UiFont::System,
        menu: MenuPlacement::MenuBar,
        send_button: SendButtonPlacement::ToolbarTrailing,
        search_field: SearchFieldPlacement::ToolbarTrailing,
        sidebar_backdrop: Backdrop::Blurred,
        scrollbar: ScrollbarStyle {
            behavior: ScrollbarBehavior::OverlayAutoHide,
            thickness: 7.0,
            hover_thickness: 11.0,
        },
    };

    /// Windows 11: custom 40px title bar with Snap Layouts controls.
    pub const WINDOWS: Self = Self {
        platform: Platform::Windows,
        window_radius: 8.0,
        square_corners_when_maximized: true,
        control_radius: 4.0,
        control_height: 32.0,
        title_bar_height: 40.0,
        primary_modifier: PrimaryModifier::Ctrl,
        ui_font: UiFont::System,
        menu: MenuPlacement::TitleBarOverflow,
        send_button: SendButtonPlacement::ComposeBottomLeading,
        search_field: SearchFieldPlacement::TitleBarCentre,
        sidebar_backdrop: Backdrop::Opaque,
        scrollbar: ScrollbarStyle {
            behavior: ScrollbarBehavior::ThinWidenOnHover,
            thickness: 3.0,
            hover_thickness: 8.0,
        },
    };

    /// Linux (GNOME/KDE): client-side header bar where the compositor
    /// allows it, bundled Inter font.
    pub const LINUX: Self = Self {
        platform: Platform::Linux,
        window_radius: 12.0,
        // Matches libadwaita: maximised and tiled windows lose their
        // rounded corners.
        square_corners_when_maximized: true,
        control_radius: 6.0,
        control_height: 34.0,
        title_bar_height: 46.0,
        primary_modifier: PrimaryModifier::Ctrl,
        // GPUI's Linux system-font lookup resolves to a font it does not
        // ship; Inter matches GNOME's Adwaita Sans. `fonts::register_ui_font`
        // registers the bundled faces at startup.
        ui_font: UiFont::Bundled("Inter"),
        menu: MenuPlacement::HeaderBarHamburger,
        send_button: SendButtonPlacement::HeaderBarTrailing,
        search_field: SearchFieldPlacement::HeaderBarAboveList,
        sidebar_backdrop: Backdrop::Opaque,
        scrollbar: ScrollbarStyle {
            behavior: ScrollbarBehavior::OverlayAutoHide,
            thickness: 3.0,
            hover_thickness: 8.0,
        },
    };

    /// The style for `platform`.
    pub const fn for_platform(platform: Platform) -> Self {
        match platform {
            Platform::MacOs => Self::MACOS,
            Platform::Windows => Self::WINDOWS,
            Platform::Linux => Self::LINUX,
        }
    }

    /// The style for the platform this binary was built for.
    pub const fn current() -> Self {
        Self::for_platform(Platform::current())
    }

    /// The window corner radius for the window's current state.
    pub fn window_radius_for(&self, maximized: bool) -> f32 {
        if maximized && self.square_corners_when_maximized {
            0.0
        } else {
            self.window_radius
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_matches_the_build_target() {
        let expected = if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        };
        assert_eq!(PlatformStyle::current().platform, expected);
    }

    #[test]
    fn radii_follow_the_plan_table() {
        let radii = |style: PlatformStyle| (style.window_radius, style.control_radius);
        assert_eq!(radii(PlatformStyle::MACOS), (10.0, 6.0));
        assert_eq!(radii(PlatformStyle::WINDOWS), (8.0, 4.0));
        assert_eq!(radii(PlatformStyle::LINUX), (12.0, 6.0));

        assert_eq!(PlatformStyle::WINDOWS.window_radius_for(true), 0.0);
        assert_eq!(PlatformStyle::WINDOWS.window_radius_for(false), 8.0);
        assert_eq!(PlatformStyle::MACOS.window_radius_for(true), 10.0);
    }

    #[test]
    fn modifiers_fonts_and_menus_follow_the_plan_table() {
        assert_eq!(PlatformStyle::MACOS.primary_modifier, PrimaryModifier::Cmd);
        assert_eq!(
            PlatformStyle::WINDOWS.primary_modifier,
            PrimaryModifier::Ctrl
        );
        assert_eq!(PlatformStyle::LINUX.primary_modifier, PrimaryModifier::Ctrl);
        assert_eq!(PlatformStyle::MACOS.ui_font.family(), ".SystemUIFont");
        assert_eq!(PlatformStyle::LINUX.ui_font.family(), "Inter");
        assert_eq!(PlatformStyle::WINDOWS.title_bar_height, 40.0);
        assert_eq!(PlatformStyle::MACOS.menu, MenuPlacement::MenuBar);
        assert_eq!(
            PlatformStyle::WINDOWS.scrollbar.behavior,
            ScrollbarBehavior::ThinWidenOnHover
        );
    }

    #[test]
    fn controls_fit_body_text_and_scrollbars_widen_on_hover() {
        for style in [
            PlatformStyle::MACOS,
            PlatformStyle::WINDOWS,
            PlatformStyle::LINUX,
        ] {
            // A control holds a 20px body line with some padding.
            assert!(style.control_height >= 24.0, "{style:?}");
            assert!(style.control_radius * 2.0 < style.control_height);
            assert!(style.scrollbar.hover_thickness >= style.scrollbar.thickness);
        }
    }
}
