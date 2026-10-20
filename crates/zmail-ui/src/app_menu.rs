//! `AppMenuButton`: the [`MenuModel`] as a button-opened menu, for Windows
//! (overflow ⋯ button) and Linux (hamburger button). macOS uses the native
//! menu bar instead (see [`crate::menus`]).
//!
//! The button opens a menu of the top-level menus (File, Edit, …); choosing
//! one replaces it with that menu's commands, each with its shortcut. A
//! command runs as if from whatever had keyboard focus when the menu
//! opened, so Copy copies from the focused text field and Reply replies
//! to the selected conversation. Commands nothing there handles are greyed
//! out, the way macOS greys out menu bar items.

use std::rc::Rc;

use gpui::{
    Context, DismissEvent, Entity, FocusHandle, Focusable, IntoElement, MouseButton, Render,
    Subscription, Window, div, prelude::*,
};

use crate::components::{IconButton, Menu, MenuEntry, popover_below_end};
use crate::menus::{MenuModel, MenuModelItem, binding_label, menu_binding};
use crate::styles::{MenuPlacement, Platform, PlatformStyle};

/// The app-menu button and its open menu.
pub(crate) struct AppMenuButton {
    model: Rc<MenuModel>,
    /// Where commands run when nothing had focus: the window root.
    root_focus: FocusHandle,
    /// What had focus when the menu opened; commands run from here, and
    /// focus returns here when the menu closes.
    restore_focus: Option<FocusHandle>,
    menu: Option<(Entity<Menu>, Subscription)>,
    /// Set by a press on the button while the menu is open: that press
    /// already closed the menu (click outside), so the click must not
    /// reopen it.
    swallow_click: bool,
}

impl AppMenuButton {
    /// A button for this platform's menu model. `root_focus` is the window
    /// root's focus handle.
    pub(crate) fn new(root_focus: FocusHandle) -> Self {
        Self::for_platform(Platform::current(), root_focus)
    }

    /// A button for `platform`'s menu model.
    pub(crate) fn for_platform(platform: Platform, root_focus: FocusHandle) -> Self {
        Self {
            model: Rc::new(MenuModel::new(platform)),
            root_focus,
            restore_focus: None,
            menu: None,
            swallow_click: false,
        }
    }

    /// The open menu, if any.
    #[cfg(test)]
    pub(crate) fn open_menu(&self) -> Option<&Entity<Menu>> {
        self.menu.as_ref().map(|(menu, _)| menu)
    }

    /// Open the list of top-level menus.
    pub(crate) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            return;
        }
        self.restore_focus = window.focused(cx);
        let this = cx.weak_entity();
        let names: Vec<_> = self
            .model
            .sections
            .iter()
            .map(|section| section.name)
            .collect();
        let menu = Menu::build(window, cx, |mut menu| {
            for (index, name) in names.into_iter().enumerate() {
                let this = this.clone();
                menu = menu.entry(
                    MenuEntry::new(name)
                        .trailing_icon("chevron-right")
                        .on_select(move |window, cx| {
                            this.update(cx, |button, cx| button.open_section(index, window, cx))
                                .ok();
                        }),
                );
            }
            menu
        });
        self.show(menu, window, cx);
    }

    /// Open top-level menu `index`, in place of the list of menus.
    pub(crate) fn open_section(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(section) = self.model.sections.get(index) else {
            return;
        };
        let origin = self.command_origin();
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let mut entries = Vec::new();
        for item in &section.items {
            let MenuModelItem::Command(command) = item else {
                entries.push(None);
                continue;
            };
            let enabled =
                command.app_wide || window.is_action_available_in(command.action.as_ref(), &origin);
            let mut entry = MenuEntry::new(command.label).disabled(!enabled);
            if let Some(binding) = menu_binding(&keymap, command.action.as_ref()) {
                entry = entry.shortcut(binding_label(binding));
            }
            let action = command.action.boxed_clone();
            let origin = origin.clone();
            entries.push(Some(entry.on_select(move |window, cx| {
                origin.dispatch_action(action.as_ref(), window, cx);
            })));
        }
        drop(keymap);
        let name = section.name;
        let menu = Menu::build(window, cx, |menu| {
            entries
                .into_iter()
                .fold(menu.header(name), |menu, entry| match entry {
                    Some(entry) => menu.entry(entry),
                    None => menu.separator(),
                })
        });
        self.show(menu, window, cx);
    }

    /// Where commands run from: what had focus when the menu opened, or
    /// the window root.
    fn command_origin(&self) -> FocusHandle {
        self.restore_focus
            .clone()
            .unwrap_or_else(|| self.root_focus.clone())
    }

    fn show(&mut self, menu: Entity<Menu>, window: &mut Window, cx: &mut Context<Self>) {
        let subscription =
            cx.subscribe_in(&menu, window, |button, _, _: &DismissEvent, window, cx| {
                button.close(window, cx);
            });
        window.focus(&menu.focus_handle(cx), cx);
        self.menu = Some((menu, subscription));
        cx.notify();
    }

    /// Close the menu and give focus back. The menu closes before an
    /// entry runs, so the command (or the next menu) sees the original
    /// focus.
    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            window.focus(&self.command_origin(), cx);
            cx.notify();
        }
    }
}

impl Render for AppMenuButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (icon, label) = match PlatformStyle::current().menu {
            MenuPlacement::TitleBarOverflow => ("ellipsis", "More"),
            MenuPlacement::HeaderBarHamburger | MenuPlacement::MenuBar => ("menu", "Main Menu"),
        };
        div()
            .relative()
            .flex_none()
            // In the title bar, a press here must reach the button rather
            // than start a window drag.
            .occlude()
            // Leave keyboard focus where it is, so the menu's commands act
            // on the pane the person was using. This has to run in the
            // capture phase, before the button takes focus on mouse down.
            .capture_any_mouse_down(|event, window, _| {
                if event.button == MouseButton::Left {
                    window.prevent_default();
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|button, _, _, _| {
                    button.swallow_click = button.menu.is_some();
                }),
            )
            .child(
                IconButton::new("app-menu", icon, label).on_click(cx.listener(
                    |button, _, window, cx| {
                        if std::mem::take(&mut button.swallow_click) {
                            return;
                        }
                        if button.menu.is_some() {
                            button.close(window, cx);
                        } else {
                            button.open(window, cx);
                        }
                    },
                )),
            )
            .when_some(self.menu.as_ref(), |this, (menu, _)| {
                this.child(popover_below_end(menu.clone()))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        Context, FocusHandle, IntoElement, Render, TestAppContext, VisualTestContext, Window, div,
    };

    use zmail_core::preferences::KeymapName;

    use super::*;
    use crate::actions::Reply;
    use crate::components::test_support::init;

    /// A window root that handles Reply (like the workspace) and holds the
    /// app-menu button.
    struct Root {
        focus_handle: FocusHandle,
        button: Entity<AppMenuButton>,
        replies: Rc<RefCell<usize>>,
    }

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let replies = self.replies.clone();
            div()
                .key_context(crate::workspace::KEY_CONTEXT)
                .track_focus(&self.focus_handle)
                .on_action(move |_: &Reply, _, _| *replies.borrow_mut() += 1)
                .size_full()
                .child(self.button.clone())
        }
    }

    fn open_root(cx: &mut TestAppContext) -> (Entity<Root>, &mut VisualTestContext) {
        init(cx);
        cx.update(|cx| {
            crate::keymap::bind_keymap(KeymapName::Default, cx);
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let focus_handle = cx.focus_handle();
            window.focus(&focus_handle, cx);
            let button =
                cx.new(|_| AppMenuButton::for_platform(Platform::Linux, focus_handle.clone()));
            Root {
                focus_handle,
                button,
                replies: Rc::default(),
            }
        });
        cx.run_until_parked();
        (root, cx)
    }

    fn button(root: &Entity<Root>, cx: &mut VisualTestContext) -> Entity<AppMenuButton> {
        root.read_with(cx, |root, _| root.button.clone())
    }

    /// The open menu's rows as (label, shortcut, disabled).
    fn rows(
        button: &Entity<AppMenuButton>,
        cx: &mut VisualTestContext,
    ) -> Vec<(String, Option<String>, bool)> {
        button.read_with(cx, |button, cx| {
            button
                .open_menu()
                .expect("the menu is open")
                .read(cx)
                .entries()
                .map(|entry| {
                    (
                        entry.label().to_string(),
                        entry.shortcut_text().map(str::to_owned),
                        entry.is_disabled(),
                    )
                })
                .collect()
        })
    }

    #[gpui::test]
    fn the_button_lists_the_menus_then_their_commands(cx: &mut TestAppContext) {
        let (root, cx) = open_root(cx);
        let button = button(&root, cx);
        button.update_in(cx, |button, window, cx| button.open(window, cx));
        let names: Vec<_> = rows(&button, cx).into_iter().map(|row| row.0).collect();
        assert_eq!(
            names,
            ["File", "Edit", "View", "Mailbox", "Message", "Help"]
        );

        // Message, the menu before Help.
        cx.simulate_keystrokes("end up enter");
        let rows = rows(&button, cx);
        let row = |label: &str| {
            rows.iter()
                .find(|row| row.0 == label)
                .unwrap_or_else(|| panic!("no {label} row in {rows:?}"))
                .clone()
        };
        assert_eq!(row("Reply").1.as_deref(), Some("ctrl-r"));
        assert_eq!(row("Delete").1.as_deref(), Some("ctrl-backspace"));
        assert!(!row("Reply").2, "the root handles Reply");
        assert!(row("Send").2, "nothing outside compose handles Send");
        assert!(
            rows.iter().all(|row| row.1.is_some()),
            "every Message command shows a shortcut: {rows:?}"
        );
    }

    #[gpui::test]
    fn a_command_runs_from_the_focus_it_was_opened_from(cx: &mut TestAppContext) {
        let (root, cx) = open_root(cx);
        let button = button(&root, cx);
        button.update_in(cx, |button, window, cx| button.open(window, cx));
        // Message (before Help), then Reply (the first command).
        cx.simulate_keystrokes("end up enter down enter");
        root.read_with(cx, |root, _| assert_eq!(*root.replies.borrow(), 1));
        button.read_with(cx, |button, _| assert!(button.open_menu().is_none()));
        let focused = cx.update(|window, cx| window.focused(cx));
        let root_focus = root.read_with(cx, |root, _| root.focus_handle.clone());
        assert_eq!(focused, Some(root_focus), "focus went back to the root");
    }

    #[gpui::test]
    fn escape_closes_and_restores_focus(cx: &mut TestAppContext) {
        let (root, cx) = open_root(cx);
        let button = button(&root, cx);
        button.update_in(cx, |button, window, cx| button.open(window, cx));
        cx.simulate_keystrokes("enter escape");
        button.read_with(cx, |button, _| assert!(button.open_menu().is_none()));
        let focused = cx.update(|window, cx| window.focused(cx));
        let root_focus = root.read_with(cx, |root, _| root.focus_handle.clone());
        assert_eq!(focused, Some(root_focus));
    }
}
