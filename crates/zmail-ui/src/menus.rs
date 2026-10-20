//! The application's command menus.
//!
//! One [`MenuModel`] lists every command a person would look for in a
//! menu, grouped the way desktop mail clients group them (File, Edit,
//! View, Mailbox, Message, plus the macOS application and Window menus).
//! It renders two ways, chosen by [`PlatformStyle::menu`]:
//!
//! - macOS: the native menu bar, through `cx.set_menus` ([`init`]). AppKit
//!   shows each item's key equivalent from the keymap and greys out items
//!   whose action nothing in the focused pane handles.
//! - Windows and Linux: the app-menu button at the end of the title bar
//!   ([`crate::app_menu::AppMenuButton`]), which opens the same model with
//!   the existing `Menu` component.
//!
//! Every entry dispatches an existing action, so a menu entry, its key
//! binding and any button behave the same. The tests check that every
//! `zmail::` action is either in a menu or listed in
//! [`ACTIONS_NOT_IN_MENUS`] with the reason, so a new command cannot be
//! left out by accident.
//!
//! The Help menu lists Keyboard Shortcuts; About and Diagnostics join it
//! with the views those entries open.

use gpui::{
    Action, App, KeyBinding, KeyContext, Keymap, Menu as NativeMenu, MenuItem as NativeMenuItem,
    OsAction, SystemMenuType, Window,
};
use zmail_core::APP_NAME;

use crate::actions::{
    AddAccount, AddAttachment, Archive, Delete, FocusSearch, Forward, GoBack, Hide, HideOthers,
    Minimize, NewMessage, Quit, Reply, ReplyAll, RetryOutbox, SaveAttachments, SaveDraft,
    SearchAllAccounts, SearchInAccount, SearchInFolder, Send, ShowAll, ShowBcc, ShowCc, ShowDrafts,
    ShowKeyboardShortcuts, ShowMail, ShowOutbox, SyncAccount, SyncAll, ToggleCommandPalette,
    ToggleDensity, ToggleFlag, ToggleRead, ToggleSidebar, ToggleTheme, Undo, Zoom,
};
use crate::components::text_input;
use crate::styles::{MenuPlacement, Platform, PlatformStyle};

/// One command in a menu.
pub(crate) struct Command {
    /// The menu label.
    pub(crate) label: &'static str,
    /// The action the entry dispatches.
    pub(crate) action: Box<dyn Action>,
    /// The standard macOS editing command this maps to, so AppKit treats
    /// the item as Cut, Copy, Paste or Select All.
    pub(crate) os_action: Option<OsAction>,
    /// Handled app-wide by [`init`] rather than by a view, so it is always
    /// available whatever has focus.
    pub(crate) app_wide: bool,
}

/// A row of a menu.
pub(crate) enum MenuModelItem {
    /// A command.
    Command(Command),
    /// A dividing line.
    Separator,
}

/// One top-level menu (File, Edit, …).
pub(crate) struct MenuSection {
    /// The menu title.
    pub(crate) name: &'static str,
    /// The rows, in order.
    pub(crate) items: Vec<MenuModelItem>,
}

/// Every top-level menu, in menu-bar order.
pub(crate) struct MenuModel {
    /// The menus.
    pub(crate) sections: Vec<MenuSection>,
}

/// `zmail::` actions that are deliberately in no menu, and why.
#[cfg(test)]
pub(crate) const ACTIONS_NOT_IN_MENUS: &[(&str, &str)] = &[
    ("zmail::SelectNext", "list navigation key"),
    ("zmail::SelectPrevious", "list navigation key"),
    ("zmail::SelectFirst", "list navigation key"),
    ("zmail::SelectLast", "list navigation key"),
    ("zmail::OpenSelected", "list navigation key"),
    ("zmail::ExtendSelectionDown", "list navigation key"),
    ("zmail::ExtendSelectionUp", "list navigation key"),
    (
        "zmail::SelectAll",
        "Edit → Select All's key, while the message list has focus",
    ),
    ("zmail::ScrollUp", "reader scrolling key"),
    ("zmail::ScrollDown", "reader scrolling key"),
    ("zmail::PageUp", "reader scrolling key"),
    ("zmail::PageDown", "reader scrolling key"),
    ("zmail::NextMessage", "reader navigation key"),
    ("zmail::PreviousMessage", "reader navigation key"),
    ("zmail::ToggleMessage", "reader navigation key"),
    (
        "zmail::FocusNextPane",
        "keyboard focus navigation (Tab, F6)",
    ),
    (
        "zmail::FocusPreviousPane",
        "keyboard focus navigation (Tab, F6)",
    ),
    ("zmail::Cancel", "Escape closes whatever is on top"),
    (
        "zmail::ContinueSetup",
        "the add-account flow's own Continue button",
    ),
    (
        "zmail::GoToInbox",
        "only the Gmail keymap binds it (g i), and a plain-key sequence is no menu key \
         equivalent; the sidebar and the palette's Go to Inbox entries cover it",
    ),
    (
        "zmail::MoveToMailbox",
        "needs a mailbox; the reader's More menu opens the Move to… picker",
    ),
    (
        "zmail::ReplyToMessage",
        "needs a message; Message → Reply replies to the latest",
    ),
    ("zmail::RemoveAttachment", "needs an attachment argument"),
    ("zmail::SelectIdentity", "needs an identity argument"),
    (
        "zmail::OpenAttachment",
        "the Open button on one attachment card",
    ),
    (
        "zmail::SaveAttachmentAs",
        "the Save As button on one attachment card",
    ),
    (
        "zmail::SaveMessageAttachments",
        "Save Attachments in one message's More menu",
    ),
    (
        "zmail::DismissSenderWarning",
        "the Looks safe button on one message's sender-safety banner",
    ),
];

/// `zmail::` actions only the macOS menus list: the system conventions
/// the window manager covers elsewhere.
#[cfg(test)]
pub(crate) const MACOS_ONLY_ACTIONS: &[&str] = &[
    "zmail::Hide",
    "zmail::HideOthers",
    "zmail::ShowAll",
    "zmail::Minimize",
    "zmail::Zoom",
];

/// A command entry.
fn command(label: &'static str, action: impl Action) -> MenuModelItem {
    MenuModelItem::Command(Command {
        label,
        action: Box::new(action),
        os_action: None,
        app_wide: false,
    })
}

/// A standard editing command entry.
fn os_command(label: &'static str, action: impl Action, os_action: OsAction) -> MenuModelItem {
    MenuModelItem::Command(Command {
        label,
        action: Box::new(action),
        os_action: Some(os_action),
        app_wide: false,
    })
}

/// A command [`init`] handles app-wide.
fn app_command(label: &'static str, action: impl Action) -> MenuModelItem {
    MenuModelItem::Command(Command {
        label,
        action: Box::new(action),
        os_action: None,
        app_wide: true,
    })
}

impl MenuModel {
    /// The menus for `platform`. macOS gets the application menu (Hide,
    /// Quit) and a Window menu; elsewhere Quit closes the File menu and the
    /// title bar's own buttons minimise and maximise.
    pub(crate) fn new(platform: Platform) -> Self {
        let macos = platform == Platform::MacOs;
        let mut sections = Vec::new();

        if macos {
            sections.push(MenuSection {
                name: APP_NAME,
                items: vec![
                    app_command("Hide Zmail", Hide),
                    app_command("Hide Others", HideOthers),
                    app_command("Show All", ShowAll),
                    MenuModelItem::Separator,
                    app_command("Quit Zmail", Quit),
                ],
            });
        }

        let mut file = vec![
            command("New Message", NewMessage),
            command("Add Account…", AddAccount),
        ];
        if !macos {
            file.extend([MenuModelItem::Separator, app_command("Quit", Quit)]);
        }
        sections.push(MenuSection {
            name: "File",
            items: file,
        });

        sections.push(MenuSection {
            name: "Edit",
            items: vec![
                // Undoes the last mail change (archive, delete, move, flag,
                // read); text fields keep their own Undo on the same key.
                os_command("Undo", Undo, OsAction::Undo),
                MenuModelItem::Separator,
                os_command("Cut", text_input::Cut, OsAction::Cut),
                os_command("Copy", text_input::Copy, OsAction::Copy),
                os_command("Paste", text_input::Paste, OsAction::Paste),
                os_command("Select All", text_input::SelectAll, OsAction::SelectAll),
                MenuModelItem::Separator,
                command("Search Mail", FocusSearch),
                command("Search This Folder", SearchInFolder),
                command("Search This Account", SearchInAccount),
                command("Search All Accounts", SearchAllAccounts),
            ],
        });

        sections.push(MenuSection {
            name: "View",
            items: vec![
                command("Command Palette…", ToggleCommandPalette),
                MenuModelItem::Separator,
                command("Mail", ShowMail),
                command("Drafts", ShowDrafts),
                command("Outbox", ShowOutbox),
                MenuModelItem::Separator,
                command("Show/Hide Sidebar", ToggleSidebar),
                command("Back to List", GoBack),
                command("Switch Light/Dark Theme", ToggleTheme),
                command("Switch Comfortable/Compact Rows", ToggleDensity),
            ],
        });

        sections.push(MenuSection {
            name: "Mailbox",
            items: vec![
                command("Sync Account", SyncAccount),
                command("Sync All Accounts", SyncAll),
                MenuModelItem::Separator,
                command("Retry Sending", RetryOutbox),
            ],
        });

        sections.push(MenuSection {
            name: "Message",
            items: vec![
                command("Reply", Reply),
                command("Reply All", ReplyAll),
                command("Forward", Forward),
                MenuModelItem::Separator,
                command("Archive", Archive),
                command("Delete", Delete),
                MenuModelItem::Separator,
                command("Flag / Unflag", ToggleFlag),
                command("Mark Read / Unread", ToggleRead),
                MenuModelItem::Separator,
                command("Save Attachments…", SaveAttachments),
                MenuModelItem::Separator,
                command("Send", Send),
                command("Save Draft", SaveDraft),
                command("Attach Files…", AddAttachment),
                command("Add Cc", ShowCc),
                command("Add Bcc", ShowBcc),
            ],
        });

        if macos {
            sections.push(MenuSection {
                name: "Window",
                items: vec![app_command("Minimize", Minimize), app_command("Zoom", Zoom)],
            });
        }

        sections.push(MenuSection {
            name: "Help",
            items: vec![command("Keyboard Shortcuts", ShowKeyboardShortcuts)],
        });

        Self { sections }
    }

    /// Every command, in menu order.
    #[cfg(test)]
    pub(crate) fn commands(&self) -> impl Iterator<Item = &Command> {
        self.sections
            .iter()
            .flat_map(|section| &section.items)
            .filter_map(|item| match item {
                MenuModelItem::Command(command) => Some(command),
                MenuModelItem::Separator => None,
            })
    }

    /// The model as GPUI menus for `cx.set_menus`. The application menu
    /// also gets the system Services submenu.
    pub(crate) fn native_menus(&self) -> Vec<NativeMenu> {
        self.sections
            .iter()
            .map(|section| {
                let mut items: Vec<NativeMenuItem> = section
                    .items
                    .iter()
                    .map(|item| match item {
                        MenuModelItem::Command(command) => NativeMenuItem::Action {
                            name: command.label.into(),
                            action: command.action.boxed_clone(),
                            os_action: command.os_action,
                            checked: false,
                            disabled: false,
                        },
                        MenuModelItem::Separator => NativeMenuItem::separator(),
                    })
                    .collect();
                if section.name == APP_NAME {
                    items.splice(
                        0..0,
                        [
                            NativeMenuItem::os_submenu("Services", SystemMenuType::Services),
                            NativeMenuItem::separator(),
                        ],
                    );
                }
                NativeMenu::new(section.name).items(items)
            })
            .collect()
    }
}

/// The key binding a menu shows for `action`, by the rule the macOS menu
/// bar uses: the first binding that applies at the workspace level, or else
/// the action's first binding. The app-menu button shows the same one, so
/// both platforms advertise the same keys.
pub(crate) fn menu_binding<'a>(
    keymap: &'a Keymap,
    action: &'a dyn Action,
) -> Option<&'a KeyBinding> {
    let workspace = workspace_context();
    let mut bindings = keymap.bindings_for_action(action).peekable();
    let first = *bindings.peek()?;
    bindings
        .find(|binding| {
            binding
                .predicate()
                .is_none_or(|predicate| predicate.eval(std::slice::from_ref(&workspace)))
        })
        .or(Some(first))
}

/// The key context a menu is evaluated in: just the window root.
fn workspace_context() -> KeyContext {
    let mut context = KeyContext::new_with_defaults();
    context.add(crate::workspace::KEY_CONTEXT);
    context
}

/// A binding's keystrokes in GPUI syntax (`cmd-shift-r`), which
/// [`Kbd`](crate::components::Kbd) labels for the platform.
pub(crate) fn binding_label(binding: &KeyBinding) -> String {
    binding
        .keystrokes()
        .iter()
        .map(|keystroke| keystroke.unparse())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Registers the app-wide command handlers and, on macOS, the native menu
/// bar. Call after the keymap is bound: the menu bar reads its key
/// equivalents from the keymap when it is set.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| update_active_window(cx, Window::minimize_window));
    cx.on_action(|_: &Zoom, cx| update_active_window(cx, Window::zoom_window));
    refresh_menu_bar(cx);
}

/// Sets the macOS menu bar from the menu model, which also reads each
/// item's key equivalent from the keymap: call it again whenever the keymap
/// changes. Other platforms' menu buttons read the keymap when they open.
pub(crate) fn refresh_menu_bar(cx: &mut App) {
    if PlatformStyle::current().menu == MenuPlacement::MenuBar {
        cx.set_menus(MenuModel::new(Platform::current()).native_menus());
    }
}

fn update_active_window(cx: &mut App, update: fn(&Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| update(window)).ok();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gpui::TestAppContext;

    use super::*;
    use crate::keymap::{DEFAULT_KEYMAP, GMAIL_KEYMAP, MACOS_KEYMAP, parse_keymap};

    const PLATFORMS: [Platform; 3] = [Platform::MacOs, Platform::Windows, Platform::Linux];

    /// A keymap with the default, macOS and text input bindings.
    fn keymap(cx: &mut TestAppContext) -> Keymap {
        cx.update(|cx| {
            let mut bindings = parse_keymap(DEFAULT_KEYMAP, cx).expect("the keymap parses");
            bindings.extend(parse_keymap(MACOS_KEYMAP, cx).expect("the macOS keymap parses"));
            bindings.extend(text_input::key_bindings());
            Keymap::new(bindings)
        })
    }

    #[gpui::test]
    fn every_action_is_in_a_menu_or_listed_as_not(cx: &mut TestAppContext) {
        let registered = cx.update(|cx| {
            cx.all_action_names()
                .iter()
                .copied()
                .filter(|name| name.starts_with("zmail::"))
                .collect::<BTreeSet<_>>()
        });
        for platform in PLATFORMS {
            let model = MenuModel::new(platform);
            let listed = model
                .commands()
                .map(|command| command.action.name())
                .collect::<BTreeSet<_>>();
            let missing = registered
                .iter()
                .filter(|name| {
                    !listed.contains(*name)
                        && !ACTIONS_NOT_IN_MENUS
                            .iter()
                            .any(|(left_out, _)| left_out == *name)
                        && (platform == Platform::MacOs || !MACOS_ONLY_ACTIONS.contains(*name))
                })
                .collect::<Vec<_>>();
            assert!(
                missing.is_empty(),
                "{platform:?}: actions in no menu: {missing:?}"
            );
            for (left_out, _) in ACTIONS_NOT_IN_MENUS {
                assert!(
                    !listed.contains(left_out),
                    "{left_out} is in a menu but listed in ACTIONS_NOT_IN_MENUS"
                );
            }
            for name in &listed {
                assert!(
                    !name.starts_with("zmail::") || registered.contains(name),
                    "{name} is not a registered action"
                );
            }
        }
    }

    #[gpui::test]
    fn sections_follow_the_plan_order(_cx: &mut TestAppContext) {
        let names = |platform| {
            MenuModel::new(platform)
                .sections
                .iter()
                .map(|section| section.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(Platform::MacOs),
            [
                "Zmail", "File", "Edit", "View", "Mailbox", "Message", "Window", "Help"
            ]
        );
        for platform in [Platform::Windows, Platform::Linux] {
            assert_eq!(
                names(platform),
                ["File", "Edit", "View", "Mailbox", "Message", "Help"]
            );
        }
    }

    #[gpui::test]
    fn every_command_shows_a_shortcut_unless_it_has_none(cx: &mut TestAppContext) {
        let keymap = keymap(cx);
        for platform in PLATFORMS {
            for command in MenuModel::new(platform).commands() {
                let name = command.action.name();
                let unbound = crate::actions::ACTIONS_WITHOUT_KEYS
                    .iter()
                    .any(|(unbound, _)| *unbound == name);
                let binding = menu_binding(&keymap, command.action.as_ref());
                assert_eq!(
                    binding.is_some(),
                    !unbound,
                    "{platform:?} {}: shortcut {:?}",
                    command.label,
                    binding.map(binding_label)
                );
            }
        }
    }

    #[gpui::test]
    fn menu_shortcuts_are_safe_as_macos_key_equivalents(cx: &mut TestAppContext) {
        // AppKit fires a menu item's key equivalent whenever GPUI leaves
        // the keystroke unhandled, from any pane. A plain key there (Delete
        // as `backspace`) would run the command from panes that never bound
        // it, so the shown binding must be a chord or already apply at the
        // workspace level.
        let keymap = keymap(cx);
        let workspace = workspace_context();
        for command in MenuModel::new(Platform::MacOs).commands() {
            let Some(binding) = menu_binding(&keymap, command.action.as_ref()) else {
                continue;
            };
            let workspace_wide = binding
                .predicate()
                .is_none_or(|predicate| predicate.eval(std::slice::from_ref(&workspace)));
            let chorded = binding.keystrokes().iter().all(|keystroke| {
                let modifiers = keystroke.modifiers();
                modifiers.platform || modifiers.control || modifiers.alt
            });
            assert!(
                chorded || workspace_wide,
                "{} shows the plain key {}",
                command.label,
                binding_label(binding)
            );
        }
    }

    #[gpui::test]
    fn the_gmail_keymap_keeps_menu_shortcuts_chorded(cx: &mut TestAppContext) {
        // The Gmail layer loads after the standard keymap, so every menu
        // still shows (and on macOS uses as its key equivalent) the
        // standard chord, never a plain letter AppKit would fire from any
        // pane.
        let standard = keymap(cx);
        let gmail = cx.update(|cx| {
            let mut bindings = standard.bindings().cloned().collect::<Vec<_>>();
            bindings.extend(parse_keymap(GMAIL_KEYMAP, cx).expect("the Gmail keymap parses"));
            Keymap::new(bindings)
        });
        for platform in PLATFORMS {
            for command in MenuModel::new(platform).commands() {
                let label = |keymap: &Keymap| {
                    menu_binding(keymap, command.action.as_ref()).map(binding_label)
                };
                assert_eq!(
                    label(&gmail),
                    label(&standard),
                    "{platform:?} {}",
                    command.label
                );
            }
        }
    }

    #[gpui::test]
    fn shortcuts_match_the_keymap(cx: &mut TestAppContext) {
        let keymap = keymap(cx);
        let label = |action: &dyn Action| {
            menu_binding(&keymap, action)
                .map(binding_label)
                .unwrap_or_default()
        };
        let secondary = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        assert_eq!(label(&NewMessage), format!("{secondary}-n"));
        assert_eq!(label(&Delete), format!("{secondary}-backspace"));
        assert_eq!(label(&Reply), format!("{secondary}-r"));
        assert_eq!(label(&Quit), format!("{secondary}-q"));
        assert_eq!(label(&SyncAccount), "f5");
        assert_eq!(label(&text_input::Copy), format!("{secondary}-c"));
        assert_eq!(label(&ShowAll), "");
        // Cmd-H whatever platform runs the test (unparse spells the
        // platform key per OS).
        let hide = menu_binding(&keymap, &Hide).expect("Hide has a key");
        let keystroke = &hide.keystrokes()[0];
        assert!(keystroke.modifiers().platform && keystroke.key() == "h");
    }

    #[gpui::test]
    fn native_menus_mirror_the_model(_cx: &mut TestAppContext) {
        let model = MenuModel::new(Platform::MacOs);
        let menus = model.native_menus();
        assert_eq!(menus.len(), model.sections.len());
        let app_menu = &menus[0];
        assert_eq!(app_menu.name.as_ref(), "Zmail");
        assert!(matches!(app_menu.items[0], NativeMenuItem::SystemMenu(_)));
        let edit = menus
            .iter()
            .find(|menu| menu.name.as_ref() == "Edit")
            .expect("an Edit menu");
        let copy = edit
            .items
            .iter()
            .find_map(|item| match item {
                NativeMenuItem::Action {
                    name, os_action, ..
                } if name.as_ref() == "Copy" => Some(*os_action),
                _ => None,
            })
            .expect("a Copy item");
        assert!(copy == Some(OsAction::Copy));
    }
}
