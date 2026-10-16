//! Thin compatibility layer for unchanged Zed theme family files.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zmail_core::{ThemeAppearance, ThemeSummary};

/// The bundled theme families, in unchanged Zed theme JSON.
pub mod bundled {
    /// The bundled dark theme family.
    pub const DARK: &str = include_str!("../../../themes/phase0-dark.json");
    /// The bundled light theme family.
    pub const LIGHT: &str = include_str!("../../../themes/phase0-light.json");
}

/// A serialized Zed theme family.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeFamily {
    /// Theme family author.
    pub author: String,
    /// Theme family display name.
    pub name: String,
    /// Theme variants inside the family.
    pub themes: Vec<Theme>,
}

impl ThemeFamily {
    /// Parses a theme family from a Zed-format JSON string.
    pub fn from_json_str(input: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(input)
    }

    /// Returns the first theme variant, which is enough for Phase 0 bootstrap work.
    pub fn default_theme(&self) -> Option<&Theme> {
        self.themes.first()
    }

    /// The UI-facing summary of `theme`, a variant of this family.
    pub fn summary(&self, theme: &Theme) -> ThemeSummary {
        let semantic = SemanticTheme::from_theme(theme);
        ThemeSummary {
            family_name: self.name.clone(),
            theme_name: theme.name.clone(),
            appearance: match theme.appearance {
                Appearance::Light => ThemeAppearance::Light,
                Appearance::Dark => ThemeAppearance::Dark,
            },
            background: semantic.background,
            surface: semantic.surface,
            border: semantic.border,
            foreground: semantic.foreground,
            muted_foreground: semantic.muted_foreground,
            accent: semantic.accent,
            selected_background: semantic.selected_background,
            hover_background: semantic.hover_background,
            danger: semantic.danger,
            warning: semantic.warning,
            success: semantic.success,
            info: semantic.info,
            button_background: semantic.button_background,
            danger_background: semantic.danger_background,
            warning_background: semantic.warning_background,
            success_background: semantic.success_background,
        }
    }
}

/// A single named theme variant within a Zed theme family.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Theme {
    /// Light or dark appearance.
    pub appearance: Appearance,
    /// Theme variant name.
    pub name: String,
    /// The style token map.
    pub style: ThemeStyle,
}

/// Appearance mode from the Zed schema.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Light theme.
    Light,
    /// Dark theme.
    Dark,
}

/// Flexible style token storage for unchanged Zed theme files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeStyle {
    /// Accent palette as defined by the schema.
    #[serde(default)]
    pub accents: Vec<Option<String>>,
    /// Any remaining theme tokens.
    #[serde(flatten)]
    pub tokens: BTreeMap<String, Value>,
}

impl ThemeStyle {
    /// Reads a color token as a string, if the token exists and is a string value.
    pub fn color(&self, key: &str) -> Option<&str> {
        match self.tokens.get(key) {
            Some(Value::String(value)) => Some(value.as_str()),
            _ => None,
        }
    }
}

/// A small semantic palette extracted from a Zed theme for use in the Phase 0 shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticTheme {
    /// App background color.
    pub background: Option<String>,
    /// Surface color.
    pub surface: Option<String>,
    /// Border color.
    pub border: Option<String>,
    /// Primary text color.
    pub foreground: Option<String>,
    /// Muted text color.
    pub muted_foreground: Option<String>,
    /// Accent color.
    pub accent: Option<String>,
    /// Selected item background color.
    pub selected_background: Option<String>,
    /// Hovered item background color.
    pub hover_background: Option<String>,
    /// Danger / error semantic color.
    pub danger: Option<String>,
    /// Warning semantic color.
    pub warning: Option<String>,
    /// Success semantic color.
    pub success: Option<String>,
    /// Info semantic color.
    pub info: Option<String>,
    /// Background for buttons and other interactive elements.
    pub button_background: Option<String>,
    /// Background tint for error banners.
    pub danger_background: Option<String>,
    /// Background tint for warning banners.
    pub warning_background: Option<String>,
    /// Background tint for success banners.
    pub success_background: Option<String>,
}

impl SemanticTheme {
    /// Builds a small semantic palette from a parsed theme.
    pub fn from_theme(theme: &Theme) -> Self {
        let surface = theme
            .style
            .color("surface.background")
            .or_else(|| theme.style.color("element.background"))
            .map(ToOwned::to_owned);

        let foreground = theme
            .style
            .color("text")
            .or_else(|| theme.style.color("editor.foreground"))
            .map(ToOwned::to_owned);

        let muted_foreground = theme
            .style
            .color("text.muted")
            .or_else(|| theme.style.color("icon.muted"))
            .map(ToOwned::to_owned);

        let accent = theme
            .style
            .accents
            .iter()
            .flatten()
            .next()
            .cloned()
            .or_else(|| theme.style.color("element.selected").map(ToOwned::to_owned));

        let selected_background = theme.style.color("element.selected").map(ToOwned::to_owned);

        let hover_background = theme.style.color("element.hover").map(ToOwned::to_owned);

        // Text and icon colours come from Zed's status foreground tokens;
        // the `status_bar.*_background` tokens are fills for Zed's status
        // bar, not foreground colours.
        let color = |key: &str| theme.style.color(key).map(ToOwned::to_owned);
        let danger = color("error");
        let warning = color("warning");
        let success = color("success");
        let info = color("info");

        Self {
            background: theme.style.color("background").map(ToOwned::to_owned),
            surface,
            border: theme.style.color("border").map(ToOwned::to_owned),
            foreground,
            muted_foreground,
            accent,
            selected_background,
            hover_background,
            danger,
            warning,
            success,
            info,
            button_background: color("element.background"),
            danger_background: color("error.background"),
            warning_background: color("warning.background"),
            success_background: color("success.background"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Appearance, SemanticTheme, ThemeFamily};

    use super::bundled::{DARK as SAMPLE_THEME, LIGHT as SAMPLE_LIGHT_THEME};

    #[test]
    fn parses_an_unchanged_zed_theme_file() {
        let family = ThemeFamily::from_json_str(SAMPLE_THEME).expect("theme family should parse");
        let theme = family
            .default_theme()
            .expect("theme family should have a default theme");
        let semantic = SemanticTheme::from_theme(theme);

        assert_eq!(family.name, "Zmail Phase 0");
        assert_eq!(theme.name, "Zmail Phase 0 Dark");
        assert_eq!(semantic.background.as_deref(), Some("#111827"));
        assert_eq!(semantic.accent.as_deref(), Some("#5BA3F5"));
    }

    #[test]
    fn parses_the_bundled_light_theme_file() {
        let family =
            ThemeFamily::from_json_str(SAMPLE_LIGHT_THEME).expect("theme family should parse");
        let theme = family
            .default_theme()
            .expect("theme family should have a default theme");
        let semantic = SemanticTheme::from_theme(theme);

        assert_eq!(family.name, "Zmail Phase 0");
        assert_eq!(theme.name, "Zmail Phase 0 Light");
        assert_eq!(theme.appearance, Appearance::Light);
        assert_eq!(semantic.background.as_deref(), Some("#FFFFFF"));
        assert_eq!(semantic.hover_background.as_deref(), Some("#F1F5F9"));
        assert_eq!(semantic.accent.as_deref(), Some("#2563EB"));
    }

    #[test]
    fn status_colours_come_from_foreground_tokens() {
        let family = ThemeFamily::from_json_str(
            r##"{
              "name": "Zed-like",
              "author": "test",
              "themes": [{
                "name": "Zed-like Dark",
                "appearance": "dark",
                "style": {
                  "background": "#282c33ff",
                  "element.background": "#2e343eff",
                  "error": "#d07277ff",
                  "error.background": "#d072771a",
                  "warning": "#dec184ff",
                  "warning.background": "#dec1841a",
                  "success": "#a1c181ff",
                  "success.background": "#a1c1811a",
                  "info": "#74ade8ff",
                  "status_bar.error_background": "#000000ff",
                  "status_bar.warning_background": "#000000ff",
                  "status_bar.info_background": "#000000ff"
                }
              }]
            }"##,
        )
        .expect("theme family should parse");
        let theme = family.default_theme().expect("theme exists");
        let semantic = SemanticTheme::from_theme(theme);

        assert_eq!(semantic.danger.as_deref(), Some("#d07277ff"));
        assert_eq!(semantic.warning.as_deref(), Some("#dec184ff"));
        assert_eq!(semantic.success.as_deref(), Some("#a1c181ff"));
        assert_eq!(semantic.info.as_deref(), Some("#74ade8ff"));
        assert_eq!(semantic.button_background.as_deref(), Some("#2e343eff"));
        assert_eq!(semantic.danger_background.as_deref(), Some("#d072771a"));
        assert_eq!(semantic.warning_background.as_deref(), Some("#dec1841a"));
        assert_eq!(semantic.success_background.as_deref(), Some("#a1c1811a"));
    }
}
