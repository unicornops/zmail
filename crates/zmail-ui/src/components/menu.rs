//! `Menu`: a list of commands in a popover, also used as a context menu
//! and by [`Select`](super::Select).
//!
//! Keyboard: Up/Down move the highlight (wrapping, skipping separators,
//! headers and disabled entries), Home/PageUp and End/PageDown jump to the
//! ends, Enter or Space runs the highlighted entry, and Escape or Tab
//! closes the menu. Moving the pointer over an entry highlights it, and a
//! click outside closes the menu. Running an entry or closing emits
//! [`DismissEvent`]; the opener drops the menu and restores focus.
//!
//! ```text
//! let menu = Menu::build(window, cx, |menu| {
//!     menu.entry(MenuEntry::new("Archive").icon("archive").shortcut("e").on_select(...))
//!         .separator()
//!         .entry(MenuEntry::new("Delete").disabled(true))
//! });
//! window.focus(&menu.focus_handle(cx), cx);
//! ```

use std::rc::Rc;

use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    Render, Role, SharedString, Window, div, prelude::*, px, rgb,
};

use crate::styles::{
    ICON_SIZE_SM, RADIUS_SM, SPACE_1, SPACE_2, TYPE_BODY, TYPE_CAPTION, TypeStyleExt,
};
use crate::widgets::{EllipsisExt, icon};

use super::actions::{
    Cancel, Confirm, MENU_CONTEXT, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
use super::kbd::Kbd;
use super::popover::Popover;
use super::theme::ComponentTheme;

/// Height of one menu entry.
pub(crate) const MENU_ITEM_HEIGHT: f32 = 28.0;
/// Narrowest a menu gets.
pub(crate) const MENU_MIN_WIDTH: f32 = 200.0;

/// What a menu entry runs when chosen.
pub(crate) type MenuHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// One command in a [`Menu`].
#[derive(Clone)]
pub(crate) struct MenuEntry {
    label: SharedString,
    icon: Option<&'static str>,
    shortcut: Option<SharedString>,
    disabled: bool,
    checked: Option<bool>,
    handler: Option<MenuHandler>,
}

impl MenuEntry {
    /// An entry labelled `label` that does nothing until given a handler.
    pub(crate) fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            shortcut: None,
            disabled: false,
            checked: None,
            handler: None,
        }
    }

    /// Show a leading Lucide icon.
    pub(crate) fn icon(mut self, name: &'static str) -> Self {
        self.icon = Some(name);
        self
    }

    /// Show the command's keyboard shortcut (GPUI binding syntax).
    pub(crate) fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Grey the entry out and skip it during keyboard navigation.
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Give the entry a check-mark column, ticked when `checked`.
    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Run `handler` when the entry is chosen.
    pub(crate) fn on_select(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.handler = Some(Rc::new(handler));
        self
    }
}

/// A row of a [`Menu`].
#[derive(Clone)]
pub(crate) enum MenuItem {
    /// A command.
    Entry(MenuEntry),
    /// A dividing line.
    Separator,
    /// A non-interactive group title.
    Header(SharedString),
}

impl MenuItem {
    fn is_selectable(&self) -> bool {
        matches!(self, Self::Entry(entry) if !entry.disabled)
    }
}

/// A keyboard- and mouse-operable command menu.
pub(crate) struct Menu {
    items: Vec<MenuItem>,
    selected: Option<usize>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DismissEvent> for Menu {}

impl Focusable for Menu {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Menu {
    /// Build a menu entity; `build` adds the items.
    pub(crate) fn build(
        _window: &mut Window,
        cx: &mut App,
        build: impl FnOnce(Self) -> Self,
    ) -> gpui::Entity<Self> {
        cx.new(|cx| {
            build(Self {
                items: Vec::new(),
                selected: None,
                focus_handle: cx.focus_handle(),
            })
        })
    }

    /// Append an entry.
    pub(crate) fn entry(mut self, entry: MenuEntry) -> Self {
        self.items.push(MenuItem::Entry(entry));
        self
    }

    /// Append an entry labelled `label` that runs `handler`.
    pub(crate) fn item(
        self,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.entry(MenuEntry::new(label).on_select(handler))
    }

    /// Append a separator.
    pub(crate) fn separator(mut self) -> Self {
        self.items.push(MenuItem::Separator);
        self
    }

    /// Append a group header.
    pub(crate) fn header(mut self, title: impl Into<SharedString>) -> Self {
        self.items.push(MenuItem::Header(title.into()));
        self
    }

    /// Highlight item `index` when the menu opens (a select's current
    /// value). Non-selectable indices are ignored.
    pub(crate) fn with_selected(mut self, index: usize) -> Self {
        if self.items.get(index).is_some_and(MenuItem::is_selectable) {
            self.selected = Some(index);
        }
        self
    }

    /// The highlighted item, if any.
    #[cfg(test)]
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = step(&self.items, self.selected, true);
        cx.notify();
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = step(&self.items, self.selected, false);
        cx.notify();
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.items.iter().position(MenuItem::is_selectable);
        cx.notify();
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.items.iter().rposition(MenuItem::is_selectable);
        cx.notify();
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.selected {
            self.activate(index, window, cx);
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    /// Run entry `index` (if it is enabled) and close the menu.
    pub(crate) fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(MenuItem::Entry(entry)) = self.items.get(index) else {
            return;
        };
        if entry.disabled {
            return;
        }
        let handler = entry.handler.clone();
        // Close first, so the handler can open something else (a modal,
        // another menu) and focus it without the menu taking focus back.
        cx.emit(DismissEvent);
        if let Some(handler) = handler {
            handler(window, cx);
        }
    }

    fn render_entry(
        &self,
        index: usize,
        entry: &MenuEntry,
        theme: &ComponentTheme,
        has_checks: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = theme.colors;
        let highlighted = self.selected == Some(index);
        let (background, foreground, muted) = if highlighted {
            (
                Some(colors.selection_background),
                colors.selection_foreground,
                colors.selection_foreground,
            )
        } else {
            (None, colors.foreground, colors.muted)
        };
        let disabled = entry.disabled;
        div()
            .id(("menu-item", index))
            .flex()
            .items_center()
            .gap(px(SPACE_2))
            .h(px(MENU_ITEM_HEIGHT))
            .px(px(SPACE_2))
            .rounded(px(RADIUS_SM))
            .type_style(TYPE_BODY)
            .role(Role::MenuItem)
            .aria_label(entry.label.clone())
            .aria_selected(highlighted)
            .when_some(background, |this, background| this.bg(rgb(background)))
            .text_color(rgb(foreground))
            .when(disabled, |this| this.opacity(0.45))
            .when(!disabled, |this| {
                this.cursor_pointer()
                    .on_hover(cx.listener(move |menu, hovered: &bool, _, cx| {
                        if *hovered && menu.selected != Some(index) {
                            menu.selected = Some(index);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |menu, _, window, cx| {
                        menu.activate(index, window, cx);
                    }))
            })
            .when(has_checks, |this| {
                this.child(
                    div()
                        .flex_none()
                        .size(px(ICON_SIZE_SM))
                        .when(entry.checked == Some(true), |this| {
                            this.child(icon("check", ICON_SIZE_SM, foreground))
                        }),
                )
            })
            .when_some(entry.icon, |this, name| {
                this.child(icon(name, ICON_SIZE_SM, muted))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ellipsis()
                    .child(entry.label.clone()),
            )
            .when_some(entry.shortcut.clone(), |this, shortcut| {
                this.child(Kbd::new(shortcut))
            })
    }
}

/// The next selectable index after (or before) `from`, wrapping around.
/// With nothing highlighted, Down goes to the first entry and Up to the
/// last.
pub(crate) fn step(items: &[MenuItem], from: Option<usize>, forward: bool) -> Option<usize> {
    let count = items.len();
    if count == 0 {
        return None;
    }
    let start = match (from, forward) {
        (Some(index), _) => index,
        (None, true) => count - 1,
        (None, false) => 0,
    };
    (1..=count)
        .map(|offset| {
            if forward {
                (start + offset) % count
            } else {
                (start + count - offset % count) % count
            }
        })
        .find(|&index| items[index].is_selectable())
        .or(from)
}

impl Render for Menu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let has_checks = self
            .items
            .iter()
            .any(|item| matches!(item, MenuItem::Entry(entry) if entry.checked.is_some()));
        let items: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| match item {
                MenuItem::Entry(entry) => self
                    .render_entry(index, entry, &theme, has_checks, cx)
                    .into_any_element(),
                MenuItem::Separator => div()
                    .my(px(SPACE_1))
                    .h(px(1.0))
                    .bg(rgb(colors.border))
                    .into_any_element(),
                MenuItem::Header(title) => div()
                    .px(px(SPACE_2))
                    .pt(px(SPACE_2))
                    .pb(px(SPACE_1))
                    .type_style(TYPE_CAPTION)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(colors.muted))
                    .child(title.clone())
                    .into_any_element(),
            })
            .collect();

        div()
            .id("menu")
            .key_context(MENU_CONTEXT)
            .track_focus(&self.focus_handle)
            .role(Role::Menu)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(Popover::new().min_width(px(MENU_MIN_WIDTH)).children(items))
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{Entity, Subscription, TestAppContext, VisualTestContext};

    use super::*;
    use crate::components::test_support::{init, press};

    fn entry(label: &str) -> MenuItem {
        MenuItem::Entry(MenuEntry::new(label.to_owned()))
    }

    fn disabled(label: &str) -> MenuItem {
        MenuItem::Entry(MenuEntry::new(label.to_owned()).disabled(true))
    }

    #[test]
    fn step_skips_separators_headers_and_disabled_entries_and_wraps() {
        let items = vec![
            MenuItem::Header("Move to".into()),
            entry("Inbox"),
            MenuItem::Separator,
            disabled("Spam"),
            entry("Trash"),
        ];
        assert_eq!(step(&items, None, true), Some(1));
        assert_eq!(step(&items, Some(1), true), Some(4));
        assert_eq!(step(&items, Some(4), true), Some(1));
        assert_eq!(step(&items, None, false), Some(4));
        assert_eq!(step(&items, Some(1), false), Some(4));
        assert_eq!(step(&items, Some(4), false), Some(1));
        assert_eq!(step(&[MenuItem::Separator], None, true), None);
        assert_eq!(step(&[], None, true), None);
    }

    /// A view that opens a menu and records what it did.
    struct Host {
        menu: Option<Entity<Menu>>,
        log: Rc<RefCell<Vec<String>>>,
        dismissed: usize,
        _subscription: Option<Subscription>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .when_some(self.menu.clone(), |this, menu| this.child(menu))
        }
    }

    fn open_menu(cx: &mut TestAppContext) -> (Entity<Host>, &mut VisualTestContext) {
        init(cx);
        let log = Rc::new(RefCell::new(Vec::new()));
        let (host, cx) = cx.add_window_view(|_, _| Host {
            menu: None,
            log: log.clone(),
            dismissed: 0,
            _subscription: None,
        });
        host.update_in(cx, |host, window, cx| {
            let record = |label: &'static str| {
                let log = host.log.clone();
                move |_: &mut Window, _: &mut App| log.borrow_mut().push(label.to_owned())
            };
            let menu = Menu::build(window, cx, |menu| {
                menu.header("Message")
                    .entry(
                        MenuEntry::new("Archive")
                            .shortcut("e")
                            .on_select(record("archive")),
                    )
                    .entry(
                        MenuEntry::new("Spam")
                            .disabled(true)
                            .on_select(record("spam")),
                    )
                    .separator()
                    .entry(MenuEntry::new("Delete").on_select(record("delete")))
            });
            host._subscription = Some(cx.subscribe(&menu, |host, _, _: &DismissEvent, cx| {
                host.dismissed += 1;
                host.menu = None;
                cx.notify();
            }));
            window.focus(&menu.focus_handle(cx), cx);
            host.menu = Some(menu);
            cx.notify();
        });
        cx.run_until_parked();
        (host, cx)
    }

    fn selected(host: &Entity<Host>, cx: &mut VisualTestContext) -> Option<usize> {
        host.read_with(cx, |host, cx| {
            host.menu
                .as_ref()
                .and_then(|menu| menu.read(cx).selected_index())
        })
    }

    #[gpui::test]
    fn arrow_keys_move_the_highlight_and_enter_runs_the_entry(cx: &mut TestAppContext) {
        let (host, cx) = open_menu(cx);
        assert_eq!(selected(&host, cx), None);

        cx.simulate_keystrokes("down");
        assert_eq!(selected(&host, cx), Some(1), "first entry, header skipped");
        cx.simulate_keystrokes("down");
        assert_eq!(
            selected(&host, cx),
            Some(4),
            "disabled and separator skipped"
        );
        cx.simulate_keystrokes("down");
        assert_eq!(selected(&host, cx), Some(1), "wraps to the top");
        cx.simulate_keystrokes("up");
        assert_eq!(selected(&host, cx), Some(4), "wraps to the bottom");
        cx.simulate_keystrokes("home");
        assert_eq!(selected(&host, cx), Some(1));
        cx.simulate_keystrokes("end");
        assert_eq!(selected(&host, cx), Some(4));

        cx.simulate_keystrokes("enter");
        host.read_with(cx, |host, _| {
            assert_eq!(*host.log.borrow(), ["delete"]);
            assert_eq!(host.dismissed, 1);
            assert!(host.menu.is_none());
        });
    }

    #[gpui::test]
    fn space_runs_the_highlighted_entry(cx: &mut TestAppContext) {
        let (host, cx) = open_menu(cx);
        press(cx, "down space");
        host.read_with(cx, |host, _| {
            assert_eq!(*host.log.borrow(), ["archive"]);
            assert!(host.menu.is_none());
        });
    }

    #[gpui::test]
    fn escape_closes_without_acting(cx: &mut TestAppContext) {
        let (host, cx) = open_menu(cx);
        cx.simulate_keystrokes("down escape");
        host.read_with(cx, |host, _| {
            assert!(host.log.borrow().is_empty());
            assert_eq!(host.dismissed, 1);
        });
    }

    #[gpui::test]
    fn tab_closes_without_acting(cx: &mut TestAppContext) {
        let (host, cx) = open_menu(cx);
        cx.simulate_keystrokes("down tab");
        host.read_with(cx, |host, _| {
            assert!(host.log.borrow().is_empty());
            assert_eq!(host.dismissed, 1);
        });
    }

    #[gpui::test]
    fn disabled_entries_cannot_be_activated(cx: &mut TestAppContext) {
        let (host, cx) = open_menu(cx);
        host.update_in(cx, |host, window, cx| {
            let menu = host.menu.clone().expect("menu is open");
            menu.update(cx, |menu, cx| menu.activate(2, window, cx));
        });
        host.read_with(cx, |host, _| {
            assert!(host.log.borrow().is_empty());
            assert_eq!(host.dismissed, 0, "still open");
        });
    }
}
