//! Settings → Keyboard: the keymap choice (Standard, or Standard plus the
//! Gmail keys) and a searchable list of every shortcut.
//!
//! Switching goes through [`keymap::use_keymap`], which rebinds at once
//! (no restart) and saves the choice. The list is the cheat sheet's
//! ([`cheat_sheet::shortcut_groups`]): it reads the bound keymap, so it
//! follows a switch and any binding added later. The search keeps the rows
//! whose label, group or keys contain every word typed.

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, Render, Role, SharedString,
    Subscription, Window, div, prelude::*, px, rgb,
};
use zmail_core::preferences::KeymapName;

use super::{SettingsEvent, SettingsHost, group_heading, section_heading, setting_row};
use crate::a11y::AccessibleExt;
use crate::cheat_sheet::{ShortcutGroup, ShortcutRow, action_label, shortcut_groups};
use crate::components::kbd::{keystrokes_label, spoken_keystrokes};
use crate::components::{ComponentTheme, Kbd, Select, SelectEvent, TextInput, TextInputEvent};
use crate::keymap::{active_keymap, use_keymap};
use crate::menus::MenuModel;
use crate::styles::{Platform, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TYPE_BODY, TypeStyleExt};

/// The keymaps, in picker order.
const KEYMAPS: [(KeymapName, &str); 2] = [
    (KeymapName::Default, "Standard"),
    (KeymapName::Gmail, "Gmail"),
];

/// The Keyboard section.
pub(crate) struct KeyboardSettings {
    keymap: Entity<Select>,
    search: Entity<TextInput>,
    groups: Vec<ShortcutGroup>,
    _subscriptions: [Subscription; 2],
}

impl KeyboardSettings {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let keymap = cx.new(|cx| {
            Select::new(KEYMAPS.map(|(_, label)| label), None, cx)
                .label("Keyboard shortcuts")
                .width(px(200.0))
        });
        let search = cx.new(|cx| {
            TextInput::new(cx)
                .compact()
                .leading_icon("search")
                .label("Search shortcuts")
                .placeholder("Search shortcuts")
        });
        let subscriptions = [
            cx.subscribe(&keymap, |this, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = *event;
                this.switch_keymap(KEYMAPS[index].0, cx);
            }),
            cx.subscribe(&search, |_, _, event: &TextInputEvent, cx| {
                if *event == TextInputEvent::Changed {
                    cx.notify();
                }
            }),
        ];
        let mut this = Self {
            keymap,
            search,
            groups: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        this
    }

    /// Show the bound keymap and its shortcuts.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let active = active_keymap(cx);
        self.keymap.update(cx, |select, cx| {
            select.set_selected(KEYMAPS.iter().position(|(keymap, _)| *keymap == active), cx)
        });
        self.groups = bound_shortcuts(cx);
        cx.notify();
    }

    fn switch_keymap(&mut self, keymap: KeymapName, cx: &mut Context<Self>) {
        let store = SettingsHost::get(cx).store.clone();
        if let Err(error) = use_keymap(&store, keymap, cx) {
            cx.emit(SettingsEvent::Error(format!("{error:#}")));
        }
        self.reload(cx);
    }

    /// The keymap picker, for tests to drive.
    #[cfg(test)]
    pub(crate) fn keymap_select(&self) -> &Entity<Select> {
        &self.keymap
    }

    /// The search field.
    #[cfg(test)]
    pub(crate) fn search(&self) -> &Entity<TextInput> {
        &self.search
    }

    /// The rows the search leaves, by group.
    pub(crate) fn visible(&self, cx: &App) -> Vec<(&str, Vec<&ShortcutRow>)> {
        let platform = ComponentTheme::get(cx).platform.platform;
        let query = self.search.read(cx).text().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        self.groups
            .iter()
            .map(|group| {
                let rows = group
                    .rows
                    .iter()
                    .filter(|row| {
                        let haystack = format!(
                            "{} {} {} {}",
                            row.label,
                            group.name,
                            row.keys.join(" "),
                            row.keys
                                .iter()
                                .map(|keys| keystrokes_label(keys, platform))
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                        .to_lowercase();
                        words.iter().all(|word| haystack.contains(word))
                    })
                    .collect::<Vec<_>>();
                (group.name.as_str(), rows)
            })
            .filter(|(_, rows)| !rows.is_empty())
            .collect()
    }
}

/// The bound keymap's shortcuts, labelled as the cheat sheet labels them.
fn bound_shortcuts(cx: &App) -> Vec<ShortcutGroup> {
    let menu = MenuModel::new(Platform::current());
    let documentation = cx.action_documentation();
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    shortcut_groups(&keymap, |name| action_label(&menu, documentation, name))
}

impl Render for KeyboardSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let (colors, platform) = (theme.colors, theme.platform.platform);
        let rows: Vec<AnyElement> =
            self.visible(cx)
                .into_iter()
                .enumerate()
                .flat_map(|(group_index, (name, rows))| {
                    std::iter::once(group_heading(name, &colors).into_any_element()).chain(
                        rows.into_iter().enumerate().map(move |(row_index, row)| {
                            let keys = row
                                .keys
                                .iter()
                                .map(|keys| spoken_keystrokes(keys, platform))
                                .collect::<Vec<_>>()
                                .join(" or ");
                            div()
                                .id(SharedString::from(format!(
                                    "settings-shortcut-{group_index}-{row_index}"
                                )))
                                .accessible(Role::ListItem, format!("{}: {keys}", row.label))
                                .flex()
                                .items_center()
                                .gap(px(SPACE_3))
                                .py(px(SPACE_2))
                                .border_b_1()
                                .border_color(rgb(colors.border))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .type_style(TYPE_BODY)
                                        .child(row.label.clone()),
                                )
                                .child(
                                    div().flex().flex_none().gap(px(SPACE_1)).children(
                                        row.keys.iter().map(|keys| Kbd::new(keys.clone())),
                                    ),
                                )
                                .into_any_element()
                        }),
                    )
                })
                .collect();
        let empty = rows.is_empty();
        div()
            .flex()
            .flex_col()
            .child(section_heading("Keyboard"))
            .child(setting_row(
                "Shortcuts",
                Some(
                    "Gmail adds single-key shortcuts such as e to archive, j and k to move, and ? for the cheat sheet. The standard shortcuts keep working."
                        .into(),
                ),
                self.keymap.clone(),
                &colors,
            ))
            .child(
                div()
                    .pt(px(SPACE_4))
                    .pb(px(SPACE_2))
                    .child(self.search.clone()),
            )
            .children(rows)
            .when(empty, |this| {
                this.child(
                    div()
                        .py(px(SPACE_3))
                        .text_color(rgb(colors.muted))
                        .child("No shortcut matches."),
                )
            })
    }
}
