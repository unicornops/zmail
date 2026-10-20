//! `MovePicker`: the "Move to…" folder picker, a search field over the
//! account's other folders in a popover.
//!
//! Typing filters the folders (names starting with the text first), Up and
//! Down move the highlight (the menu key bindings), Enter or a click picks
//! a folder, and Escape or a click outside closes the picker. Picking emits
//! [`MovePickerEvent::Picked`]; picking or closing emits [`DismissEvent`],
//! and the opener drops the picker and restores focus.
//!
//! ```text
//! let picker = cx.new(|cx| MovePicker::new(destinations, window, cx));
//! // In render, beside the button that opened it:
//! .child(popover_below(picker.clone()))
//! ```

use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Role,
    SharedString, Subscription, Window, div, prelude::*, px, rgb,
};
use zmail_core::MailboxSummary;

use crate::a11y::{
    AccessibleExt, Politeness, announcement_key, highlight_announcement, live_region,
};
use crate::components::actions::{Cancel, MENU_CONTEXT, SelectNext, SelectPrevious};
use crate::components::menu::{MENU_ITEM_HEIGHT, MENU_MIN_WIDTH};
use crate::components::popover::Popover;
use crate::components::{ComponentTheme, TextInput, TextInputEvent};
use crate::styles::{ICON_SIZE_SM, RADIUS_SM, SPACE_1, SPACE_2, TYPE_BODY, TypeStyleExt};
use crate::widgets::{EllipsisExt, icon};

/// The most folder rows shown before the list scrolls.
const VISIBLE_ROWS: f32 = 10.0;

/// What the picker reports besides [`DismissEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MovePickerEvent {
    /// The user picked this mailbox.
    Picked(i64),
}

/// A searchable list of destination folders.
pub(crate) struct MovePicker {
    focus_handle: FocusHandle,
    query: gpui::Entity<TextInput>,
    /// Every destination, in sidebar order.
    mailboxes: Vec<MailboxSummary>,
    /// Indices into `mailboxes` that match the query, best first.
    matches: Vec<usize>,
    /// Index into `matches`.
    highlighted: usize,
    _subscription: Subscription,
}

impl EventEmitter<MovePickerEvent> for MovePicker {}
impl EventEmitter<DismissEvent> for MovePicker {}

impl Focusable for MovePicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl MovePicker {
    /// A picker over `mailboxes` (the folders the conversations can move
    /// to), with focus in its search field.
    pub(crate) fn new(
        mailboxes: Vec<MailboxSummary>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(cx)
                .compact()
                .leading_icon("search")
                .label("Move to folder")
                .placeholder("Move to…")
        });
        // The field keeps Enter (it submits) and passes Up and Down on to
        // the picker's menu bindings.
        let subscription = cx.subscribe(
            &query,
            |picker, _, event: &TextInputEvent, cx| match event {
                TextInputEvent::Changed => picker.refilter(cx),
                TextInputEvent::Submitted => picker.pick(picker.highlighted, cx),
            },
        );
        query.update(cx, |query, cx| query.focus(window, cx));
        let matches = (0..mailboxes.len()).collect();
        Self {
            focus_handle: cx.focus_handle(),
            query,
            mailboxes,
            matches,
            highlighted: 0,
            _subscription: subscription,
        }
    }

    /// The names of the folders that match the query, best first.
    #[cfg(test)]
    pub(crate) fn matching_names(&self) -> Vec<&str> {
        self.matches
            .iter()
            .map(|&index| self.mailboxes[index].name.as_str())
            .collect()
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().trim().to_lowercase();
        self.matches = filter(&self.mailboxes, &query);
        self.highlighted = 0;
        cx.notify();
    }

    fn pick(&mut self, position: usize, cx: &mut Context<Self>) {
        let Some(mailbox) = self
            .matches
            .get(position)
            .map(|&index| &self.mailboxes[index])
        else {
            return;
        };
        cx.emit(MovePickerEvent::Picked(mailbox.id));
        cx.emit(DismissEvent);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            self.highlighted = (self.highlighted + 1) % self.matches.len();
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            self.highlighted = (self.highlighted + self.matches.len() - 1) % self.matches.len();
            cx.notify();
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

/// The indices of `mailboxes` whose names contain `query` (already
/// lowercased), names starting with it first, each group in sidebar order.
fn filter(mailboxes: &[MailboxSummary], query: &str) -> Vec<usize> {
    let mut prefix = Vec::new();
    let mut inside = Vec::new();
    for (index, mailbox) in mailboxes.iter().enumerate() {
        let name = mailbox.name.to_lowercase();
        if name.starts_with(query) {
            prefix.push(index);
        } else if name.contains(query) {
            inside.push(index);
        }
    }
    prefix.extend(inside);
    prefix
}

impl Render for MovePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let rows: Vec<_> = self
            .matches
            .iter()
            .enumerate()
            .map(|(position, &index)| {
                let mailbox = &self.mailboxes[index];
                let highlighted = position == self.highlighted;
                let (background, foreground, muted) = if highlighted {
                    (
                        Some(colors.selection_background),
                        colors.selection_foreground,
                        colors.selection_foreground,
                    )
                } else {
                    (None, colors.foreground, colors.muted)
                };
                let name = SharedString::from(mailbox.name.clone());
                div()
                    .id(("move-picker-folder", position))
                    .debug_selector(move || format!("move-picker-{position}"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(SPACE_2))
                    .h(px(MENU_ITEM_HEIGHT))
                    .px(px(SPACE_2))
                    .rounded(px(RADIUS_SM))
                    .type_style(TYPE_BODY)
                    .accessible(Role::MenuItem, name.clone())
                    .aria_selected(highlighted)
                    .cursor_pointer()
                    .when_some(background, |this, background| this.bg(rgb(background)))
                    .text_color(rgb(foreground))
                    .on_hover(cx.listener(move |picker, hovered: &bool, _, cx| {
                        if *hovered && picker.highlighted != position {
                            picker.highlighted = position;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |picker, _, _, cx| picker.pick(position, cx)))
                    .child(icon("folder", ICON_SIZE_SM, muted))
                    .child(div().flex_1().min_w_0().ellipsis().child(name))
            })
            .collect();
        let empty = rows.is_empty();
        let highlight = highlight_announcement(
            &self
                .matches
                .get(self.highlighted)
                .map(|&index| self.mailboxes[index].name.clone())
                .unwrap_or_default(),
            self.highlighted,
            self.matches.len(),
        );

        div()
            .id("move-picker")
            .track_focus(&self.focus_handle)
            .accessible(Role::Menu, "Move to folder")
            .relative()
            .child(live_region(
                ("move-picker-highlight", announcement_key(&highlight)),
                highlight,
                Politeness::Polite,
            ))
            .key_context(MENU_CONTEXT)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::cancel))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(
                Popover::new()
                    .min_width(px(MENU_MIN_WIDTH))
                    .child(div().p(px(SPACE_1)).child(self.query.clone()))
                    .child(
                        div()
                            .id("move-picker-folders")
                            .flex()
                            .flex_col()
                            .max_h(px(MENU_ITEM_HEIGHT * VISIBLE_ROWS))
                            .overflow_y_scroll()
                            .children(rows),
                    )
                    .when(empty, |this| {
                        this.child(
                            div()
                                .px(px(SPACE_2))
                                .py(px(SPACE_1))
                                .type_style(TYPE_BODY)
                                .text_color(rgb(colors.muted))
                                .child("No folder matches"),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{Entity, Subscription, TestAppContext, VisualTestContext};

    use super::*;
    use crate::components::test_support::{init, press};

    fn mailbox(id: i64, name: &str) -> MailboxSummary {
        MailboxSummary {
            id,
            account_id: 1,
            name: name.to_owned(),
            role: None,
            delimiter: None,
            parent: None,
            total_threads: 0,
            unread_threads: 0,
        }
    }

    struct Root {
        picker: Entity<MovePicker>,
        events: Rc<RefCell<Vec<String>>>,
        _subscriptions: Vec<Subscription>,
    }

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.picker.clone())
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<Root>, &mut VisualTestContext) {
        init(cx);
        cx.update(crate::components::text_input::init);
        cx.add_window_view(|window, cx| {
            let mailboxes = vec![
                mailbox(1, "Archive"),
                mailbox(2, "Projects"),
                mailbox(3, "Receipts"),
                mailbox(4, "Old projects"),
            ];
            let picker = cx.new(|cx| MovePicker::new(mailboxes, window, cx));
            let events = Rc::new(RefCell::new(Vec::new()));
            let picked = events.clone();
            let dismissed = events.clone();
            let subscriptions = vec![
                cx.subscribe(&picker, move |_, _, event: &MovePickerEvent, _| {
                    let MovePickerEvent::Picked(id) = event;
                    picked.borrow_mut().push(format!("picked {id}"));
                }),
                cx.subscribe(&picker, move |_, _, _: &DismissEvent, _| {
                    dismissed.borrow_mut().push("dismissed".to_owned());
                }),
            ];
            Root {
                picker,
                events,
                _subscriptions: subscriptions,
            }
        })
    }

    #[gpui::test]
    fn typing_filters_and_enter_picks_the_highlighted_folder(cx: &mut TestAppContext) {
        let (root, cx) = setup(cx);
        let picker = root.read_with(cx, |root, _| root.picker.clone());
        cx.run_until_parked();

        cx.simulate_input("pro");
        cx.run_until_parked();
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.matching_names().join(", ")),
            "Projects, Old projects",
            "names starting with the text come first"
        );

        press(cx, "down enter");
        cx.run_until_parked();
        let events = root.read_with(cx, |root, _| root.events.borrow().clone());
        assert_eq!(events, ["picked 4", "dismissed"]);
    }

    #[gpui::test]
    fn up_wraps_and_escape_closes_without_picking(cx: &mut TestAppContext) {
        let (root, cx) = setup(cx);
        let picker = root.read_with(cx, |root, _| root.picker.clone());
        cx.run_until_parked();

        press(cx, "up");
        assert_eq!(picker.read_with(cx, |picker, _| picker.highlighted), 3);
        cx.simulate_input("zzz");
        cx.run_until_parked();
        assert!(picker.read_with(cx, |picker, _| picker.matching_names().is_empty()));
        // Enter with nothing matching does nothing.
        press(cx, "enter escape");
        cx.run_until_parked();
        let events = root.read_with(cx, |root, _| root.events.borrow().clone());
        assert_eq!(events, ["dismissed"]);
    }
}
