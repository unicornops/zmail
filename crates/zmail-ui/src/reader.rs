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
//! More menu at its end holds Move to… (one entry per mailbox, until the
//! Phase 4 folder picker) and, when the pane is narrow, the actions that
//! no longer fit in the row.
//!
//! | Module | Contents |
//! |---|---|
//! | `actions` | The action bar's buttons and the More menu's entries |
//! | `conversation` | The conversation view: subject, metadata, messages |
//! | `message` | The message view: one message's header, status and body |

mod actions;
mod conversation;
mod message;

use gpui::{
    App, Context, DismissEvent, Entity, FocusHandle, Focusable, IntoElement, MouseButton, Render,
    ScrollHandle, Subscription, Window, div, point, prelude::*, px, rgb,
};
use zmail_core::{MailboxSummary, ThreadView, time::HourCycle};

use self::actions::{MoreItem, action_bar_buttons, more_menu_items};
use self::conversation::{render_reader_detail, render_selection_summary};
use crate::actions::{PageDown, PageUp, ScrollDown, ScrollUp};
use crate::components::{IconButton, Menu, MenuEntry, popover_below};
use crate::pane::pane_focus_indicator;
use crate::styles::UiColors;

/// The key context the reader sets.
pub(crate) const KEY_CONTEXT: &str = "Reader";

/// How far one arrow key scrolls the conversation.
const SCROLL_STEP: f32 = 48.;

/// Height of the reader's action bar.
const ACTION_BAR_HEIGHT: f32 = 48.0;

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
}

pub(crate) struct ReaderView {
    focus_handle: FocusHandle,
    hour_cycle: HourCycle,
    state: ReaderState,
    scroll_handle: ScrollHandle,
    /// The open More menu and the subscription that closes it.
    more_menu: Option<(Entity<Menu>, Subscription)>,
    /// Set when a press on the More button closed the open menu, so the
    /// click that follows does not reopen it.
    swallow_more_click: bool,
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
            scroll_handle: ScrollHandle::new(),
            more_menu: None,
            swallow_more_click: false,
            #[cfg(test)]
            counters: Default::default(),
        }
    }

    /// Replaces what the reader shows; re-renders only when it changed. A
    /// different thread starts at the top.
    pub(crate) fn set_state(&mut self, state: ReaderState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        let thread_changed = self.state.thread.as_ref().map(|thread| thread.id)
            != state.thread.as_ref().map(|thread| thread.id);
        self.state = state;
        if thread_changed {
            self.scroll_handle.set_offset(point(px(0.), px(0.)));
            self.more_menu = None;
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

    /// Whether the More menu is open.
    #[cfg(test)]
    pub(crate) fn more_menu(&self) -> Option<Entity<Menu>> {
        self.more_menu.as_ref().map(|(menu, _)| menu.clone())
    }

    /// Opens the More menu and gives it focus. Choosing an entry
    /// dispatches its action from the reader, like the buttons do.
    fn open_more_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = more_menu_items(self.state.thread.as_ref(), &self.state);
        if items.is_empty() {
            return;
        }
        let focus_handle = self.focus_handle.clone();
        let menu = Menu::build(window, cx, |mut menu| {
            for item in items {
                menu = match item {
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
                };
            }
            menu
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            |reader, menu, _: &DismissEvent, window, cx| {
                // An entry that moved focus elsewhere (none do yet) keeps
                // it; otherwise focus comes back to the reader.
                let refocus = menu.focus_handle(cx).contains_focused(window, cx)
                    || window.focused(cx).is_none();
                reader.more_menu = None;
                if refocus {
                    window.focus(&reader.focus_handle, cx);
                }
                cx.notify();
            },
        );
        window.focus(&menu.focus_handle(cx), cx);
        self.more_menu = Some((menu, subscription));
        cx.notify();
    }

    fn toggle_more_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.swallow_more_click) {
            return;
        }
        if self.more_menu.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        } else {
            self.open_more_menu(window, cx);
        }
    }

    fn scroll_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        let offset = self.scroll_handle.offset();
        let max = self.scroll_handle.max_offset().y;
        // Offsets grow negative as the content scrolls up.
        let y = (offset.y - px(delta)).clamp(-max, px(0.));
        self.scroll_handle.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn page_height(&self) -> f32 {
        let height: f32 = self.scroll_handle.bounds().size.height.into();
        (height - SCROLL_STEP).max(SCROLL_STEP)
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
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
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
                .when(thread.is_none(), |this| {
                    this.child(div().px_1().text_color(rgb(colors.muted)).child("Reader"))
                })
                .children(actions)
                .when(has_more, |this| {
                    this.child(div().flex_1()).child(
                        div()
                            .debug_selector(|| "reader-more".into())
                            .relative()
                            .flex_none()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|reader, _, _, _| {
                                    reader.swallow_more_click = reader.more_menu.is_some();
                                }),
                            )
                            .child(
                                IconButton::new("reader-more", "ellipsis", "More actions")
                                    .selected(self.more_menu.is_some())
                                    .on_click(cx.listener(|reader, _, window, cx| {
                                        reader.toggle_more_menu(window, cx);
                                    })),
                            )
                            .when_some(self.more_menu.as_ref(), |this, (menu, _)| {
                                this.child(popover_below(menu.clone()))
                            }),
                    )
                }),
            )
            .child(
                div()
                    .id("reader-scroll")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .px_4()
                    .py_4()
                    .child(if self.state.selected_count > 1 {
                        render_selection_summary(self.state.selected_count, &colors)
                    } else {
                        render_reader_detail(
                            thread,
                            self.hour_cycle,
                            &self.state.own_addresses,
                            &colors,
                        )
                    }),
            )
    }
}
