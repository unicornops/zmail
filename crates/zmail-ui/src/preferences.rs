//! Reads and writes the persisted [`UiPreferences`] for the workspace.
//!
//! The theme mode, the imported theme, the sidebar's collapsed state, the
//! pane widths, the list density and the reading pane are wired up so far;
//! the rest are stored by `zmail-store` and get wired as the views that use
//! them land. The Settings window writes the preferences to the store and
//! then calls [`WorkspaceView::apply_preferences`].

use gpui::{Context, Window, WindowAppearance};
use zmail_core::{
    ThemeAppearance,
    preferences::{Density, PaneWidths, ReadingPanePosition, ThemeMode, UiPreferences},
};

use crate::actions::ToggleDensity;

use super::WorkspaceView;

impl WorkspaceView {
    /// Applies the saved theme at startup: the imported theme if there is
    /// one, then the theme mode. An explicit light or dark choice wins, and
    /// `System` follows the window's appearance.
    pub(crate) fn apply_theme_preference(&mut self, system: WindowAppearance) {
        self.load_imported_theme();
        let mode = match self.store.ui_preferences() {
            Ok(preferences) => preferences.theme_mode,
            Err(error) => {
                self.store_error = Some(format!("Could not load the UI preferences: {error:#}"));
                ThemeMode::System
            }
        };
        self.theme_overridden = forced_appearance(mode).is_some();
        self.use_theme_variant(forced_appearance(mode).unwrap_or(theme_appearance(system)));
    }

    /// Reads the imported Zed theme family from the store. One that no
    /// longer imports (stored by a version with different rules) is
    /// dropped with an error, and the bundled themes apply.
    fn load_imported_theme(&mut self) {
        self.imported_theme = match self.store.custom_theme() {
            Ok(Some(json)) => match zmail_theme::import_theme_family(&json) {
                Ok(theme) => Some(theme),
                Err(error) => {
                    self.store_error = Some(format!("Could not load the imported theme: {error}"));
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                self.store_error = Some(format!("Could not load the imported theme: {error:#}"));
                None
            }
        };
    }

    /// Re-reads the saved preferences and applies them to this window and
    /// every other: Settings calls this after each change.
    pub(crate) fn apply_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reading_pane = self.reading_pane;
        self.apply_theme_preference(window.appearance());
        self.apply_layout_preferences();
        if self.reading_pane != reading_pane {
            self.reading_pane_changed(window, cx);
        }
        self.apply_theme(cx);
    }

    /// Applies the saved layout at startup: whether the sidebar is hidden
    /// and the widths the user dragged the panes to.
    pub(crate) fn apply_layout_preferences(&mut self) {
        match self.store.ui_preferences() {
            Ok(preferences) => {
                self.sidebar_collapsed = preferences.sidebar_collapsed;
                self.density = preferences.density;
                self.reading_pane = supported_reading_pane(preferences.reading_pane);
                self.sender_warnings = preferences.sender_warnings;
                self.sidebar_tree = preferences.sidebar_tree;
                if let Some(width) = preferences.pane_widths.sidebar {
                    self.sidebar_width = width;
                }
                if let Some(width) = preferences.pane_widths.message_list {
                    self.list_width = width;
                }
            }
            Err(error) => {
                self.store_error = Some(format!("Could not load the UI preferences: {error:#}"));
            }
        }
    }

    /// Saves the mailbox the list shows, to reopen on the next launch.
    pub(crate) fn persist_last_mailbox(&mut self) {
        let Some(selection) = self.mailbox_selection() else {
            return;
        };
        if let Err(error) = self.store.update_ui_preferences(|preferences| {
            preferences.last_mailbox = Some(selection);
        }) {
            self.store_error = Some(format!("Could not save the open mailbox: {error:#}"));
        }
    }

    /// Saves the sidebar's collapsed accounts and folders.
    pub(crate) fn persist_sidebar_tree(&mut self) {
        let tree = self.sidebar_tree.clone();
        if let Err(error) = self.store.update_ui_preferences(|preferences| {
            preferences.sidebar_tree = tree;
        }) {
            self.store_error = Some(format!("Could not save the sidebar: {error:#}"));
        }
    }

    /// Saves whether the sidebar is hidden.
    pub(crate) fn persist_sidebar_collapsed(&mut self) {
        let collapsed = self.sidebar_collapsed;
        if let Err(error) = self.store.update_ui_preferences(|preferences| {
            preferences.sidebar_collapsed = collapsed;
        }) {
            self.store_error = Some(format!("Could not save the layout: {error:#}"));
        }
    }

    /// Switches the message list between comfortable and compact rows and
    /// saves the choice.
    pub(crate) fn toggle_density(
        &mut self,
        _: &ToggleDensity,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.density = match self.density {
            Density::Comfortable => Density::Compact,
            Density::Compact => Density::Comfortable,
        };
        let density = self.density;
        if let Err(error) = self.store.update_ui_preferences(|preferences| {
            preferences.density = density;
        }) {
            self.store_error = Some(format!("Could not save the row density: {error:#}"));
        }
        crate::settings::preferences_changed(cx);
        cx.notify();
    }

    /// Saves the sidebar and list widths.
    pub(crate) fn persist_pane_widths(&mut self) {
        let (sidebar, list) = (self.sidebar_width, self.list_width);
        if let Err(error) = self.store.update_ui_preferences(|preferences| {
            preferences.pane_widths = PaneWidths {
                sidebar: Some(sidebar),
                message_list: Some(list),
            };
        }) {
            self.store_error = Some(format!("Could not save the pane widths: {error:#}"));
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

/// The reading-pane positions the layout supports: beside the list, or
/// none. A list-above-reader layout (`Bottom`) is not built yet, so it
/// shows the reader beside the list.
pub(crate) fn supported_reading_pane(position: ReadingPanePosition) -> ReadingPanePosition {
    match position {
        ReadingPanePosition::Off => ReadingPanePosition::Off,
        ReadingPanePosition::Right | ReadingPanePosition::Bottom => ReadingPanePosition::Right,
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

pub(crate) fn theme_appearance(appearance: WindowAppearance) -> ThemeAppearance {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_appearance(appearance: ThemeAppearance) -> WindowAppearance {
        match appearance {
            ThemeAppearance::Light => WindowAppearance::Light,
            ThemeAppearance::Dark => WindowAppearance::Dark,
        }
    }

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
