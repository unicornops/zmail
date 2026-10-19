//! The keyboard shortcut cheat sheet (`?` in the Gmail keymap, Secondary+/
//! in both, Help → Keyboard Shortcuts).
//!
//! Nothing here is a list of keys: [`shortcut_groups`] reads the bound
//! keymap, so the sheet always shows the active keymap (standard or Gmail)
//! and stays right as bindings change. Each binding is placed by where it
//! applies, found by evaluating its context against the key context stack
//! of each place in the window:
//!
//! - bindings that apply on the bare window (`Workspace`, or no context)
//!   are **General**;
//! - the rest are grouped by the set of panes they apply in ("Message
//!   list", "Message list and reader", "Compose", …).
//!
//! Bindings for text fields, menus and other components never match those
//! stacks, so text editing keys stay out. A command is labelled with its
//! menu label when it has one, else the first phrase of its action's doc
//! comment.

use std::collections::HashMap;
use std::hash::BuildHasher;

use gpui::{
    AnyElement, Context, KeyContext, Keymap, Role, SharedString, Window, div, prelude::*, px, rgb,
};
use zmail_core::preferences::KeymapName;

use crate::WorkspaceView;
use crate::actions::ShowKeyboardShortcuts;
use crate::components::kbd::keystrokes_label;
use crate::components::{Button, ComponentTheme, Kbd, Modal};
use crate::keymap::active_keymap;
use crate::menus::{MenuModel, binding_label};
use crate::styles::{
    Platform, SPACE_1, SPACE_2, SPACE_3, SPACE_8, TYPE_BODY, TYPE_CAPTION, TypeStyleExt,
};
use crate::widgets::EllipsisExt;

/// Width of the cheat sheet: two columns of groups.
const SHEET_WIDTH: f32 = 760.0;

/// Width of one column of groups.
const GROUP_WIDTH: f32 = 330.0;

/// The places a binding can apply, as the key context stack focus has
/// there (outermost first), in the order the sheet lists them.
const PLACES: &[(&str, &[&str])] = &[
    ("Message list", &["Workspace", "MessageList"]),
    ("Reader", &["Workspace", "Reader"]),
    ("Sidebar", &["Workspace", "Sidebar"]),
    ("Compose", &["ComposeWindow", "Compose"]),
    ("Account setup", &["Workspace", "AccountSetup"]),
    ("Command palette", &["Workspace", "CommandPalette"]),
];

/// The stack of the bare window: what General means.
const GENERAL: &[&str] = &["Workspace"];

/// One command and its keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShortcutRow {
    /// What the command does.
    pub(crate) label: String,
    /// Its keystrokes in GPUI syntax (for [`Kbd`]), in keymap order.
    pub(crate) keys: Vec<String>,
}

/// The shortcuts that apply in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShortcutGroup {
    /// "General", "Message list", "Message list and reader", …
    pub(crate) name: String,
    pub(crate) rows: Vec<ShortcutRow>,
}

fn stack(contexts: &[&str]) -> Vec<KeyContext> {
    contexts
        .iter()
        .map(|name| {
            let mut context = KeyContext::new_with_defaults();
            context.add(*name);
            context
        })
        .collect()
}

/// "Message list", "Message list and reader", "Sidebar, message list and
/// reader".
fn group_name(places: &[usize]) -> String {
    let names = places
        .iter()
        .enumerate()
        .map(|(position, &place)| {
            let name = PLACES[place].0;
            if position == 0 {
                name.to_owned()
            } else {
                name.to_lowercase()
            }
        })
        .collect::<Vec<_>>();
    match names.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The label of an action without a menu entry: the first phrase of its
/// doc comment ("Select the next row.").
fn doc_label(documentation: &str) -> String {
    let text = documentation
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let end = text
        .find(". ")
        .or_else(|| text.find(": "))
        .or_else(|| text.find(" ("))
        .unwrap_or(text.len());
    text[..end].trim_end_matches('.').to_owned()
}

/// The keymap's shortcuts, grouped by where they apply. `label` names an
/// action (by its registered name), or returns `None` to leave it out.
pub(crate) fn shortcut_groups(
    keymap: &Keymap,
    label: impl Fn(&str) -> Option<String>,
) -> Vec<ShortcutGroup> {
    let general = stack(GENERAL);
    let places = PLACES
        .iter()
        .map(|(_, contexts)| stack(contexts))
        .collect::<Vec<_>>();

    // (where it applies, label) -> keys, in keymap order.
    let mut groups: Vec<(Vec<usize>, Vec<ShortcutRow>)> = Vec::new();
    for binding in keymap.bindings() {
        let applies = |contexts: &[KeyContext]| {
            binding
                .predicate()
                .is_none_or(|predicate| predicate.eval(contexts))
        };
        let key = if applies(&general) {
            Vec::new()
        } else {
            let matched = places
                .iter()
                .enumerate()
                .filter(|(_, contexts)| applies(contexts))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if matched.is_empty() {
                continue;
            }
            matched
        };
        let Some(label) = label(binding.action().name()) else {
            continue;
        };
        let keystrokes = binding_label(binding);
        let rows = match groups.iter_mut().find(|(places, _)| *places == key) {
            Some((_, rows)) => rows,
            None => {
                groups.push((key, Vec::new()));
                &mut groups.last_mut().expect("just pushed").1
            }
        };
        match rows.iter_mut().find(|row| row.label == label) {
            Some(row) if !row.keys.contains(&keystrokes) => row.keys.push(keystrokes),
            Some(_) => {}
            None => rows.push(ShortcutRow {
                label,
                keys: vec![keystrokes],
            }),
        }
    }

    // General first, then by the first place each group applies in.
    groups.sort_by(|(left, _), (right, _)| {
        let rank =
            |places: &Vec<usize>| (places.first().map_or(0, |first| first + 1), places.len());
        rank(left).cmp(&rank(right))
    });
    groups
        .into_iter()
        .map(|(places, rows)| ShortcutGroup {
            name: if places.is_empty() {
                "General".to_owned()
            } else {
                group_name(&places)
            },
            rows,
        })
        .collect()
}

/// The labels the sheet uses: the menu label when the action has one,
/// else the first phrase of its doc comment.
pub(crate) fn action_label<S: BuildHasher>(
    menu: &MenuModel,
    documentation: &HashMap<&'static str, &'static str, S>,
    name: &str,
) -> Option<String> {
    menu.sections
        .iter()
        .flat_map(|section| &section.items)
        .find_map(|item| match item {
            crate::menus::MenuModelItem::Command(command) if command.action.name() == name => {
                Some(command.label.trim_end_matches('…').to_owned())
            }
            _ => None,
        })
        .or_else(|| documentation.get(name).map(|doc| doc_label(doc)))
}

impl WorkspaceView {
    /// Opens the cheat sheet over the bound keymap, or closes it when it
    /// is open.
    pub(crate) fn toggle_keyboard_shortcuts(
        &mut self,
        _: &ShowKeyboardShortcuts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shortcuts_modal.is_open() {
            self.close_keyboard_shortcuts(window, cx);
            return;
        }
        let menu = MenuModel::new(Platform::current());
        let documentation = cx.action_documentation().clone();
        let keymap = cx.key_bindings();
        self.shortcut_groups = shortcut_groups(&keymap.borrow(), |name| {
            action_label(&menu, &documentation, name)
        });
        self.shortcuts_modal.open(window, cx);
        cx.notify();
    }

    fn close_keyboard_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shortcuts_modal.close(window, cx);
        self.shortcut_groups.clear();
        cx.notify();
    }

    /// The cheat sheet, while it is open.
    pub(crate) fn render_keyboard_shortcuts(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.shortcuts_modal.is_open() {
            return None;
        }
        let theme = ComponentTheme::get(cx);
        let (colors, platform) = (theme.colors, theme.platform.platform);
        let description = match active_keymap(cx) {
            KeymapName::Default => "The standard shortcuts.",
            KeymapName::Gmail => "The standard shortcuts with the Gmail keys added.",
        };
        let groups = self
            .shortcut_groups
            .iter()
            .enumerate()
            .map(|(index, group)| {
                div()
                    .id(("keyboard-shortcuts-group", index))
                    .role(Role::List)
                    .aria_label(SharedString::from(group.name.clone()))
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_1))
                    .w(px(GROUP_WIDTH))
                    .child(
                        div()
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.muted))
                            .pb(px(SPACE_1))
                            .child(group.name.clone()),
                    )
                    .children(group.rows.iter().enumerate().map(move |(row_index, row)| {
                        // Spoken as the key caps read ("⌘N", "Ctrl+N").
                        let keys = row
                            .keys
                            .iter()
                            .map(|key| keystrokes_label(key, platform))
                            .collect::<Vec<_>>()
                            .join(", ");
                        div()
                            .id(SharedString::from(format!(
                                "keyboard-shortcut-{index}-{row_index}"
                            )))
                            .role(Role::ListItem)
                            .flex()
                            .items_center()
                            .gap(px(SPACE_2))
                            .type_style(TYPE_BODY)
                            .aria_label(SharedString::from(format!("{}: {keys}", row.label)))
                            .child(div().flex_1().min_w_0().ellipsis().child(row.label.clone()))
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .gap(px(SPACE_1))
                                    .children(row.keys.iter().map(|key| Kbd::new(key.clone()))),
                            )
                    }))
            });
        Some(
            Modal::new("keyboard-shortcuts", self.shortcuts_modal.focus_handle())
                .title("Keyboard Shortcuts")
                .description(description)
                .width(px(SHEET_WIDTH))
                .child(
                    div()
                        .debug_selector(|| "keyboard-shortcuts-groups".into())
                        .flex()
                        .flex_wrap()
                        .gap_x(px(SPACE_8))
                        .gap_y(px(SPACE_3 * 2.0))
                        .children(groups),
                )
                .footer(Button::new("keyboard-shortcuts-close", "Close").on_click(
                    cx.listener(|view, _, window, cx| view.close_keyboard_shortcuts(window, cx)),
                ))
                .on_dismiss(cx.listener(|view, _, window, cx| {
                    view.close_keyboard_shortcuts(window, cx);
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::keymap::{DEFAULT_KEYMAP, GMAIL_KEYMAP, parse_keymap};

    fn groups(cx: &mut TestAppContext, sources: &[&str]) -> Vec<ShortcutGroup> {
        cx.update(|cx| {
            let mut bindings = Vec::new();
            for source in sources {
                bindings.extend(parse_keymap(source, cx).expect("the keymap parses"));
            }
            bindings.extend(crate::components::text_input::key_bindings());
            let menu = MenuModel::new(Platform::Linux);
            let documentation = cx.action_documentation().clone();
            shortcut_groups(&Keymap::new(bindings), |name| {
                action_label(&menu, &documentation, name)
            })
        })
    }

    fn group<'a>(groups: &'a [ShortcutGroup], name: &str) -> &'a ShortcutGroup {
        groups
            .iter()
            .find(|group| group.name == name)
            .unwrap_or_else(|| {
                let names = groups.iter().map(|group| &group.name).collect::<Vec<_>>();
                panic!("no {name} group in {names:?}")
            })
    }

    fn keys<'a>(group: &'a ShortcutGroup, label: &str) -> &'a [String] {
        &group
            .rows
            .iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("no {label} in {}", group.name))
            .keys
    }

    #[gpui::test]
    fn the_standard_sheet_is_grouped_by_where_keys_apply(cx: &mut TestAppContext) {
        let groups = groups(cx, &[DEFAULT_KEYMAP]);
        let secondary = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        assert_eq!(groups[0].name, "General");
        assert_eq!(keys(&groups[0], "New Message"), [format!("{secondary}-n")]);
        assert_eq!(
            keys(&groups[0], "Command Palette"),
            [format!("{secondary}-k")]
        );
        // Several keys for one command share a row.
        assert_eq!(
            keys(&groups[0], "Sync All Accounts"),
            ["shift-f5".to_owned(), format!("{secondary}-shift-n")]
        );
        let conversations = group(&groups, "Message list and reader");
        assert_eq!(
            keys(conversations, "Delete"),
            [
                format!("{secondary}-backspace"),
                "delete".to_owned(),
                "backspace".to_owned()
            ]
        );
        // Undocumented in menus: the doc comment's first phrase.
        assert_eq!(
            keys(
                group(&groups, "Reader"),
                "Scroll the conversation down a little"
            ),
            ["down"]
        );
        assert_eq!(
            keys(group(&groups, "Message list"), "Select the next row"),
            ["down"]
        );
        assert_eq!(
            keys(group(&groups, "Sidebar"), "Select the next row"),
            ["down"]
        );
        // The reader's duplicate sections list once.
        assert_eq!(
            keys(
                group(&groups, "Reader"),
                "Move to the next message of the conversation"
            ),
            ["n"]
        );
        // Text editing keys are the text field's business.
        assert!(
            !groups
                .iter()
                .any(|group| group.rows.iter().any(|row| row.label == "Paste"))
        );
        assert!(groups.iter().any(|group| group.name == "Compose"));
        assert!(groups.iter().any(|group| group.name == "Command palette"));
    }

    #[gpui::test]
    fn the_gmail_sheet_adds_the_gmail_keys(cx: &mut TestAppContext) {
        let standard = groups(cx, &[DEFAULT_KEYMAP]);
        let gmail = groups(cx, &[DEFAULT_KEYMAP, GMAIL_KEYMAP]);
        assert_ne!(standard, gmail);
        let conversations = group(&gmail, "Message list and reader");
        assert!(keys(conversations, "Archive").contains(&"e".to_owned()));
        assert!(keys(conversations, "Delete").contains(&"#".to_owned()));
        assert!(keys(conversations, "Select the next row").contains(&"j".to_owned()));
        assert_eq!(keys(&gmail[0], "Go to the inbox"), ["g i"]);
        assert!(keys(&gmail[0], "Keyboard Shortcuts").contains(&"?".to_owned()));
        assert!(
            keys(group(&gmail, "Message list, reader and sidebar"), "Undo")
                .contains(&"z".to_owned())
        );
    }

    #[test]
    fn doc_labels_take_the_first_phrase() {
        assert_eq!(doc_label("Select the next row."), "Select the next row");
        assert_eq!(
            doc_label(
                "Close the topmost overlay: the account form, the error banner,\n the status message, or search."
            ),
            "Close the topmost overlay"
        );
        assert_eq!(
            doc_label(
                "Go to the inbox: the Unified Inbox when there are several\n accounts, else the account's Inbox."
            ),
            "Go to the inbox"
        );
    }

    #[test]
    fn group_names_read_as_a_list() {
        assert_eq!(group_name(&[0]), "Message list");
        assert_eq!(group_name(&[0, 1]), "Message list and reader");
        assert_eq!(group_name(&[0, 1, 2]), "Message list, reader and sidebar");
    }
}
