//! The reader pane: the open conversation and its actions.
//!
//! [`ReaderView`] is its own entity with its own focus handle and the
//! `Reader` key context. The shell pushes the selected thread (loaded once
//! per selection or store refresh, never per frame) with
//! [`ReaderView::set_state`]. Its buttons dispatch the same actions as the
//! key bindings; the shell handles them, because replying and moving need
//! the selection and the account.
//!
//! The actions are one row of icon buttons above the conversation. The
//! More menu at its end holds Move to… (the searchable folder picker) and,
//! when the pane is narrow, the actions that no longer fit in the row.
//! Each expanded message has its own Reply and More buttons too.
//!
//! The conversation below is a variable-height `list`, so a long thread
//! only lays out the messages on screen. n and p move between messages and
//! Enter expands or collapses the current one.
//!
//! | Module | Contents |
//! |---|---|
//! | `actions` | The action bar's buttons and the More menu's entries |
//! | `conversation` | The conversation list: its items and which messages are expanded |
//! | `header` | An expanded message's header: sender, recipients, date, Reply and More |
//! | `message` | The message view: one message, expanded or as a summary line |
//! | `popover` | The More menus, the sender menu and the Move to… picker |
//! | `quoted` | Finds the quoted text and signature that fold |
//! | `source` | The View Plain Text / View Source dialog |
//! | `body` | A message body: paragraphs at a readable measure, the selection |
//! | `selectable` | `SelectableText`: a paragraph the mouse can select |
//! | `links` | A body's links and the dialog that opens one |
//! | `banners` | The remote-content notice and the sender-safety banners |

mod actions;
mod attachments;
mod banners;
mod body;
mod conversation;
mod header;
mod links;
mod message;
mod popover;
mod quoted;
mod selectable;
mod source;

use std::{collections::HashMap, rc::Rc};

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ListAlignment, ListOffset,
    ListState, MouseButton, Render, Role, Subscription, Window, div, list, prelude::*, px, rgb,
};
use zmail_core::{MailboxSummary, ThreadView, preferences::SenderWarnings, time::HourCycle};

use self::actions::{action_bar_buttons, more_menu_items};
use self::body::{BodyContext, BodyModel, BodySelection};
use self::conversation::{
    ConversationItem, ConversationState, render_conversation_header, render_no_conversation,
    render_selection_summary,
};
use self::links::LinkPrompt;
use self::message::{MessageContext, MessageDisplay, render_message};
use self::popover::OpenPopover;
pub(crate) use self::popover::PopoverAnchor;
use self::source::SourceView;
use crate::a11y::AccessibleExt;
use crate::actions::{
    Forward, NextMessage, PageDown, PageUp, PreviousMessage, Reply, ReplyAll, ScrollDown, ScrollUp,
    ToggleMessage,
};
use crate::components::{Button, IconButton, ModalState};
use crate::compose::ComposeView;
use crate::pane::{dispatch_from, pane_focus_indicator};
use crate::styles::UiColors;

/// The key context the reader sets.
pub(crate) const KEY_CONTEXT: &str = "Reader";

/// How far one arrow key scrolls the conversation.
const SCROLL_STEP: f32 = 48.;

/// Height of the reader's action bar.
const ACTION_BAR_HEIGHT: f32 = 48.0;

/// How far beyond the visible messages the conversation list lays out, so
/// scrolling does not show messages popping in.
const LIST_OVERDRAW: f32 = 400.;

/// What the shell shows in the reader.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ReaderState {
    pub(crate) thread: Option<ThreadView>,
    /// The account's mailboxes, for the move buttons.
    pub(crate) mailboxes: Vec<MailboxSummary>,
    /// The user's own addresses, shown as "me".
    pub(crate) own_addresses: Vec<String>,
    /// The pane is too narrow for every action button: only Reply,
    /// Archive and Delete stay in the row, the rest move to More.
    pub(crate) compact_actions: bool,
    /// How many conversations the list has selected. With more than one,
    /// the reader shows the count instead of a conversation.
    pub(crate) selected_count: usize,
    /// The inline reply to this conversation, shown below its last
    /// message. Without one, the footer offers Reply, Reply All and
    /// Forward.
    pub(crate) reply: Option<Entity<ComposeView>>,
    /// Which sender-safety warnings show (Settings → Privacy).
    pub(crate) sender_warnings: SenderWarnings,
}

pub(crate) struct ReaderView {
    focus_handle: FocusHandle,
    hour_cycle: HourCycle,
    state: ReaderState,
    /// The conversation list's scroll position and measured item heights.
    list_state: ListState,
    /// Which messages are expanded and which is current.
    conversation: ConversationState,
    /// The open menu or folder picker.
    popover: Option<OpenPopover>,
    /// Set when a press on a popover's trigger closed it, so the click
    /// that follows does not reopen it.
    swallow_click: Option<PopoverAnchor>,
    /// The View Plain Text / View Source dialog.
    source: Option<SourceView>,
    source_modal: ModalState,
    /// Each message's prepared body, while the conversation is open.
    bodies: HashMap<i64, Rc<BodyModel>>,
    /// The selected text, inside one message's body.
    selection: Option<BodySelection>,
    /// The mouse is dragging out the selection.
    dragging: bool,
    /// A paragraph took the current mouse press, so the reader's own press
    /// handler keeps the selection it started.
    press_selected: bool,
    /// The open-link dialog.
    link_prompt: Option<LinkPrompt>,
    link_modal: ModalState,
    /// Re-measures the footer whenever the inline reply changes, since it
    /// grows with its text.
    reply_observation: Option<Subscription>,
    /// Test instrumentation: renders.
    #[cfg(test)]
    pub(crate) counters: crate::pane::RenderCounters,
}

impl Focusable for ReaderView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ReaderView {
    pub(crate) fn new(hour_cycle: HourCycle, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            hour_cycle,
            state: ReaderState::default(),
            list_state: ListState::new(0, ListAlignment::Top, px(LIST_OVERDRAW)),
            conversation: ConversationState::default(),
            popover: None,
            swallow_click: None,
            source: None,
            source_modal: ModalState::new(cx),
            bodies: HashMap::new(),
            selection: None,
            dragging: false,
            press_selected: false,
            link_prompt: None,
            link_modal: ModalState::new(cx),
            reply_observation: None,
            #[cfg(test)]
            counters: Default::default(),
        }
    }

    /// Replaces what the reader shows; re-renders only when it changed. A
    /// different thread starts at the top, with its latest and unread
    /// messages expanded.
    pub(crate) fn set_state(&mut self, state: ReaderState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        let old_count = self.list_state.item_count();
        if self.state.reply != state.reply {
            self.reply_observation = state.reply.as_ref().map(|reply| {
                cx.observe(reply, |reader, _, cx| {
                    let footer = reader.list_state.item_count().saturating_sub(1);
                    reader.list_state.remeasure_items(footer..footer + 1);
                    cx.notify();
                })
            });
        }
        self.state = state;
        let thread = self.state.thread.as_ref();
        let count = thread.map_or(0, |thread| ConversationItem::count(thread.messages.len()));
        if self.conversation.is_for(thread) {
            if let Some(thread) = thread {
                self.conversation.update(thread);
                // A body that changed (downloaded since) is prepared again.
                self.bodies.retain(|id, body| {
                    thread
                        .messages
                        .iter()
                        .any(|message| message.id == *id && message.rendered_body == *body.text)
                });
            }
            // The messages may have changed height (a body downloaded, a
            // message arrived); the scroll position stays.
            if count == old_count {
                self.list_state.remeasure();
            } else {
                self.list_state.splice(0..old_count, count);
            }
        } else {
            self.conversation = ConversationState::open(thread);
            self.list_state.reset(count);
            self.popover = None;
            self.bodies.clear();
            self.selection = None;
            self.dragging = false;
        }
        cx.notify();
    }

    /// What the reader shows.
    #[cfg(test)]
    pub(crate) fn state(&self) -> &ReaderState {
        &self.state
    }

    /// Whether the action bar is compact.
    #[cfg(test)]
    pub(crate) fn compact_actions(&self) -> bool {
        self.state.compact_actions
    }

    /// The action bar's More menu, while it is open.
    #[cfg(test)]
    pub(crate) fn more_menu(&self) -> Option<gpui::Entity<crate::components::Menu>> {
        self.popover
            .as_ref()
            .filter(|popover| popover.anchor == PopoverAnchor::ActionBar)
            .and_then(|popover| match &popover.content {
                popover::PopoverContent::Menu(menu) => Some(menu.clone()),
                popover::PopoverContent::MovePicker(_) => None,
            })
    }

    /// Whether `anchor` opened the open popover.
    fn popover_anchor_is(&self, anchor: PopoverAnchor) -> bool {
        self.popover
            .as_ref()
            .is_some_and(|popover| popover.anchor == anchor)
    }

    /// Which messages are expanded and which is current.
    #[cfg(test)]
    pub(crate) fn conversation(&self) -> &ConversationState {
        &self.conversation
    }

    /// Scrolls the inline reply into view.
    pub(crate) fn reveal_reply(&mut self, cx: &mut Context<Self>) {
        let count = self.list_state.item_count();
        if count > 0 {
            self.list_state.scroll_to_reveal_item(count - 1);
            cx.notify();
        }
    }

    /// Expands every message of the open conversation.
    pub(crate) fn expand_all(&mut self, cx: &mut Context<Self>) {
        if let Some(thread) = self.state.thread.as_ref() {
            self.conversation.expand_all(thread);
            self.list_state.remeasure();
            cx.notify();
        }
    }

    fn message_count(&self) -> usize {
        self.state
            .thread
            .as_ref()
            .map_or(0, |thread| thread.messages.len())
    }

    /// Expands or collapses the message at `index` and makes it current.
    fn toggle_message(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(message) = self
            .state
            .thread
            .as_ref()
            .and_then(|thread| thread.messages.get(index))
        else {
            return;
        };
        self.conversation.toggle_expanded(message.id);
        self.conversation.set_current(index);
        let item = ConversationItem::message_index(index);
        self.list_state.remeasure_items(item..item + 1);
        cx.notify();
    }

    /// Shows or hides the full From, To and Date lines of the message at
    /// `index`.
    fn toggle_details(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(message) = self
            .state
            .thread
            .as_ref()
            .and_then(|thread| thread.messages.get(index))
        else {
            return;
        };
        self.conversation.toggle_details(message.id);
        let item = ConversationItem::message_index(index);
        self.list_state.remeasure_items(item..item + 1);
        cx.notify();
    }

    /// Shows or hides the quoted text of the message at `index`.
    fn toggle_fold(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(message) = self
            .state
            .thread
            .as_ref()
            .and_then(|thread| thread.messages.get(index))
        else {
            return;
        };
        self.conversation.toggle_unfolded(message.id);
        let item = ConversationItem::message_index(index);
        self.list_state.remeasure_items(item..item + 1);
        cx.notify();
    }

    /// Makes the message at `index` current and scrolls it to the top.
    fn go_to_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.conversation.set_current(index);
        self.list_state.scroll_to(ListOffset {
            item_ix: ConversationItem::message_index(index),
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn next_message(&mut self, _: &NextMessage, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.message_count();
        if count == 0 {
            return;
        }
        let next = self
            .conversation
            .current()
            .map_or(0, |current| (current + 1).min(count - 1));
        self.go_to_message(next, cx);
    }

    fn previous_message(&mut self, _: &PreviousMessage, _: &mut Window, cx: &mut Context<Self>) {
        if self.message_count() == 0 {
            return;
        }
        let previous = self
            .conversation
            .current()
            .map_or(0, |current| current.saturating_sub(1));
        self.go_to_message(previous, cx);
    }

    fn toggle_current_message(
        &mut self,
        _: &ToggleMessage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(current) = self.conversation.current() {
            self.toggle_message(current, cx);
        }
    }

    fn scroll_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.list_state.scroll_by(px(delta));
        cx.notify();
    }

    fn page_height(&self) -> f32 {
        let height: f32 = self.list_state.viewport_bounds().size.height.into();
        (height - SCROLL_STEP).max(SCROLL_STEP)
    }

    /// One item of the conversation list.
    fn render_item(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = *cx.global::<UiColors>();
        let Some(thread) = self.state.thread.clone() else {
            return div().into_any_element();
        };
        let thread = &thread;
        match ConversationItem::at(index, thread.messages.len()) {
            ConversationItem::Header => render_conversation_header(thread, &colors),
            ConversationItem::Message(index) => {
                let message = &thread.messages[index];
                let model = self.body_model(message);
                let context = MessageContext {
                    now: chrono::Local::now(),
                    hour_cycle: self.hour_cycle,
                    own_addresses: &self.state.own_addresses,
                    colors,
                    focused: self.focus_handle.contains_focused(window, cx),
                    sender_warnings: self.state.sender_warnings,
                    message_count: thread.messages.len(),
                };
                let display = MessageDisplay {
                    index,
                    expanded: self.conversation.is_expanded(message.id),
                    unfolded: self.conversation.is_unfolded(message.id),
                    details: self.conversation.shows_details(message.id),
                    current: self.conversation.current() == Some(index),
                };
                let body = BodyContext {
                    message_id: message.id,
                    index,
                    model: &model,
                    selection: self.selection_in(message.id),
                    dragging: self.dragging
                        && self
                            .selection
                            .is_some_and(|selection| selection.message_id == message.id),
                    colors,
                };
                render_message(self, message, display, &context, &body, cx)
            }
            // The inline reply, or the buttons that start one.
            ConversationItem::Footer => div()
                .debug_selector(|| "reader-footer".into())
                .px_4()
                .pt_2()
                .pb(px(SCROLL_STEP))
                .child(match self.state.reply.clone() {
                    Some(reply) => div()
                        .debug_selector(|| "reader-inline-reply".into())
                        .child(reply)
                        .into_any_element(),
                    None => render_reply_bar(&self.focus_handle).into_any_element(),
                })
                .into_any_element(),
        }
    }

    fn scroll_up(&mut self, _: &ScrollUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-SCROLL_STEP, cx);
    }

    fn scroll_down(&mut self, _: &ScrollDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(SCROLL_STEP, cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-self.page_height(), cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.page_height(), cx);
    }
}

/// The footer's buttons when no reply is open: Reply opens the inline
/// reply below the conversation.
fn render_reply_bar(focus_handle: &FocusHandle) -> impl IntoElement {
    let button = |id: &'static str, label: &'static str, icon: &'static str| {
        Button::new(id, label).icon(icon)
    };
    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .child(
            div().debug_selector(|| "reader-footer-reply".into()).child(
                button("reader-footer-reply", "Reply", "reply")
                    .on_click(dispatch_from(focus_handle, Reply)),
            ),
        )
        .child(
            button("reader-footer-reply-all", "Reply All", "reply-all")
                .on_click(dispatch_from(focus_handle, ReplyAll)),
        )
        .child(
            button("reader-footer-forward", "Forward", "forward")
                .on_click(dispatch_from(focus_handle, Forward)),
        )
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.counters.renders += 1;
        }
        let colors = *cx.global::<UiColors>();
        let focused = self.focus_handle.contains_focused(window, cx);
        let thread = self.state.thread.as_ref();
        let actions = action_bar_buttons(
            thread,
            self.state.compact_actions,
            &self.focus_handle,
            &colors,
        );
        let has_more = !more_menu_items(thread, &self.state).is_empty();

        div()
            .id("reader")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .accessible(Role::Pane, "Reader")
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::next_message))
            .on_action(cx.listener(Self::previous_message))
            .on_action(cx.listener(Self::toggle_current_message))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            // After any paragraph's own press handler: a press no
            // paragraph took clears the selection.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|reader, _, _, cx| reader.press_outside_text(cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            // ---- Action bar: one row, never wrapping ---------------------
            .child(
                pane_focus_indicator(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .min_h(px(ACTION_BAR_HEIGHT))
                        .px_3()
                        .overflow_hidden()
                        .border_b_1()
                        .border_color(rgb(colors.border)),
                    focused,
                    &colors,
                )
                .id("reader-actions")
                .accessible(Role::Toolbar, "Conversation actions")
                .when(thread.is_none(), |this| {
                    this.child(div().px_1().text_color(rgb(colors.muted)).child("Reader"))
                })
                .children(actions)
                .when(has_more, |this| {
                    this.child(div().flex_1()).child(
                        div().debug_selector(|| "reader-more".into()).child(
                            self.popover_trigger(
                                PopoverAnchor::ActionBar,
                                IconButton::new("reader-more", "ellipsis", "More actions")
                                    .selected(self.popover_anchor_is(PopoverAnchor::ActionBar))
                                    .on_click(cx.listener(|reader, _, window, cx| {
                                        reader.toggle_popover(PopoverAnchor::ActionBar, window, cx);
                                    })),
                                cx,
                            ),
                        ),
                    )
                }),
            )
            .child(if self.state.selected_count > 1 {
                render_selection_summary(self.state.selected_count, &colors)
            } else if thread.is_none() {
                render_no_conversation(&colors)
            } else {
                list(
                    self.list_state.clone(),
                    cx.processor(|reader, index, window, cx| reader.render_item(index, window, cx)),
                )
                .flex_1()
                .w_full()
                .into_any_element()
            })
            .children(self.render_source(&colors, cx))
            .children(self.render_link_prompt(&colors, cx))
    }
}
