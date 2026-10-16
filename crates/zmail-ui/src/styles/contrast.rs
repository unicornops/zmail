//! WCAG 2.2 contrast maths for `0xRRGGBB` colours.
//!
//! [`UiColors`](super::UiColors) uses these helpers to derive tokens that
//! have no theme source (for example the text colour on an accent fill),
//! and the tests below check every colour pair the shell draws in both
//! bundled themes.

/// Minimum contrast for text and text-like glyphs (WCAG 1.4.3, level AA).
pub const MIN_TEXT_CONTRAST: f32 = 4.5;

/// Minimum contrast for icons, control boundaries, focus indicators and
/// state indicators such as selection (WCAG 1.4.11 and 2.4.13).
pub const MIN_NON_TEXT_CONTRAST: f32 = 3.0;

/// Relative luminance of an sRGB colour, per WCAG 2.2.
pub fn relative_luminance(rgb: u32) -> f32 {
    let channel = |shift: u32| {
        let value = ((rgb >> shift) & 0xFF) as f32 / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// WCAG contrast ratio between two opaque colours, from 1.0 to 21.0.
pub fn contrast_ratio(first: u32, second: u32) -> f32 {
    let first = relative_luminance(first);
    let second = relative_luminance(second);
    let (lighter, darker) = if first >= second {
        (first, second)
    } else {
        (second, first)
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// The colour to draw text in on `background`: the first of `preferred`
/// that reaches [`MIN_TEXT_CONTRAST`], otherwise whichever of `preferred`,
/// white and black contrasts most.
pub fn readable_on(background: u32, preferred: &[u32]) -> u32 {
    if let Some(&color) = preferred
        .iter()
        .find(|&&color| contrast_ratio(color, background) >= MIN_TEXT_CONTRAST)
    {
        return color;
    }
    preferred
        .iter()
        .copied()
        .chain([0xFFFFFF, 0x000000])
        .max_by(|a, b| contrast_ratio(*a, background).total_cmp(&contrast_ratio(*b, background)))
        .unwrap_or(0x000000)
}

/// Mixes `color` towards `toward` in small steps until it reaches
/// `min_ratio` against every colour in `against`, and returns the first
/// colour that does (or `toward` if none does). A colour that already
/// passes is returned unchanged.
pub fn ensure_contrast(color: u32, against: &[u32], min_ratio: f32, toward: u32) -> u32 {
    const STEPS: u32 = 64;
    let passes = |candidate: u32| {
        against
            .iter()
            .all(|&other| contrast_ratio(candidate, other) >= min_ratio)
    };
    (0..=STEPS)
        .map(|step| mix(color, toward, step as f32 / STEPS as f32))
        .find(|&candidate| passes(candidate))
        .unwrap_or(toward)
}

/// Linear mix of two colours in sRGB space: `amount` 0.0 is `from`, 1.0 is
/// `to`.
pub fn mix(from: u32, to: u32, amount: f32) -> u32 {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let a = ((from >> shift) & 0xFF) as f32;
        let b = ((to >> shift) & 0xFF) as f32;
        ((a + (b - a) * amount).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styles::UiColors;
    use zmail_core::{ThemeAppearance, ThemeSummary};
    use zmail_theme::ThemeFamily;

    fn approx(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 0.01
    }

    #[test]
    fn contrast_matches_known_wcag_values() {
        assert!(approx(contrast_ratio(0xFFFFFF, 0x000000), 21.0));
        assert!(approx(contrast_ratio(0x000000, 0xFFFFFF), 21.0));
        assert!(approx(contrast_ratio(0x777777, 0x777777), 1.0));
        // #767676 on white is the classic "just passes AA" grey.
        assert!(contrast_ratio(0x767676, 0xFFFFFF) >= 4.5);
        assert!(contrast_ratio(0x777777, 0xFFFFFF) < 4.5);
    }

    #[test]
    fn readable_on_prefers_theme_colours_then_falls_back() {
        assert_eq!(readable_on(0x1D4ED8, &[0x1E293B, 0xFFFFFF]), 0xFFFFFF);
        assert_eq!(readable_on(0x5BA3F5, &[0xE5EEF9, 0x111827]), 0x111827);
        // Neither mid grey reads on mid grey, so black or white wins.
        let pick = readable_on(0x808080, &[0x7F7F7F, 0x818181]);
        assert!(pick == 0x000000 || pick == 0xFFFFFF);
    }

    #[test]
    fn ensure_contrast_moves_only_as_far_as_needed() {
        // Already fine: unchanged.
        assert_eq!(ensure_contrast(0x000000, &[0xFFFFFF], 3.0, 0x000000), 0);
        let border = ensure_contrast(0xE2E8F0, &[0xFFFFFF, 0xF8FAFC], 3.0, 0x1E293B);
        assert!(contrast_ratio(border, 0xFFFFFF) >= 3.0);
        assert!(contrast_ratio(border, 0xF8FAFC) >= 3.0);
        // Not all the way to the target colour.
        assert_ne!(border, 0x1E293B);
    }

    /// Where a pair of colours is drawn, and the contrast it needs.
    struct Pair {
        what: String,
        fg: u32,
        bg: u32,
        min: f32,
    }

    /// Every foreground/background pairing the shell draws, by role.
    fn pairs(c: &UiColors) -> Vec<Pair> {
        let mut pairs = Vec::new();
        let mut push =
            |what: String, fg: u32, bg: u32, min: f32| pairs.push(Pair { what, fg, bg, min });

        // Surfaces body text, secondary text, links and status text sit
        // on: panes, cards, list rows in every state, and buttons.
        let fills = [
            ("background", c.background),
            ("surface", c.surface),
            ("selected_background", c.selected_background),
            ("hover_background", c.hover_background),
            ("button_background", c.button_background),
        ];
        for (fill_name, fill) in fills {
            for (role, color) in [
                ("foreground text", c.foreground),
                ("muted text and icons", c.muted),
                ("accent text (links)", c.accent),
                ("danger text", c.danger),
                ("warning text", c.warning),
                ("success text", c.success),
                ("info text", c.info),
            ] {
                push(
                    format!("{role} on {fill_name}"),
                    color,
                    fill,
                    MIN_TEXT_CONTRAST,
                );
            }
            // Focus rings and unread dots must stand out on any fill.
            for (role, color) in [("focus ring", c.focus_ring), ("accent indicator", c.accent)] {
                push(
                    format!("{role} on {fill_name}"),
                    color,
                    fill,
                    MIN_NON_TEXT_CONTRAST,
                );
            }
        }

        // Status banners: body text plus the status colour's own text.
        for (name, banner, status) in [
            ("danger", c.danger_background, c.danger),
            ("warning", c.warning_background, c.warning),
            ("success", c.success_background, c.success),
        ] {
            push(
                format!("foreground text on the {name} banner"),
                c.foreground,
                banner,
                MIN_TEXT_CONTRAST,
            );
            push(
                format!("{name} text on its banner"),
                status,
                banner,
                MIN_TEXT_CONTRAST,
            );
        }

        // Accent fills: badges, primary buttons and the focused selection.
        push(
            "accent_foreground on accent".to_owned(),
            c.accent_foreground,
            c.accent,
            MIN_TEXT_CONTRAST,
        );
        push(
            "selection_foreground on selection_background".to_owned(),
            c.selection_foreground,
            c.selection_background,
            MIN_TEXT_CONTRAST,
        );
        // The badge in the message list draws `background` on `accent`.
        push(
            "unread badge text".to_owned(),
            c.background,
            c.accent,
            MIN_TEXT_CONTRAST,
        );

        // State and boundary indicators against the panes they sit on.
        for (pane_name, pane) in [("background", c.background), ("surface", c.surface)] {
            push(
                format!("focused selection fill on {pane_name}"),
                c.selection_background,
                pane,
                MIN_NON_TEXT_CONTRAST,
            );
            push(
                format!("control border on {pane_name}"),
                c.control_border,
                pane,
                MIN_NON_TEXT_CONTRAST,
            );
        }

        // Avatars draw initials in the better of foreground/background on
        // one of these fills.
        for (fill_name, fill) in [
            ("accent", c.accent),
            ("info", c.info),
            ("success", c.success),
            ("warning", c.warning),
        ] {
            let initials =
                if contrast_ratio(c.foreground, fill) >= contrast_ratio(c.background, fill) {
                    c.foreground
                } else {
                    c.background
                };
            push(
                format!("avatar initials on {fill_name}"),
                initials,
                fill,
                MIN_TEXT_CONTRAST,
            );
        }

        pairs
    }

    fn bundled(json: &str) -> ThemeSummary {
        let family = ThemeFamily::from_json_str(json).expect("bundled theme should parse");
        let theme = family.default_theme().expect("bundled theme has a variant");
        family.summary(theme)
    }

    fn failures(theme: &ThemeSummary) -> Vec<String> {
        let colors = UiColors::from_theme(theme);
        pairs(&colors)
            .into_iter()
            .filter_map(|pair| {
                let ratio = contrast_ratio(pair.fg, pair.bg);
                (ratio < pair.min).then(|| {
                    format!(
                        "{}: {} #{:06X} on #{:06X} is {ratio:.2}:1, needs {}:1",
                        theme.theme_name, pair.what, pair.fg, pair.bg, pair.min
                    )
                })
            })
            .collect()
    }

    #[test]
    fn bundled_themes_meet_wcag_contrast() {
        let dark = bundled(zmail_theme::bundled::DARK);
        let light = bundled(zmail_theme::bundled::LIGHT);
        assert_eq!(dark.appearance, ThemeAppearance::Dark);
        assert_eq!(light.appearance, ThemeAppearance::Light);

        let failures: Vec<String> = [dark, light].iter().flat_map(failures).collect();
        assert!(
            failures.is_empty(),
            "contrast failures:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn derived_tokens_reach_contrast_for_a_low_contrast_theme() {
        // Zed's One Light keeps borders faint; the derived control border
        // and accent text still have to pass.
        let theme = ThemeSummary {
            family_name: "One".to_owned(),
            theme_name: "One Light".to_owned(),
            appearance: ThemeAppearance::Light,
            background: Some("#fafafaff".to_owned()),
            surface: Some("#ebebecff".to_owned()),
            border: Some("#c9c9caff".to_owned()),
            foreground: Some("#242529ff".to_owned()),
            muted_foreground: Some("#58585aff".to_owned()),
            accent: Some("#5c78e2ff".to_owned()),
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
        };
        let c = UiColors::from_theme(&theme);
        for pane in [c.background, c.surface] {
            assert!(contrast_ratio(c.control_border, pane) >= MIN_NON_TEXT_CONTRAST);
        }
        assert!(contrast_ratio(c.accent_foreground, c.accent) >= MIN_TEXT_CONTRAST);
        assert!(
            contrast_ratio(c.selection_foreground, c.selection_background) >= MIN_TEXT_CONTRAST
        );
    }
}
