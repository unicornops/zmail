//! Reads and writes the persisted [`UiPreferences`] for the workspace.
//!
//! Only the theme mode is wired up so far; density, pane widths, the
//! reading pane and the rest are stored by `zmail-store` and get wired as
//! the views that use them land.

use gpui::WindowAppearance;
use zmail_core::{
    ThemeAppearance,
    preferences::{ThemeMode, UiPreferences},
};

use super::WorkspaceView;

impl WorkspaceView {
    /// Applies the saved theme mode at startup: an explicit light or dark
    /// choice wins, and `System` follows the window's appearance.
    pub(crate) fn apply_theme_preference(&mut self, system: WindowAppearance) {
        let mode = match self.store.ui_preferences() {
            Ok(preferences) => preferences.theme_mode,
            Err(error) => {
                self.store_error = Some(format!("Could not load the UI preferences: {error:#}"));
                ThemeMode::System
            }
        };
        match forced_appearance(mode) {
            Some(appearance) => {
                self.follow_system_appearance(window_appearance(appearance));
                self.theme_overridden = true;
            }
            None => {
                self.follow_system_appearance(system);
            }
        }
    }

    /// Saves the theme the user just picked with the toolbar toggle.
    /// Picking the system's own appearance goes back to following it.
    pub(crate) fn persist_theme_choice(&mut self, system: WindowAppearance) {
        let chosen = self.bootstrap.theme.appearance;
        let mode = if chosen == theme_appearance(system) {
            self.theme_overridden = false;
            ThemeMode::System
        } else {
            match chosen {
                ThemeAppearance::Light => ThemeMode::Light,
                ThemeAppearance::Dark => ThemeMode::Dark,
            }
        };
        if let Err(error) = self
            .store
            .update_ui_preferences(|preferences: &mut UiPreferences| {
                preferences.theme_mode = mode;
            })
        {
            self.store_error = Some(format!("Could not save the theme: {error:#}"));
        }
    }
}

/// The appearance a theme mode forces, or `None` to follow the system.
fn forced_appearance(mode: ThemeMode) -> Option<ThemeAppearance> {
    match mode {
        ThemeMode::System => None,
        ThemeMode::Light => Some(ThemeAppearance::Light),
        ThemeMode::Dark => Some(ThemeAppearance::Dark),
    }
}

fn theme_appearance(appearance: WindowAppearance) -> ThemeAppearance {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
    }
}

fn window_appearance(appearance: ThemeAppearance) -> WindowAppearance {
    match appearance {
        ThemeAppearance::Light => WindowAppearance::Light,
        ThemeAppearance::Dark => WindowAppearance::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_modes_map_to_appearances() {
        assert_eq!(forced_appearance(ThemeMode::System), None);
        assert_eq!(
            forced_appearance(ThemeMode::Light),
            Some(ThemeAppearance::Light)
        );
        assert_eq!(
            forced_appearance(ThemeMode::Dark),
            Some(ThemeAppearance::Dark)
        );
        for appearance in [ThemeAppearance::Light, ThemeAppearance::Dark] {
            assert_eq!(theme_appearance(window_appearance(appearance)), appearance);
        }
        assert_eq!(
            theme_appearance(WindowAppearance::VibrantDark),
            ThemeAppearance::Dark
        );
    }
}
