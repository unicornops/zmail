//! The sidebar pane: accounts and the selected account's mailboxes.
//!
//! [`SidebarView`] is its own entity with its own focus handle and the
//! `Sidebar` key context. The workspace shell pushes the accounts,
//! mailboxes and selection in with [`SidebarView::set_state`]; the sidebar
//! reports clicks and keyboard selection as [`SidebarEvent`]s and never
//! touches the store itself.
//!
//! Drafts and Outbox sit at the bottom, under "Outgoing", until the
//! sidebar's Favourites section (Phase 3 of the UI plan, #117) replaces
//! them.

use std::ops::Range;

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    SharedString, StatefulInteractiveElement, UniformListScrollHandle, Window, div, prelude::*, px,
    rgb, uniform_list,
};
use zmail_core::{AccountSummary, MailboxSummary};

use crate::actions::{
    AddAccount, OpenSelected, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
use crate::components::{ButtonSize, IconButton};
use crate::pane::{dispatch_from, pane_focus_indicator};
use crate::styles::{AVATAR_SIZE, ICON_SIZE_SM, MAILBOX_ITEM_HEIGHT, UiColors};
use crate::widgets::{
    EllipsisExt, HoverExt, avatar, badge, icon, interactive_background, message_avatar_background,
    sync_badge,
};

/// The key context the sidebar sets.
pub(crate) const KEY_CONTEXT: &str = "Sidebar";

/// What the shell shows in the sidebar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SidebarState {
    pub(crate) accounts: Vec<AccountSummary>,
    pub(crate) selected_account_id: Option<i64>,
    pub(crate) mailboxes: Vec<MailboxSummary>,
    pub(crate) selected_mailbox_id: Option<i64>,
    /// Which row shows as the current view.
    pub(crate) highlight: SidebarHighlight,
    /// Number of drafts, shown beside the Drafts row.
    pub(crate) draft_count: usize,
    /// Number of messages waiting in the outbox.
    pub(crate) outbox_count: usize,
}

/// The sidebar row that matches what the message list shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SidebarHighlight {
    /// The selected mailbox.
    #[default]
    Mailbox,
    /// The Drafts row.
    Drafts,
    /// The Outbox row.
    Outbox,
    /// Nothing (search results are not a sidebar view).
    None,
}

/// What the user did in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarEvent {
    /// An account row was clicked or reached with the arrow keys.
    SelectAccount(i64),
    /// A mailbox row was clicked or reached with the arrow keys.
    SelectMailbox(i64),
    /// The Drafts row was clicked or reached with the arrow keys.
    ShowDrafts,
    /// The Outbox row was clicked or reached with the arrow keys.
    ShowOutbox,
    /// Enter: move on to the message list.
    Activate,
    /// A mailbox, Drafts or Outbox row was clicked (after its select
    /// event). The sidebar overlay closes on it.
    RowClicked,
}

/// One keyboard stop in the sidebar: the accounts, the selected
/// account's mailboxes, then Drafts and Outbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarItem {
    Account(i64),
    Mailbox(i64),
    Drafts,
    Outbox,
}

pub(crate) struct SidebarView {
    focus_handle: FocusHandle,
    state: SidebarState,
    /// The item the arrow keys move from: the last one picked in the
    /// sidebar. It falls back to the selected mailbox.
    cursor: Option<SidebarItem>,
    mailbox_scroll: UniformListScrollHandle,
    /// Test instrumentation: renders.
    #[cfg(test)]
    pub(crate) counters: crate::pane::RenderCounters,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl Focusable for SidebarView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SidebarView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            state: SidebarState::default(),
            cursor: None,
            mailbox_scroll: UniformListScrollHandle::new(),
            #[cfg(test)]
            counters: Default::default(),
        }
    }

    /// Replaces what the sidebar shows; re-renders only when it changed.
    pub(crate) fn set_state(&mut self, state: SidebarState, cx: &mut Context<Self>) {
        if self.state != state {
            self.state = state;
            if self
                .cursor
                .is_some_and(|cursor| self.index_of(cursor).is_none())
            {
                self.cursor = None;
            }
            cx.notify();
        }
    }

    // -----------------------------------------------------------------------
    // Keyboard selection
    // -----------------------------------------------------------------------

    fn items(&self) -> Vec<SidebarItem> {
        self.state
            .accounts
            .iter()
            .map(|account| SidebarItem::Account(account.id))
            .chain(
                self.state
                    .mailboxes
                    .iter()
                    .map(|mailbox| SidebarItem::Mailbox(mailbox.id)),
            )
            .chain([SidebarItem::Drafts, SidebarItem::Outbox])
            .collect()
    }

    fn index_of(&self, item: SidebarItem) -> Option<usize> {
        self.items().iter().position(|candidate| *candidate == item)
    }

    fn current_index(&self) -> Option<usize> {
        self.cursor
            .and_then(|cursor| self.index_of(cursor))
            .or_else(|| {
                self.state
                    .selected_mailbox_id
                    .and_then(|id| self.index_of(SidebarItem::Mailbox(id)))
            })
            .or_else(|| {
                self.state
                    .selected_account_id
                    .and_then(|id| self.index_of(SidebarItem::Account(id)))
            })
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items().get(index).copied() else {
            return;
        };
        self.cursor = Some(item);
        match item {
            SidebarItem::Account(id) => cx.emit(SidebarEvent::SelectAccount(id)),
            SidebarItem::Mailbox(id) => {
                let account_count = self.state.accounts.len();
                self.mailbox_scroll
                    .scroll_to_item(index - account_count, gpui::ScrollStrategy::Nearest);
                cx.emit(SidebarEvent::SelectMailbox(id));
            }
            SidebarItem::Drafts => cx.emit(SidebarEvent::ShowDrafts),
            SidebarItem::Outbox => cx.emit(SidebarEvent::ShowOutbox),
        }
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.items().len();
        let next = match self.current_index() {
            Some(index) => (index + 1).min(count.saturating_sub(1)),
            None => 0,
        };
        self.select_index(next, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let previous = self
            .current_index()
            .map(|index| index.saturating_sub(1))
            .unwrap_or(0);
        self.select_index(previous, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.select_index(0, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.items().len();
        if count > 0 {
            self.select_index(count - 1, cx);
        }
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(SidebarEvent::Activate);
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.counters.renders += 1;
        }
        let colors = *cx.global::<UiColors>();
        let focused = self.focus_handle.contains_focused(window, cx);

        div()
            .id("sidebar")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::open_selected))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.surface))
            .border_r_1()
            .border_color(rgb(colors.border))
            // ---- Accounts header -----------------------------------------
            .child(
                pane_focus_indicator(
                    div()
                        .px_3()
                        .py_3()
                        .border_b_1()
                        .border_color(rgb(colors.border)),
                    focused,
                    &colors,
                )
                .flex()
                .items_center()
                .justify_between()
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Accounts"))
                .child(
                    IconButton::new("sidebar-add-account", "plus", "Add Account")
                        .size(ButtonSize::Small)
                        .shortcut("secondary-shift-a")
                        .on_click(dispatch_from(&self.focus_handle, AddAccount)),
                ),
            )
            // ---- Account items -------------------------------------------
            .child(
                div()
                    .flex()
                    .flex_col()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .children(
                        self.state
                            .accounts
                            .iter()
                            .map(|account| self.render_account(account, &colors, cx)),
                    ),
            )
            // ---- Mailboxes header ----------------------------------------
            .child(
                div()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Mailboxes")),
            )
            // ---- Virtualised mailbox list --------------------------------
            .child(
                div().flex_1().overflow_hidden().child(
                    uniform_list(
                        "mailboxes",
                        self.state.mailboxes.len(),
                        cx.processor(move |this, range: Range<usize>, _window, cx| {
                            this.render_mailboxes(range, &colors, cx)
                        }),
                    )
                    .track_scroll(&self.mailbox_scroll)
                    .h_full(),
                ),
            )
            // ---- Drafts and Outbox ---------------------------------------
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .child(
                        div()
                            .px_3()
                            .py_3()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Outgoing"),
                    )
                    .child(self.render_view_row(SidebarItem::Drafts, &colors, cx))
                    .child(self.render_view_row(SidebarItem::Outbox, &colors, cx)),
            )
    }
}

impl SidebarView {
    fn render_account(
        &self,
        account: &AccountSummary,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let account_id = account.id;
        let is_selected = Some(account_id) == self.state.selected_account_id;
        let background_color = rgb(interactive_background(is_selected, colors.surface, colors));
        let (badge_icon, badge_color, badge_text) = sync_badge(account.sync_status.phase, colors);
        let avatar_color = message_avatar_background(&account.display_name, colors);

        div()
            .id(account_row_id(account_id))
            .debug_selector(move || format!("account-row-{account_id}"))
            .px_3()
            .py_3()
            .cursor_pointer()
            .border_b_1()
            .border_color(rgb(colors.border))
            .bg(background_color)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.cursor = Some(SidebarItem::Account(account_id));
                cx.emit(SidebarEvent::SelectAccount(account_id));
            }))
            .hover_bg(is_selected, colors)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_start()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_start()
                            .gap_3()
                            .child(avatar(
                                &account.display_name,
                                AVATAR_SIZE,
                                avatar_color,
                                colors,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().ellipsis().child(account.display_name.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(colors.muted))
                                            .ellipsis()
                                            .child(account.username.clone()),
                                    )
                                    .children((account.sync_status.failure_count > 0).then(|| {
                                        div()
                                            .text_sm()
                                            .text_color(rgb(colors.danger))
                                            .ellipsis()
                                            .child(format!(
                                                "Sync error: {}",
                                                account
                                                    .sync_status
                                                    .last_error_message
                                                    .as_deref()
                                                    .unwrap_or("unknown"),
                                            ))
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_full()
                            .bg(rgb(colors.button_background))
                            .child(icon(badge_icon, ICON_SIZE_SM, badge_color))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(badge_color))
                                    .child(badge_text),
                            ),
                    ),
            )
    }

    /// The Drafts or Outbox row.
    fn render_view_row(
        &self,
        item: SidebarItem,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (id, label, icon_name, count, highlight, event) = match item {
            SidebarItem::Outbox => (
                "sidebar-outbox",
                "Outbox",
                "send",
                self.state.outbox_count,
                SidebarHighlight::Outbox,
                SidebarEvent::ShowOutbox,
            ),
            _ => (
                "sidebar-drafts",
                "Drafts",
                "file-text",
                self.state.draft_count,
                SidebarHighlight::Drafts,
                SidebarEvent::ShowDrafts,
            ),
        };
        let is_selected = self.state.highlight == highlight;
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(MAILBOX_ITEM_HEIGHT))
            .px_3()
            .cursor_pointer()
            .bg(rgb(interactive_background(
                is_selected,
                colors.surface,
                colors,
            )))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.cursor = Some(item);
                cx.emit(event);
                cx.emit(SidebarEvent::RowClicked);
            }))
            .hover_bg(is_selected, colors)
            .child(icon(icon_name, ICON_SIZE_SM, colors.muted))
            .child(div().flex_1().min_w_0().ellipsis().child(label))
            .when(count > 0, |this| {
                this.child(badge(count, colors.button_background, colors.foreground))
            })
    }

    fn render_mailboxes(
        &mut self,
        range: Range<usize>,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Stateful<gpui::Div>> {
        let mut items = Vec::new();
        for index in range {
            let Some(mailbox) = self.state.mailboxes.get(index) else {
                continue;
            };
            let mailbox_id = mailbox.id;
            let is_selected = Some(mailbox_id) == self.state.selected_mailbox_id
                && self.state.highlight == SidebarHighlight::Mailbox;
            let background_color = rgb(interactive_background(is_selected, colors.surface, colors));

            items.push(
                div()
                    .id(mailbox_row_id(mailbox_id))
                    .debug_selector(move || format!("mailbox-row-{mailbox_id}"))
                    .w_full()
                    .h(px(MAILBOX_ITEM_HEIGHT))
                    .px_3()
                    .items_center()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .bg(background_color)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cursor = Some(SidebarItem::Mailbox(mailbox_id));
                        cx.emit(SidebarEvent::SelectMailbox(mailbox_id));
                        cx.emit(SidebarEvent::RowClicked);
                    }))
                    .hover_bg(is_selected, colors)
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .flex_1()
                                    .min_w_0()
                                    .child(icon(
                                        mailbox_icon_name(&mailbox.name),
                                        ICON_SIZE_SM,
                                        colors.muted,
                                    ))
                                    .child(div().ellipsis().child(mailbox.name.clone())),
                            )
                            .when(mailbox.unread_threads > 0, |this| {
                                this.child(badge(
                                    mailbox.unread_threads,
                                    colors.accent,
                                    colors.background,
                                ))
                            }),
                    ),
            );
        }
        items
    }
}

fn account_row_id(account_id: i64) -> SharedString {
    SharedString::from(format!("account-row-{account_id}"))
}

fn mailbox_row_id(mailbox_id: i64) -> SharedString {
    SharedString::from(format!("mailbox-row-{mailbox_id}"))
}

fn mailbox_icon_name(name: &str) -> &'static str {
    let normalized = name.trim().to_ascii_lowercase();

    if normalized == "inbox" {
        "inbox"
    } else if normalized == "sent" || normalized == "sent items" {
        "send"
    } else if normalized == "drafts" {
        "file-text"
    } else if normalized == "archive" {
        "archive"
    } else if normalized == "trash"
        || normalized == "junk"
        || normalized == "spam"
        || normalized == "junk email"
    {
        "trash-2"
    } else {
        "folder"
    }
}
