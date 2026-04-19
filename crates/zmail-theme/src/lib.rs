//! Thin compatibility layer for unchanged Zed theme family files.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

        let danger = theme
            .style
            .color("status_bar.error_background")
            .or_else(|| theme.style.color("error"))
            .map(ToOwned::to_owned);

        let warning = theme
            .style
            .color("status_bar.warning_background")
            .or_else(|| theme.style.color("warning"))
            .map(ToOwned::to_owned);

        let success = theme
            .style
            .color("status_bar.info_background")
            .or_else(|| theme.style.color("success"))
            .map(ToOwned::to_owned);

        let info = theme.style.color("info").map(ToOwned::to_owned);

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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Appearance, SemanticTheme, ThemeFamily};

    const SAMPLE_THEME: &str = include_str!("../../../themes/phase0-dark.json");
    const SAMPLE_LIGHT_THEME: &str = include_str!("../../../themes/phase0-light.json");

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
}
