//! `Select`: pick one option from a list, shown as a button that opens a
//! [`Menu`].
//!
//! Keyboard: Tab focuses the trigger; Enter, Space, Down, Up or Alt+Down
//! open the menu with the current option highlighted; the menu's keys then
//! apply, and choosing or closing returns focus to the trigger.

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, Pixels, Render, Role, SharedString, Subscription, Window, div, prelude::*, px,
    rgb,
};

use crate::a11y::{A11yFlags, AccessibleExt};
use crate::styles::{ICON_SIZE_SM, SPACE_2, TYPE_BODY, TextScale, TypeStyleExt, rems_from_px};
use crate::widgets::{EllipsisExt, icon};

use super::actions::{SELECT_CONTEXT, SelectNext, SelectPrevious};
use super::hit_target::HitTargetExt;
use super::menu::{Menu, MenuEntry};
use super::popover::popover_below;
use super::theme::{ComponentTheme, FocusRingExt};

/// Emitted by a [`Select`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectEvent {
    /// The user chose the option at this index.
    Changed(usize),
}

/// A drop-down picker.
pub(crate) struct Select {
    options: Vec<SharedString>,
    selected: Option<usize>,
    placeholder: SharedString,
    label: SharedString,
    disabled: bool,
    width: Option<Pixels>,
    focus_handle: FocusHandle,
    menu: Option<(Entity<Menu>, Subscription)>,
    /// Set when a pointer press lands on the trigger while the menu is
    /// open: that press already closed the menu, so its click must not
    /// reopen it.
    swallow_click: bool,
}

impl EventEmitter<SelectEvent> for Select {}

impl Focusable for Select {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Select {
    /// A picker over `options` with `selected` chosen (or nothing).
    pub(crate) fn new(
        options: impl IntoIterator<Item = impl Into<SharedString>>,
        selected: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Self {
        let options: Vec<SharedString> = options.into_iter().map(Into::into).collect();
        let selected = selected.filter(|index| *index < options.len());
        Self {
            options,
            selected,
            placeholder: "Choose…".into(),
            label: SharedString::default(),
            disabled: false,
            width: None,
            focus_handle: cx.focus_handle(),
            menu: None,
            swallow_click: false,
        }
    }

    /// Text shown while nothing is chosen.
    pub(crate) fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The accessible name, such as the field label beside it.
    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    /// A fixed trigger width at the default text size; it grows with the
    /// text size.
    pub(crate) fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }

    /// Enable or disable the picker.
    pub(crate) fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    /// The chosen option's index.
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// The chosen option's text.
    pub(crate) fn selected_label(&self) -> Option<&SharedString> {
        self.selected.and_then(|index| self.options.get(index))
    }

    /// Change the chosen option without emitting [`SelectEvent`].
    pub(crate) fn set_selected(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        self.selected = index.filter(|index| *index < self.options.len());
        cx.notify();
    }

    /// Whether the menu is open.
    #[cfg(test)]
    pub(crate) fn is_open(&self) -> bool {
        self.menu.is_some()
    }

    /// Open the menu and give it focus.
    pub(crate) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled || self.menu.is_some() || self.options.is_empty() {
            return;
        }
        let this = cx.weak_entity();
        let options = self.options.clone();
        let selected = self.selected;
        let menu = Menu::build(window, cx, |mut menu| {
            for (index, option) in options.into_iter().enumerate() {
                let this = this.clone();
                menu = menu.entry(
                    MenuEntry::new(option)
                        .checked(selected == Some(index))
                        .on_select(move |_, cx| {
                            this.update(cx, |select, cx| select.choose(index, cx)).ok();
                        }),
                );
            }
            match selected {
                Some(index) => menu.with_selected(index),
                None => menu,
            }
        });
        let subscription =
            cx.subscribe_in(&menu, window, |select, _, _: &DismissEvent, window, cx| {
                select.close(window, cx);
            });
        window.focus(&menu.focus_handle(cx), cx);
        self.menu = Some((menu, subscription));
        cx.notify();
    }

    /// Close the menu and return focus to the trigger.
    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Choose the option at `index` as a click on it would.
    #[cfg(test)]
    pub(crate) fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        self.choose(index, cx);
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(index);
        cx.emit(SelectEvent::Changed(index));
        cx.notify();
    }

    fn open_from_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open(window, cx);
    }
}

impl Render for Select {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let fill = colors.button_background;
        let hover = theme.hover_tint(fill);
        let (text, text_color) = match self.selected_label() {
            Some(label) => (label.clone(), colors.foreground),
            None => (self.placeholder.clone(), colors.muted),
        };
        let disabled = self.disabled;
        let open = self.menu.is_some();

        div()
            .relative()
            .flex_none()
            .when_some(self.width, |this, width| {
                this.w(rems_from_px(f32::from(width)))
            })
            .child(
                div()
                    .id("select-trigger")
                    .hit_target(|| format!("select-{}", cx.entity_id()))
                    .key_context(SELECT_CONTEXT)
                    .track_focus(&self.focus_handle.clone().tab_index(0).tab_stop(!disabled))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .w_full()
                    .h(TextScale::get(cx).fit(theme.platform.control_height, TYPE_BODY))
                    .px(px(10.0))
                    .rounded(px(theme.platform.control_radius))
                    .bg(rgb(fill))
                    .border_1()
                    .border_color(rgb(colors.control_border))
                    .type_style(TYPE_BODY)
                    .text_color(rgb(text_color))
                    .focus_ring(&theme)
                    .accessible(Role::ComboBox, self.label.clone())
                    .aria_value(text.clone())
                    .aria_expanded(open)
                    .a11y_flags(A11yFlags::disabled(disabled))
                    .map(|this| {
                        if disabled {
                            this.opacity(0.45)
                        } else {
                            this.tab_index(0)
                                .cursor_pointer()
                                .hover(move |style| style.bg(rgb(hover)))
                                .on_action(cx.listener(|select, _: &SelectNext, window, cx| {
                                    select.open_from_key(window, cx);
                                }))
                                .on_action(cx.listener(|select, _: &SelectPrevious, window, cx| {
                                    select.open_from_key(window, cx);
                                }))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|select, _, _, _| {
                                        select.swallow_click = select.menu.is_some();
                                    }),
                                )
                                .on_click(cx.listener(|select, _, window, cx| {
                                    if std::mem::take(&mut select.swallow_click) {
                                        return;
                                    }
                                    if select.menu.is_some() {
                                        select.close(window, cx);
                                    } else {
                                        select.open(window, cx);
                                    }
                                }))
                        }
                    })
                    .child(div().flex_1().min_w_0().ellipsis().child(text))
                    .child(icon("chevron-down", ICON_SIZE_SM, colors.muted)),
            )
            .when_some(self.menu.as_ref(), |this, (menu, _)| {
                this.child(popover_below(menu.clone()))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::TestAppContext;

    use super::*;
    use crate::components::test_support::{init, press};

    struct Host {
        select: Entity<Select>,
        _subscription: Subscription,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.select.clone())
        }
    }

    #[gpui::test]
    fn keyboard_opens_moves_and_chooses(cx: &mut TestAppContext) {
        init(cx);
        let events = Rc::new(RefCell::new(Vec::new()));
        let (host, cx) = cx.add_window_view(|_, cx| {
            let select =
                cx.new(|cx| Select::new(["Comfortable", "Compact"], Some(0), cx).label("Density"));
            let log = events.clone();
            let subscription = cx.subscribe(&select, move |_, _, event: &SelectEvent, _| {
                log.borrow_mut().push(*event);
            });
            Host {
                select,
                _subscription: subscription,
            }
        });
        let select = host.read_with(cx, |host, _| host.select.clone());
        let trigger = select.read_with(cx, |select, _| select.focus_handle.clone());
        cx.update(|window, cx| window.focus(&trigger, cx));
        cx.run_until_parked();

        // Down opens the menu with the current value highlighted.
        cx.simulate_keystrokes("down");
        select.read_with(cx, |select, cx| {
            assert!(select.is_open());
            let (menu, _) = select.menu.as_ref().expect("open");
            assert_eq!(menu.read(cx).selected_index(), Some(0));
        });

        cx.simulate_keystrokes("down enter");
        select.read_with(cx, |select, _| {
            assert!(!select.is_open());
            assert_eq!(select.selected_label().map(|s| s.as_ref()), Some("Compact"));
        });
        assert_eq!(*events.borrow(), [SelectEvent::Changed(1)]);
        cx.update(|window, _| assert!(trigger.is_focused(window), "focus returns"));

        // Space opens it too, and Escape closes without changing it.
        press(cx, "space");
        assert!(select.read_with(cx, |select, _| select.is_open()));
        cx.simulate_keystrokes("up escape");
        select.read_with(cx, |select, _| {
            assert!(!select.is_open());
            assert_eq!(select.selected_index(), Some(1));
        });
        assert_eq!(events.borrow().len(), 1);
        cx.update(|window, _| assert!(trigger.is_focused(window)));
    }
}
