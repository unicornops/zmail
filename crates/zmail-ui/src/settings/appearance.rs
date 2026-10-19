//! Settings → Appearance: the theme mode, an imported Zed theme, the list
//! density and the reading pane.
//!
//! Importing reads a Zed theme family file the user picks
//! (`prompt_for_paths`) and checks it with
//! [`zmail_theme::import_theme_family`]. A file that does not import shows
//! why in a banner and changes nothing; one that does is stored whole
//! (`LocalStore::set_custom_theme`), and its light and dark variants replace
//! the bundled ones. "Use Default Theme" goes back to the bundled themes.

use std::path::{Path, PathBuf};

use gpui::{
    Context, Entity, IntoElement, PathPromptOptions, Render, SharedString, Subscription, Window,
    div, prelude::*, px,
};
use zmail_core::preferences::{Density, ReadingPanePosition, ThemeMode, UiPreferences};
use zmail_theme::{MAX_THEME_FILE_BYTES, import_theme_family};

use super::{
    SettingsEvent, SettingsHost, apply_to_workspace, group_heading, section_heading, setting_row,
};
use crate::components::{Banner, BannerTone, Button, ComponentTheme, Select, SelectEvent};
use crate::preferences::supported_reading_pane;
use crate::styles::SPACE_2;

/// Width of the section's pickers.
const SELECT_WIDTH: f32 = 220.0;

const THEME_MODES: [(ThemeMode, &str); 3] = [
    (ThemeMode::System, "Match System"),
    (ThemeMode::Light, "Light"),
    (ThemeMode::Dark, "Dark"),
];

const DENSITIES: [(Density, &str); 2] = [
    (Density::Comfortable, "Comfortable"),
    (Density::Compact, "Compact"),
];

/// The reading-pane positions the layout supports (see
/// [`supported_reading_pane`]).
const READING_PANES: [(ReadingPanePosition, &str); 2] = [
    (ReadingPanePosition::Right, "Beside the List"),
    (ReadingPanePosition::Off, "Off"),
];

/// The Appearance section.
pub(crate) struct AppearanceSettings {
    theme_mode: Entity<Select>,
    density: Entity<Select>,
    reading_pane: Entity<Select>,
    /// The imported theme family's name, or `None` for the bundled themes.
    imported_theme: Option<String>,
    /// Why the last import failed, shown until the next import or reset.
    import_error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl AppearanceSettings {
    pub(crate) fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let select = |options: Vec<&'static str>, label: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                Select::new(options, None, cx)
                    .label(label)
                    .width(px(SELECT_WIDTH))
            })
        };
        let theme_mode = select(THEME_MODES.map(|(_, label)| label).to_vec(), "Theme", cx);
        let density = select(DENSITIES.map(|(_, label)| label).to_vec(), "Density", cx);
        let reading_pane = select(
            READING_PANES.map(|(_, label)| label).to_vec(),
            "Reading pane",
            cx,
        );
        let subscriptions = vec![
            cx.subscribe(&theme_mode, |_, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = *event;
                let mode = THEME_MODES[index].0;
                save(cx, "the theme", move |preferences| {
                    preferences.theme_mode = mode;
                });
            }),
            cx.subscribe(&density, |_, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = *event;
                let density = DENSITIES[index].0;
                save(cx, "the row density", move |preferences| {
                    preferences.density = density;
                });
            }),
            cx.subscribe(&reading_pane, |_, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = *event;
                let position = READING_PANES[index].0;
                save(cx, "the reading pane", move |preferences| {
                    preferences.reading_pane = position;
                });
            }),
        ];
        let mut this = Self {
            theme_mode,
            density,
            reading_pane,
            imported_theme: None,
            import_error: None,
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        this
    }

    /// Show the saved values.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let store = SettingsHost::get(cx).store.clone();
        let preferences = store.ui_preferences().unwrap_or_default();
        self.theme_mode.update(cx, |select, cx| {
            select.set_selected(
                THEME_MODES
                    .iter()
                    .position(|(mode, _)| *mode == preferences.theme_mode),
                cx,
            )
        });
        self.density.update(cx, |select, cx| {
            select.set_selected(
                DENSITIES
                    .iter()
                    .position(|(density, _)| *density == preferences.density),
                cx,
            )
        });
        let reading_pane = supported_reading_pane(preferences.reading_pane);
        self.reading_pane.update(cx, |select, cx| {
            select.set_selected(
                READING_PANES
                    .iter()
                    .position(|(position, _)| *position == reading_pane),
                cx,
            )
        });
        self.imported_theme = store
            .custom_theme()
            .ok()
            .flatten()
            .and_then(|json| import_theme_family(&json).ok())
            .map(|theme| theme.name);
        cx.notify();
    }

    /// The imported theme's name, if one is in use.
    #[cfg(test)]
    pub(crate) fn imported_theme(&self) -> Option<&str> {
        self.imported_theme.as_deref()
    }

    /// Why the last import failed.
    #[cfg(test)]
    pub(crate) fn import_error(&self) -> Option<&str> {
        self.import_error.as_deref()
    }

    /// The pickers, for tests to drive.
    #[cfg(test)]
    pub(crate) fn selects(&self) -> [Entity<Select>; 3] {
        [
            self.theme_mode.clone(),
            self.density.clone(),
            self.reading_pane.clone(),
        ]
    }

    /// Ask for a theme file and import it.
    fn pick_theme_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                // Cancelled, or the picker went away.
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |_, cx| {
                        cx.emit(SettingsEvent::Error(format!(
                            "The file picker could not open: {error}"
                        )));
                    });
                    None
                }
            };
            if let Some(path) = path {
                let _ = this.update(cx, |this, cx| this.import_theme_from(&path, cx));
            }
        })
        .detach();
    }

    /// Import the Zed theme family in `path`. A file that does not import
    /// leaves the current theme and says why.
    pub(crate) fn import_theme_from(&mut self, path: &Path, cx: &mut Context<Self>) {
        let imported = read_theme_file(path).and_then(|json| {
            let theme = import_theme_family(&json).map_err(|error| error.to_string())?;
            SettingsHost::get(cx)
                .store
                .set_custom_theme(Some(&json))
                .map_err(|error| format!("The theme could not be saved: {error:#}"))?;
            Ok(theme)
        });
        match imported {
            Ok(theme) => {
                self.import_error = None;
                self.imported_theme = Some(theme.name.clone());
                apply_to_workspace(cx);
                cx.emit(SettingsEvent::Status(format!(
                    "Now using the theme \u{201c}{}\u{201d}.",
                    theme.name
                )));
            }
            Err(error) => self.import_error = Some(error),
        }
        cx.notify();
    }

    /// Go back to the bundled themes.
    pub(crate) fn use_default_theme(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = SettingsHost::get(cx).store.set_custom_theme(None) {
            cx.emit(SettingsEvent::Error(format!(
                "Could not reset the theme: {error:#}"
            )));
            return;
        }
        self.imported_theme = None;
        self.import_error = None;
        apply_to_workspace(cx);
        cx.notify();
    }
}

/// Saves one preference and applies it to every window.
fn save(cx: &mut Context<AppearanceSettings>, what: &str, change: impl FnOnce(&mut UiPreferences)) {
    match SettingsHost::get(cx).store.update_ui_preferences(change) {
        Ok(_) => apply_to_workspace(cx),
        Err(error) => cx.emit(SettingsEvent::Error(format!(
            "Could not save {what}: {error:#}"
        ))),
    }
}

/// Reads a theme file as text, refusing anything too large to be a theme
/// before reading it.
fn read_theme_file(path: &Path) -> Result<String, String> {
    let name = file_name(path);
    let size = std::fs::metadata(path)
        .map_err(|error| format!("{name} could not be read: {error}."))?
        .len();
    if size > MAX_THEME_FILE_BYTES as u64 {
        return Err(format!(
            "{name} is {} MB, which is too large to be a theme.",
            size / (1024 * 1024)
        ));
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("{name} could not be read: {error}."))?;
    String::from_utf8(bytes)
        .map_err(|_| format!("{name} is not a text file, so it cannot be a Zed theme."))
}

fn file_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf());
    format!("\u{201c}{}\u{201d}", name.display())
}

impl Render for AppearanceSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let theme_description: SharedString = match &self.imported_theme {
            Some(name) => format!(
                "Using \u{201c}{name}\u{201d}, imported from a Zed theme file. Light and dark follow the Theme setting."
            )
            .into(),
            None => "Using Zmail's own theme. Import a Zed theme family file (.json) to use its colours.".into(),
        };
        div()
            .flex()
            .flex_col()
            .child(section_heading("Appearance"))
            .child(setting_row(
                "Theme",
                Some("Match System follows your system's light or dark appearance.".into()),
                self.theme_mode.clone(),
                &colors,
            ))
            .child(setting_row(
                "Message list",
                Some("Comfortable rows show a preview line; compact rows fit more mail.".into()),
                self.density.clone(),
                &colors,
            ))
            .child(setting_row(
                "Reading pane",
                Some(
                    "Off opens a message in place of the list, with a Back button to return."
                        .into(),
                ),
                self.reading_pane.clone(),
                &colors,
            ))
            .child(group_heading("Theme file", &colors))
            .child(setting_row(
                self.imported_theme
                    .clone()
                    .unwrap_or_else(|| "Zmail".to_owned()),
                Some(theme_description),
                div()
                    .flex()
                    .gap(px(SPACE_2))
                    .when(self.imported_theme.is_some(), |this| {
                        this.child(
                            Button::new("use-default-theme", "Use Default Theme")
                                .on_click(cx.listener(|this, _, _, cx| this.use_default_theme(cx))),
                        )
                    })
                    .child(Button::new("import-theme", "Import Theme…").on_click(
                        cx.listener(|this, _, window, cx| this.pick_theme_file(window, cx)),
                    )),
                &colors,
            ))
            .when_some(self.import_error.clone(), |this, error| {
                this.child(
                    div().pt(px(SPACE_2)).child(
                        Banner::new("theme-import-error", error)
                            .tone(BannerTone::Danger)
                            .title("The theme was not imported"),
                    ),
                )
            })
    }
}
