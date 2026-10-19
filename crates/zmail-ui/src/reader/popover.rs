//! The reader's popovers: the action bar's More menu, each message's More
//! menu and sender menu, and the Move to… folder picker any of them can
//! open. One popover is open at a time, below the button that opened it.
//!
//! Menu entries either dispatch an action from the reader, exactly like
//! the key binding, or run a [`ReaderCommand`] the reader handles itself.

use gpui::{
    AnyElement, ClipboardItem, Context, DismissEvent, Entity, Focusable, IntoElement, MouseButton,
    Subscription, Window, div, prelude::*,
};
use zmail_core::participants::sender_identity;

use super::ReaderView;
use super::actions::{
    MoreItem, ReaderCommand, message_menu_items, more_menu_items, move_destinations,
    sender_menu_items,
};
use crate::actions::MoveToMailbox;
use crate::components::{Menu, MenuEntry, popover_below, popover_below_end};
use crate::move_picker::{MovePicker, MovePickerEvent};

/// What opened a popover, and so where it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PopoverAnchor {
    /// The action bar's More button.
    ActionBar,
    /// The More button of the message at this index.
    MessageMore(usize),
    /// The sender's name on the message at this index.
    Sender(usize),
}

/// What an open popover holds.
pub(super) enum PopoverContent {
    Menu(Entity<Menu>),
    MovePicker(Entity<MovePicker>),
}

/// The open popover.
pub(super) struct OpenPopover {
    pub(super) anchor: PopoverAnchor,
    pub(super) content: PopoverContent,
    _subscriptions: Vec<Subscription>,
}

impl OpenPopover {
    /// The popover, placed below its trigger: left-aligned under a name,
    /// right-aligned under a button at the end of a row.
    pub(super) fn element(&self) -> AnyElement {
        let below_end = !matches!(self.anchor, PopoverAnchor::Sender(_));
        match &self.content {
            PopoverContent::Menu(menu) if below_end => {
                popover_below_end(menu.clone()).into_any_element()
            }
            PopoverContent::Menu(menu) => popover_below(menu.clone()).into_any_element(),
            PopoverContent::MovePicker(picker) => {
                popover_below_end(picker.clone()).into_any_element()
            }
        }
    }
}

impl ReaderView {
    /// The open popover's anchor.
    #[cfg(test)]
    pub(crate) fn popover_anchor(&self) -> Option<PopoverAnchor> {
        self.popover.as_ref().map(|popover| popover.anchor)
    }

    /// The open popover, if `anchor` opened it.
    pub(super) fn popover_at(&self, anchor: PopoverAnchor) -> Option<AnyElement> {
        self.popover
            .as_ref()
            .filter(|popover| popover.anchor == anchor)
            .map(OpenPopover::element)
    }

    /// Wraps a popover's trigger: it anchors the popover, and a press on
    /// it while its popover is open closes the popover without the click
    /// that follows reopening it.
    pub(super) fn popover_trigger(
        &self,
        anchor: PopoverAnchor,
        trigger: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .relative()
            .flex_none()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |reader, _, _, _| {
                    reader.swallow_click = reader
                        .popover
                        .as_ref()
                        .is_some_and(|popover| popover.anchor == anchor)
                        .then_some(anchor);
                }),
            )
            .child(trigger)
            .children(self.popover_at(anchor))
    }

    /// A click on a popover's trigger: closes the popover it opened, or
    /// opens it.
    pub(super) fn toggle_popover(
        &mut self,
        anchor: PopoverAnchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.swallow_click.take() == Some(anchor) {
            return;
        }
        if self
            .popover
            .as_ref()
            .is_some_and(|popover| popover.anchor == anchor)
        {
            self.close_popover(window, cx);
            return;
        }
        let Some(thread) = self.state.thread.as_ref() else {
            return;
        };
        let items = match anchor {
            PopoverAnchor::ActionBar => more_menu_items(Some(thread), &self.state),
            PopoverAnchor::MessageMore(index) => message_menu_items(thread, index, &self.state),
            PopoverAnchor::Sender(index) => thread
                .messages
                .get(index)
                .map(|message| sender_menu_items(&message.sender, index))
                .unwrap_or_default(),
        };
        self.open_menu(anchor, items, window, cx);
    }

    /// Closes the open popover and gives focus back to the reader.
    pub(super) fn close_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popover.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Opens a menu of `items` below `anchor` and gives it focus.
    fn open_menu(
        &mut self,
        anchor: PopoverAnchor,
        items: Vec<MoreItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let focus_handle = self.focus_handle.clone();
        let reader = cx.entity().downgrade();
        let menu = Menu::build(window, cx, |mut menu| {
            for item in items {
                menu =
                    match item {
                        MoreItem::Separator => menu.separator(),
                        MoreItem::Header(title) => menu.header(title),
                        MoreItem::Action(action) => {
                            let focus_handle = focus_handle.clone();
                            let mut entry = MenuEntry::new(action.label).icon(action.icon);
                            if let Some(shortcut) = action.shortcut {
                                entry = entry.shortcut(shortcut);
                            }
                            let command = action.action;
                            menu.entry(entry.on_select(move |window, cx| {
                                focus_handle.dispatch_action(command.as_ref(), window, cx);
                            }))
                        }
                        MoreItem::Command {
                            label,
                            icon,
                            command,
                        } => {
                            let reader = reader.clone();
                            menu.entry(MenuEntry::new(label).icon(icon).on_select(
                                move |window, cx| {
                                    let _ = reader.update(cx, |reader, cx| {
                                        reader.run_command(command, anchor, window, cx);
                                    });
                                },
                            ))
                        }
                    };
            }
            menu
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            |reader, menu, _: &DismissEvent, window, cx| {
                // The menu closes after its entry ran; an entry that opened
                // something else (the folder picker) keeps it, and focus.
                let still_open = reader.popover.as_ref().is_some_and(|popover| {
                    matches!(&popover.content, PopoverContent::Menu(open) if open == menu)
                });
                if !still_open {
                    return;
                }
                let refocus = menu.focus_handle(cx).contains_focused(window, cx)
                    || window.focused(cx).is_none();
                reader.popover = None;
                if refocus {
                    window.focus(&reader.focus_handle, cx);
                }
                cx.notify();
            },
        );
        window.focus(&menu.focus_handle(cx), cx);
        self.popover = Some(OpenPopover {
            anchor,
            content: PopoverContent::Menu(menu),
            _subscriptions: vec![subscription],
        });
        cx.notify();
    }

    /// Runs a menu's reader command. `anchor` is where its menu opened.
    pub(super) fn run_command(
        &mut self,
        command: ReaderCommand,
        anchor: PopoverAnchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.state.thread.as_ref() else {
            return;
        };
        let sender = |index: usize| {
            thread
                .messages
                .get(index)
                .map(|message| sender_identity(&message.sender))
        };
        match command {
            ReaderCommand::MoveTo => self.open_move_picker(anchor, window, cx),
            ReaderCommand::ViewSource(index) => self.open_source(index, window, cx),
            ReaderCommand::CopyAddress(index) => {
                if let Some((_, Some(email))) = sender(index) {
                    cx.write_to_clipboard(ClipboardItem::new_string(email));
                }
            }
            ReaderCommand::CopyNameAndAddress(index) => {
                if let Some((name, Some(email))) = sender(index) {
                    cx.write_to_clipboard(ClipboardItem::new_string(format!("{name} <{email}>")));
                }
            }
        }
    }

    /// Opens the Move to… picker below `anchor`. Picking a folder
    /// dispatches [`MoveToMailbox`] from the reader, so the shell moves
    /// the conversation.
    fn open_move_picker(
        &mut self,
        anchor: PopoverAnchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.state.thread.as_ref() else {
            return;
        };
        let destinations = move_destinations(thread, &self.state);
        if destinations.is_empty() {
            return;
        }
        let picker = cx.new(|cx| MovePicker::new(destinations, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &picker,
                window,
                |reader, _, event: &MovePickerEvent, window, cx| {
                    let MovePickerEvent::Picked(mailbox_id) = *event;
                    // The shell reads the reader while it moves the
                    // conversation, so dispatch once this update is over.
                    let focus_handle = reader.focus_handle.clone();
                    window.defer(cx, move |window, cx| {
                        focus_handle.dispatch_action(&MoveToMailbox { mailbox_id }, window, cx);
                    });
                },
            ),
            cx.subscribe_in(
                &picker,
                window,
                |reader, _, _: &DismissEvent, window, cx| {
                    reader.close_popover(window, cx);
                },
            ),
        ];
        window.focus(&picker.focus_handle(cx), cx);
        self.popover = Some(OpenPopover {
            anchor,
            content: PopoverContent::MovePicker(picker),
            _subscriptions: subscriptions,
        });
        cx.notify();
    }
}
