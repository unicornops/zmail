//! The sidebar pane: inboxes, favourites and every account's folders.
//!
//! [`SidebarView`] is its own entity with its own focus handle and the
//! `Sidebar` key context. The workspace shell pushes the accounts,
//! mailboxes, counts, selection and collapsed folders in with
//! [`SidebarView::set_state`]; the sidebar reports clicks and keyboard
//! selection as [`SidebarEvent`]s and never touches the store itself.
//!
//! It is one scrolling list in three parts:
//!
//! - **Inboxes**: the Unified Inbox and each account's Inbox (just "Inbox"
//!   when there is one account).
//! - **Favourites**: Flagged, Drafts, Sent and Outbox ("3 waiting", with
//!   Retry when a message failed).
//! - **Accounts**: a header per account (colour dot, name, sync status and
//!   "Fix…" on errors) over its collapsible folder tree.
//!
//! Unread badges sit on inboxes and favourites only.

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    Render, ScrollHandle, SharedString, Stateful, StatefulInteractiveElement, Window, div,
    prelude::*, px, rgb,
};
use zmail_core::{
    AccountSummary, MailboxRole, MailboxSummary, SyncPhase, VirtualMailbox, mailbox_leaf_name,
    preferences::{Density, FolderRef, SidebarTree},
};

use crate::actions::{
    AddAccount, OpenSelected, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
use crate::components::{
    Badge, BadgeTone, Button, ButtonSize, ButtonVariant, IconButton, Scrollbar,
};
use crate::drag::DraggedThreads;
use crate::pane::{dispatch_from, pane_focus_indicator};
use crate::styles::{DensityTokens, ICON_SIZE_SM, SIDEBAR_TREE_INDENT, UiColors};
use crate::tooltip::text_tooltip;
use crate::widgets::{HoverExt, account_dot, icon, interactive_background, sync_badge};

/// The key context the sidebar sets.
pub(crate) const KEY_CONTEXT: &str = "Sidebar";

/// What the shell shows in the sidebar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SidebarState {
    pub(crate) accounts: Vec<AccountSummary>,
    /// Every account's mailboxes, in `LocalStore::list_all_mailboxes`
    /// order (account by account, roles first).
    pub(crate) mailboxes: Vec<MailboxSummary>,
    /// Which row shows as the current view.
    pub(crate) highlight: SidebarHighlight,
    /// Unread conversations in Flagged.
    pub(crate) flagged_unread: usize,
    /// Number of drafts, shown beside the Drafts row.
    pub(crate) draft_count: usize,
    /// What the outbox holds.
    pub(crate) outbox: OutboxStatus,
    /// Collapsed accounts and folders.
    pub(crate) tree: SidebarTree,
    /// Row density.
    pub(crate) density: Density,
}

/// Outbox counts for the Outbox row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OutboxStatus {
    /// Messages not sent yet.
    pub(crate) waiting: usize,
    /// Messages whose last attempt failed; Retry sends them again.
    pub(crate) failed: usize,
}

/// The sidebar row that matches what the message list shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SidebarHighlight {
    /// A stored mailbox (the Inbox row or a folder).
    Mailbox(i64),
    /// The Unified Inbox, Flagged or Sent.
    Virtual(VirtualMailbox),
    /// The Drafts row.
    Drafts,
    /// The Outbox row.
    Outbox,
    /// Nothing (search results are not a sidebar view).
    #[default]
    None,
}

/// What the user did in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarEvent {
    /// An inbox or folder row was clicked or reached with the arrow keys.
    SelectMailbox(i64),
    /// The Unified Inbox, Flagged or Sent row was picked.
    SelectVirtual(VirtualMailbox),
    /// The Drafts row was clicked or reached with the arrow keys.
    ShowDrafts,
    /// The Outbox row was clicked or reached with the arrow keys.
    ShowOutbox,
    /// The Outbox row's Retry button: send the failed messages again.
    RetryOutbox,
    /// An account or folder was expanded or collapsed.
    ToggleCollapsed(TreeNode),
    /// An account's "Fix…" button.
    FixAccount(i64),
    /// Conversations dragged from the message list were dropped on a
    /// folder: move them there.
    DropThreads {
        thread_ids: Vec<i64>,
        mailbox_id: i64,
    },
    /// Enter: move on to the message list.
    Activate,
    /// A row that shows a list was clicked (after its select event). The
    /// sidebar overlay closes on it.
    RowClicked,
}

/// A node of the sidebar that collapses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TreeNode {
    /// An account's folder tree.
    Account(i64),
    /// A folder's subfolders.
    Folder(FolderRef),
}

/// One keyboard stop in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarItem {
    /// The Unified Inbox, Flagged or Sent.
    Virtual(VirtualMailbox),
    /// An account's Inbox in the Inboxes section.
    Inbox(i64),
    Drafts,
    Outbox,
    /// An account's header.
    Account(i64),
    /// A stored mailbox in an account's folder tree.
    Folder(i64),
    /// A folder-tree parent that holds no mail itself (an IMAP
    /// `\Noselect` parent such as Gmail's `[Gmail]`).
    Group(FolderRef),
}

impl SidebarItem {
    /// The view the row opens, if it opens one.
    fn highlight(&self) -> Option<SidebarHighlight> {
        match self {
            Self::Virtual(virtual_mailbox) => Some(SidebarHighlight::Virtual(*virtual_mailbox)),
            Self::Inbox(id) | Self::Folder(id) => Some(SidebarHighlight::Mailbox(*id)),
            Self::Drafts => Some(SidebarHighlight::Drafts),
            Self::Outbox => Some(SidebarHighlight::Outbox),
            Self::Account(_) | Self::Group(_) => None,
        }
    }
}

/// One line of the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarRow {
    /// A section heading.
    Section(Section),
    /// A row that takes keyboard focus.
    Item {
        item: SidebarItem,
        /// Indent level.
        depth: usize,
        /// `Some(expanded)` for a row that collapses.
        expanded: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Favourites,
    Accounts,
}

pub(crate) struct SidebarView {
    focus_handle: FocusHandle,
    state: SidebarState,
    /// The item the arrow keys move from: the last one picked in the
    /// sidebar. It falls back to the highlighted row.
    cursor: Option<SidebarItem>,
    scroll: ScrollHandle,
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
            scroll: ScrollHandle::new(),
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
                .as_ref()
                .is_some_and(|cursor| self.row_index(cursor).is_none())
            {
                self.cursor = None;
            }
            cx.notify();
        }
    }

    // -----------------------------------------------------------------------
    // Rows
    // -----------------------------------------------------------------------

    /// Every line of the sidebar, top to bottom.
    fn rows(&self) -> Vec<SidebarRow> {
        let state = &self.state;
        let mut rows = Vec::new();
        if state.accounts.is_empty() {
            rows.push(SidebarRow::Section(Section::Accounts));
            return rows;
        }
        let item = |item, depth| SidebarRow::Item {
            item,
            depth,
            expanded: None,
        };

        // Inboxes.
        let inboxes = state
            .accounts
            .iter()
            .filter_map(|account| self.role_mailbox(account.id, MailboxRole::Inbox))
            .map(|mailbox| mailbox.id)
            .collect::<Vec<_>>();
        if state.accounts.len() > 1 {
            rows.push(item(
                SidebarItem::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox)),
                0,
            ));
            rows.extend(
                inboxes
                    .into_iter()
                    .map(|id| item(SidebarItem::Inbox(id), 1)),
            );
        } else {
            rows.extend(
                inboxes
                    .into_iter()
                    .map(|id| item(SidebarItem::Inbox(id), 0)),
            );
        }

        rows.push(SidebarRow::Section(Section::Favourites));
        rows.extend([
            item(SidebarItem::Virtual(VirtualMailbox::Flagged), 0),
            item(SidebarItem::Drafts, 0),
            item(
                SidebarItem::Virtual(VirtualMailbox::Unified(MailboxRole::Sent)),
                0,
            ),
            item(SidebarItem::Outbox, 0),
        ]);

        rows.push(SidebarRow::Section(Section::Accounts));
        for account in &state.accounts {
            let collapsed = state.tree.collapsed_accounts.contains(&account.id);
            rows.push(SidebarRow::Item {
                item: SidebarItem::Account(account.id),
                depth: 0,
                expanded: Some(!collapsed),
            });
            if collapsed {
                continue;
            }
            // Folders below a collapsed folder are hidden.
            let mut hidden_below = None;
            for node in folder_tree(
                state
                    .mailboxes
                    .iter()
                    .filter(|mailbox| mailbox.account_id == account.id),
            ) {
                if hidden_below.is_some_and(|depth| node.depth > depth) {
                    continue;
                }
                hidden_below = None;
                let folder = FolderRef {
                    account_id: account.id,
                    name: node.name.clone(),
                };
                let expanded = node.has_children.then(|| {
                    let collapsed = state.tree.collapsed_folders.contains(&folder);
                    if collapsed {
                        hidden_below = Some(node.depth);
                    }
                    !collapsed
                });
                rows.push(SidebarRow::Item {
                    item: match node.mailbox_id {
                        Some(id) => SidebarItem::Folder(id),
                        None => SidebarItem::Group(folder),
                    },
                    depth: node.depth + 1,
                    expanded,
                });
            }
        }
        rows
    }

    /// The keyboard stops, in order.
    fn items(&self) -> Vec<SidebarItem> {
        self.rows()
            .into_iter()
            .filter_map(|row| match row {
                SidebarRow::Item { item, .. } => Some(item),
                SidebarRow::Section(_) => None,
            })
            .collect()
    }

    /// The row index of `item`, for scrolling it into view.
    fn row_index(&self, item: &SidebarItem) -> Option<usize> {
        self.rows().iter().position(
            |row| matches!(row, SidebarRow::Item { item: candidate, .. } if candidate == item),
        )
    }

    /// The account's first mailbox with `role`, as the store resolves
    /// roles.
    fn role_mailbox(&self, account_id: i64, role: MailboxRole) -> Option<&MailboxSummary> {
        self.state
            .mailboxes
            .iter()
            .find(|mailbox| mailbox.account_id == account_id && mailbox.role == Some(role))
    }

    /// Unread conversations in every account's mailbox with `role`.
    fn role_unread(&self, role: MailboxRole) -> usize {
        self.state
            .accounts
            .iter()
            .filter_map(|account| self.role_mailbox(account.id, role))
            .map(|mailbox| mailbox.unread_threads)
            .sum()
    }

    /// The row drawn as selected: the cursor when it shows the current
    /// view (the Inbox is both in Inboxes and in its account's folders),
    /// otherwise the first row that does.
    fn selected_item(&self, items: &[SidebarItem]) -> Option<SidebarItem> {
        let highlight = self.state.highlight;
        if highlight == SidebarHighlight::None {
            return None;
        }
        self.cursor
            .clone()
            .filter(|cursor| cursor.highlight() == Some(highlight))
            .or_else(|| {
                items
                    .iter()
                    .find(|item| item.highlight() == Some(highlight))
                    .cloned()
            })
    }

    // -----------------------------------------------------------------------
    // Keyboard selection
    // -----------------------------------------------------------------------

    fn current_index(&self) -> Option<usize> {
        let items = self.items();
        let current = self.cursor.clone().or_else(|| self.selected_item(&items))?;
        items.iter().position(|item| *item == current)
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items().get(index).cloned() else {
            return;
        };
        if let Some(row) = self.row_index(&item) {
            self.scroll.scroll_to_item(row);
        }
        self.cursor = Some(item.clone());
        if let Some(event) = select_event(&item) {
            cx.emit(event);
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

    /// Enter: on an account or folder parent it expands or collapses the
    /// tree; anywhere else it moves on to the list.
    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        let node = self.cursor.as_ref().and_then(|cursor| {
            self.rows().into_iter().find_map(|row| match row {
                SidebarRow::Item {
                    item,
                    expanded: Some(_),
                    ..
                } if item == *cursor => self.tree_node(&item),
                _ => None,
            })
        });
        match node {
            Some(node) => cx.emit(SidebarEvent::ToggleCollapsed(node)),
            None => cx.emit(SidebarEvent::Activate),
        }
    }

    /// The collapsible node a row stands for.
    fn tree_node(&self, item: &SidebarItem) -> Option<TreeNode> {
        match item {
            SidebarItem::Account(id) => Some(TreeNode::Account(*id)),
            SidebarItem::Group(folder) => Some(TreeNode::Folder(folder.clone())),
            SidebarItem::Folder(id) => self
                .state
                .mailboxes
                .iter()
                .find(|mailbox| mailbox.id == *id)
                .map(|mailbox| {
                    TreeNode::Folder(FolderRef {
                        account_id: mailbox.account_id,
                        name: mailbox.name.clone(),
                    })
                }),
            _ => None,
        }
    }
}

/// The event picking `item` sends; account headers and folder parents
/// that hold no mail only move the cursor.
fn select_event(item: &SidebarItem) -> Option<SidebarEvent> {
    match item {
        SidebarItem::Virtual(virtual_mailbox) => {
            Some(SidebarEvent::SelectVirtual(*virtual_mailbox))
        }
        SidebarItem::Inbox(id) | SidebarItem::Folder(id) => Some(SidebarEvent::SelectMailbox(*id)),
        SidebarItem::Drafts => Some(SidebarEvent::ShowDrafts),
        SidebarItem::Outbox => Some(SidebarEvent::ShowOutbox),
        SidebarItem::Account(_) | SidebarItem::Group(_) => None,
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
        let rows = self.rows();
        let items = self.items();
        let selected = self.selected_item(&items);
        let children = rows
            .iter()
            .map(|row| match row {
                SidebarRow::Section(section) => self.render_section(*section, &colors),
                SidebarRow::Item {
                    item,
                    depth,
                    expanded,
                } => {
                    // Account headers and folder parents show the keyboard
                    // cursor; other rows show the current view.
                    let is_selected = match item {
                        SidebarItem::Account(_) | SidebarItem::Group(_) => {
                            focused && self.cursor.as_ref() == Some(item)
                        }
                        _ => selected.as_ref() == Some(item),
                    };
                    self.render_item(item, *depth, *expanded, is_selected, &colors, cx)
                }
            })
            .collect::<Vec<_>>();

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
            // The focus indicator: a top rule in the focus-ring colour.
            .child(pane_focus_indicator(
                div()
                    .flex_none()
                    .w_full()
                    .border_t_1()
                    .border_color(rgb(colors.border)),
                focused,
                &colors,
            ))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("sidebar-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .pb_2()
                            .children(children),
                    )
                    .child(Scrollbar::vertical("sidebar-scrollbar", &self.scroll)),
            )
    }
}

impl SidebarView {
    fn tokens(&self) -> DensityTokens {
        DensityTokens::for_density(self.state.density)
    }

    fn render_section(&self, section: Section, colors: &UiColors) -> AnyElement {
        let label = match section {
            Section::Favourites => "Favourites",
            Section::Accounts => "Accounts",
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(self.tokens().sidebar_row_height))
            .mt_2()
            .px(px(self.tokens().sidebar_row_padding_x))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(colors.muted))
            .child(label)
            .when(section == Section::Accounts, |this| {
                this.child(
                    IconButton::new("sidebar-add-account", "plus", "Add Account")
                        .size(ButtonSize::Small)
                        .shortcut("secondary-shift-a")
                        .on_click(dispatch_from(&self.focus_handle, AddAccount)),
                )
            })
            .into_any_element()
    }

    fn render_item(
        &self,
        item: &SidebarItem,
        depth: usize,
        expanded: Option<bool>,
        is_selected: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match item {
            SidebarItem::Account(account_id) => self
                .state
                .accounts
                .iter()
                .find(|account| account.id == *account_id)
                .map(|account| {
                    self.render_account(account, expanded.unwrap_or(true), is_selected, colors, cx)
                        .into_any_element()
                })
                .unwrap_or_else(|| div().into_any_element()),
            SidebarItem::Folder(_) | SidebarItem::Group(_) => self
                .render_folder_row(item, depth, expanded, is_selected, colors, cx)
                .into_any_element(),
            _ => self
                .render_view_row(item, depth, is_selected, colors, cx)
                .into_any_element(),
        }
    }

    /// The common frame of a row: height, indent, background, hover and
    /// click. Picking a row that opens a view also tells the shell it was
    /// clicked, so the sidebar overlay closes.
    fn row_frame(
        &self,
        id: SharedString,
        item: &SidebarItem,
        depth: usize,
        is_selected: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let tokens = self.tokens();
        let selector = id.to_string();
        let clicked = item.clone();
        div()
            .id(id)
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .min_w_0()
            .h(px(tokens.sidebar_row_height))
            .pl(px(
                tokens.sidebar_row_padding_x + depth as f32 * SIDEBAR_TREE_INDENT
            ))
            .pr(px(tokens.sidebar_row_padding_x))
            .cursor_pointer()
            .bg(rgb(interactive_background(
                is_selected,
                colors.surface,
                colors,
            )))
            .hover_bg(is_selected, colors)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.cursor = Some(clicked.clone());
                match select_event(&clicked) {
                    Some(event) => {
                        cx.emit(event);
                        cx.emit(SidebarEvent::RowClicked);
                    }
                    None => {
                        if let Some(node) = this.tree_node(&clicked) {
                            cx.emit(SidebarEvent::ToggleCollapsed(node));
                        }
                    }
                }
                cx.notify();
            }))
    }

    /// A row label that ends in "…" only when it really does not fit.
    fn label(text: impl Into<SharedString>) -> gpui::Div {
        div().flex_1().min_w_0().truncate().child(text.into())
    }

    /// The expand/collapse chevron, or a spacer of the same width so
    /// labels line up.
    fn chevron(
        &self,
        id: SharedString,
        node: Option<TreeNode>,
        expanded: Option<bool>,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match (node, expanded) {
            (Some(node), Some(expanded)) => div()
                .debug_selector({
                    let selector = id.to_string();
                    move || selector.clone()
                })
                .id(id)
                .flex_none()
                .cursor_pointer()
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(SidebarEvent::ToggleCollapsed(node.clone()));
                }))
                .child(icon(
                    if expanded {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    ICON_SIZE_SM,
                    colors.muted,
                ))
                .into_any_element(),
            _ => div().flex_none().size(px(ICON_SIZE_SM)).into_any_element(),
        }
    }

    /// An inbox, favourite or Drafts/Outbox row.
    fn render_view_row(
        &self,
        item: &SidebarItem,
        depth: usize,
        is_selected: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let state = &self.state;
        let (id, icon_name, label, unread): (SharedString, _, SharedString, usize) = match item {
            SidebarItem::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox)) => (
                "sidebar-unified-inbox".into(),
                "inbox",
                VirtualMailbox::Unified(MailboxRole::Inbox).label().into(),
                self.role_unread(MailboxRole::Inbox),
            ),
            SidebarItem::Virtual(VirtualMailbox::Unified(role)) => (
                format!("sidebar-{}", role.as_str()).into(),
                mailbox_icon_name(Some(*role)),
                role.label().into(),
                self.role_unread(*role),
            ),
            SidebarItem::Virtual(VirtualMailbox::Flagged) => (
                "sidebar-flagged".into(),
                "flag",
                VirtualMailbox::Flagged.label().into(),
                state.flagged_unread,
            ),
            SidebarItem::Inbox(mailbox_id) => {
                let mailbox = state
                    .mailboxes
                    .iter()
                    .find(|mailbox| mailbox.id == *mailbox_id);
                let label = match mailbox {
                    // Under the Unified Inbox each inbox is named by its
                    // account.
                    Some(mailbox) if state.accounts.len() > 1 => state
                        .accounts
                        .iter()
                        .find(|account| account.id == mailbox.account_id)
                        .map(|account| account.display_name.clone())
                        .unwrap_or_default(),
                    _ => MailboxRole::Inbox.label().to_owned(),
                };
                (
                    format!("inbox-row-{mailbox_id}").into(),
                    "inbox",
                    label.into(),
                    mailbox.map(|mailbox| mailbox.unread_threads).unwrap_or(0),
                )
            }
            SidebarItem::Drafts => ("sidebar-drafts".into(), "file-text", "Drafts".into(), 0),
            _ => ("sidebar-outbox".into(), "clock", "Outbox".into(), 0),
        };
        let account_id = match item {
            SidebarItem::Inbox(mailbox_id) if state.accounts.len() > 1 => state
                .mailboxes
                .iter()
                .find(|mailbox| mailbox.id == *mailbox_id)
                .map(|mailbox| mailbox.account_id),
            _ => None,
        };
        let outbox = state.outbox;
        self.row_frame(id, item, depth, is_selected, colors, cx)
            .child(icon(icon_name, ICON_SIZE_SM, colors.muted))
            .when_some(account_id, |this, account_id| {
                this.child(account_dot(account_id))
            })
            .child(Self::label(label))
            .when(unread > 0, |this| this.child(Badge::count(unread)))
            .when(
                *item == SidebarItem::Drafts && state.draft_count > 0,
                |this| this.child(Badge::count(state.draft_count).tone(BadgeTone::Neutral)),
            )
            .when(*item == SidebarItem::Outbox, |this| {
                this.when(outbox.waiting > 0, |this| {
                    this.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgb(if outbox.failed > 0 {
                                colors.danger
                            } else {
                                colors.muted
                            }))
                            .child(format!("{} waiting", outbox.waiting)),
                    )
                })
                .when(outbox.failed > 0, |this| {
                    this.child(
                        div()
                            .flex_none()
                            .debug_selector(|| "sidebar-outbox-retry".to_owned())
                            .child(
                                IconButton::new(
                                    "sidebar-outbox-retry",
                                    "refresh-cw",
                                    "Retry sending",
                                )
                                .size(ButtonSize::Small)
                                .on_click(cx.listener(
                                    |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(SidebarEvent::RetryOutbox);
                                    },
                                )),
                            ),
                    )
                })
            })
    }

    /// An account header: chevron, colour dot, name and sync status, with
    /// "Fix…" instead of the error text when syncing fails.
    fn render_account(
        &self,
        account: &AccountSummary,
        expanded: bool,
        is_selected: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let account_id = account.id;
        let item = SidebarItem::Account(account_id);
        let status = &account.sync_status;
        let failing = status.phase == SyncPhase::Failed || status.failure_count > 0;
        let (status_icon, status_color, status_label) = sync_badge(status.phase, colors);
        let status_tooltip = match status.last_error_message.as_deref() {
            Some(error) if failing => format!("{status_label}: {error}"),
            _ => status_label.to_owned(),
        };
        self.row_frame(
            account_row_id(account_id),
            &item,
            0,
            is_selected,
            colors,
            cx,
        )
        .child(self.chevron(
            format!("account-chevron-{account_id}").into(),
            Some(TreeNode::Account(account_id)),
            Some(expanded),
            colors,
            cx,
        ))
        .child(account_dot(account_id))
        .child(Self::label(account.display_name.clone()).font_weight(FontWeight::SEMIBOLD))
        .child(
            div()
                .id(SharedString::from(format!("account-status-{account_id}")))
                .flex_none()
                .tooltip(text_tooltip(status_tooltip, *colors))
                .child(icon(
                    status_icon,
                    ICON_SIZE_SM,
                    if status.phase == SyncPhase::Idle && !failing {
                        colors.muted
                    } else {
                        status_color
                    },
                )),
        )
        .when(failing, |this| {
            this.child(
                div()
                    .flex_none()
                    .debug_selector(move || format!("account-fix-{account_id}"))
                    .child(
                        Button::new(format!("account-fix-{account_id}"), "Fix…")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Small)
                            .tooltip("Open the account settings")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::FixAccount(account_id));
                            })),
                    ),
            )
        })
    }

    /// A row of an account's folder tree. Folders have no unread badges
    /// (those are for inboxes and favourites).
    ///
    /// Each folder row is built here, so drop targets for dragged threads
    /// hang off this one place.
    fn render_folder_row(
        &self,
        item: &SidebarItem,
        depth: usize,
        expanded: Option<bool>,
        is_selected: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        // Only stored mailboxes take dropped conversations; a parent that
        // holds no mail does not.
        let mut drop_target = None;
        let (id, icon_name, label) = match item {
            SidebarItem::Folder(mailbox_id) => {
                let mailbox = self
                    .state
                    .mailboxes
                    .iter()
                    .find(|mailbox| mailbox.id == *mailbox_id);
                drop_target = mailbox.map(|mailbox| (mailbox.id, mailbox.account_id));
                (
                    mailbox_row_id(*mailbox_id),
                    mailbox_icon_name(mailbox.and_then(|mailbox| mailbox.role)),
                    mailbox.map(folder_label).unwrap_or_default(),
                )
            }
            SidebarItem::Group(folder) => {
                let delimiter = self
                    .state
                    .mailboxes
                    .iter()
                    .find(|mailbox| {
                        mailbox.account_id == folder.account_id
                            && mailbox.parent.as_deref() == Some(folder.name.as_str())
                    })
                    .and_then(|mailbox| mailbox.delimiter.clone());
                (
                    SharedString::from(format!(
                        "folder-group-{}-{}",
                        folder.account_id, folder.name
                    )),
                    "folder",
                    mailbox_leaf_name(&folder.name, None, delimiter.as_deref()).to_owned(),
                )
            }
            _ => return div().id("sidebar-unknown-folder"),
        };
        let chevron_id = SharedString::from(format!("{id}-chevron"));
        self.row_frame(id, item, depth, is_selected, colors, cx)
            .when_some(drop_target, |row, (mailbox_id, account_id)| {
                Self::drop_target(row, mailbox_id, account_id, colors, cx)
            })
            .child(self.chevron(chevron_id, self.tree_node(item), expanded, colors, cx))
            .child(icon(icon_name, ICON_SIZE_SM, colors.muted))
            .child(Self::label(label))
    }

    /// Makes `row`, the folder `mailbox_id` of account `account_id`, take
    /// conversations dragged from the list. It highlights only while
    /// conversations it accepts hover over it.
    fn drop_target(
        row: Stateful<gpui::Div>,
        mailbox_id: i64,
        account_id: i64,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let (fill, text) = (colors.selection_background, colors.selection_foreground);
        row.drag_over::<DraggedThreads>(move |style, dragged, _, _| {
            if dragged.accepts(mailbox_id, account_id) {
                style.bg(rgb(fill)).text_color(rgb(text))
            } else {
                style
            }
        })
        .can_drop(move |dragged, _, _| {
            dragged
                .downcast_ref::<DraggedThreads>()
                .is_some_and(|dragged| dragged.accepts(mailbox_id, account_id))
        })
        .on_drop(cx.listener(move |_, dragged: &DraggedThreads, _, cx| {
            cx.emit(SidebarEvent::DropThreads {
                thread_ids: dragged.thread_ids.clone(),
                mailbox_id,
            });
        }))
    }
}

/// A folder's name in the tree: its last path segment, except that the
/// Inbox is always "Inbox" (IMAP servers call it "INBOX").
fn folder_label(mailbox: &MailboxSummary) -> String {
    match mailbox.role {
        Some(MailboxRole::Inbox) if mailbox.parent.is_none() => {
            MailboxRole::Inbox.label().to_owned()
        }
        _ => mailbox.leaf_name().to_owned(),
    }
}

fn account_row_id(account_id: i64) -> SharedString {
    SharedString::from(format!("account-row-{account_id}"))
}

fn mailbox_row_id(mailbox_id: i64) -> SharedString {
    SharedString::from(format!("mailbox-row-{mailbox_id}"))
}

/// The icon for a mailbox: from its role, never its name, so "Sent Mail",
/// "Gesendet" and Gmail's "[Gmail]/Sent Mail" all get the Sent icon.
pub(crate) fn mailbox_icon_name(role: Option<MailboxRole>) -> &'static str {
    match role {
        Some(MailboxRole::Inbox) => "inbox",
        Some(MailboxRole::Drafts) => "file-text",
        Some(MailboxRole::Sent) => "send",
        Some(MailboxRole::Archive) => "archive",
        Some(MailboxRole::Junk) => "shield-alert",
        Some(MailboxRole::Trash) => "trash-2",
        None => "folder",
    }
}

// ---------------------------------------------------------------------------
// Folder tree
// ---------------------------------------------------------------------------

/// One node of an account's folder tree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderNode {
    /// Server name.
    name: String,
    /// The stored mailbox, or `None` for a parent that holds no mail.
    mailbox_id: Option<i64>,
    /// Nesting level, 0 at the top.
    depth: usize,
    has_children: bool,
}

/// An account's mailboxes as a tree, flattened top to bottom.
///
/// Mailboxes nest under their `parent`. A parent that is not a stored
/// mailbox (an IMAP `\Noselect` parent) becomes a node of its own, nested
/// in turn by its name and the delimiter. Siblings keep the order of
/// `mailboxes`, so role mailboxes stay first.
fn folder_tree<'a>(mailboxes: impl Iterator<Item = &'a MailboxSummary>) -> Vec<FolderNode> {
    struct Tree<'a> {
        stored: HashMap<&'a str, &'a MailboxSummary>,
        /// Delimiters of the stored mailboxes under each missing parent,
        /// to find that parent's own parent.
        group_delimiters: HashMap<String, String>,
        children: HashMap<String, Vec<String>>,
        roots: Vec<String>,
        placed: HashSet<String>,
    }

    impl Tree<'_> {
        fn place(&mut self, name: &str) {
            if !self.placed.insert(name.to_owned()) {
                return;
            }
            let parent = match self.stored.get(name) {
                Some(mailbox) => mailbox.parent.clone(),
                None => self.group_delimiters.get(name).and_then(|delimiter| {
                    name.rsplit_once(delimiter.as_str())
                        .map(|(parent, _)| parent.to_owned())
                        .filter(|parent| !parent.is_empty())
                }),
            };
            match parent.filter(|parent| parent != name) {
                Some(parent) => {
                    if !self.stored.contains_key(parent.as_str())
                        && let Some(delimiter) = self
                            .stored
                            .get(name)
                            .and_then(|mailbox| mailbox.delimiter.clone())
                            .or_else(|| self.group_delimiters.get(name).cloned())
                    {
                        self.group_delimiters
                            .entry(parent.clone())
                            .or_insert(delimiter);
                    }
                    // A cycle (never sent by a sane server) would place
                    // the node under itself; it goes to the top instead.
                    if self.placed.contains(&parent) && !self.reachable(&parent) {
                        self.roots.push(name.to_owned());
                        return;
                    }
                    self.place(&parent);
                    self.children
                        .entry(parent)
                        .or_default()
                        .push(name.to_owned());
                }
                None => self.roots.push(name.to_owned()),
            }
        }

        /// Whether `name` is already in the tree (under a root).
        fn reachable(&self, name: &str) -> bool {
            self.roots.iter().any(|root| root == name)
                || self
                    .children
                    .values()
                    .any(|children| children.iter().any(|child| child == name))
        }

        fn flatten(&self, name: &str, depth: usize, out: &mut Vec<FolderNode>) {
            let children = self.children.get(name);
            out.push(FolderNode {
                name: name.to_owned(),
                mailbox_id: self.stored.get(name).map(|mailbox| mailbox.id),
                depth,
                has_children: children.is_some_and(|children| !children.is_empty()),
            });
            for child in children.into_iter().flatten() {
                self.flatten(child, depth + 1, out);
            }
        }
    }

    let mailboxes = mailboxes.collect::<Vec<_>>();
    let mut tree = Tree {
        stored: mailboxes
            .iter()
            .map(|mailbox| (mailbox.name.as_str(), *mailbox))
            .collect(),
        group_delimiters: HashMap::new(),
        children: HashMap::new(),
        roots: Vec::new(),
        placed: HashSet::new(),
    };
    for mailbox in &mailboxes {
        tree.place(&mailbox.name);
    }
    let mut out = Vec::new();
    for root in &tree.roots {
        tree.flatten(root, 0, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_icons_follow_the_role_and_exist() {
        let icons = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");
        for role in MailboxRole::ALL.map(Some).into_iter().chain([None]) {
            let name = mailbox_icon_name(role);
            assert!(
                icons.join(format!("{name}.svg")).is_file(),
                "the {name} icon is bundled"
            );
        }
        assert_eq!(mailbox_icon_name(Some(MailboxRole::Sent)), "send");
        assert_eq!(mailbox_icon_name(None), "folder");
    }

    fn mailbox(
        id: i64,
        name: &str,
        role: Option<MailboxRole>,
        parent: Option<&str>,
        delimiter: Option<&str>,
    ) -> MailboxSummary {
        MailboxSummary {
            id,
            account_id: 1,
            name: name.to_owned(),
            role,
            delimiter: delimiter.map(str::to_owned),
            parent: parent.map(str::to_owned),
            total_threads: 0,
            unread_threads: 0,
        }
    }

    fn outline(nodes: &[FolderNode]) -> Vec<String> {
        nodes
            .iter()
            .map(|node| {
                format!(
                    "{}{}{}",
                    "  ".repeat(node.depth),
                    node.name,
                    if node.mailbox_id.is_none() {
                        " (group)"
                    } else {
                        ""
                    }
                )
            })
            .collect()
    }

    #[test]
    fn the_inbox_is_called_inbox() {
        let inbox = mailbox(1, "INBOX", Some(MailboxRole::Inbox), None, Some("/"));
        assert_eq!(folder_label(&inbox), "Inbox");
        let nested = mailbox(2, "Work/Website", None, Some("Work"), Some("/"));
        assert_eq!(folder_label(&nested), "Website");
    }

    #[test]
    fn folders_nest_under_their_parents() {
        let slash = Some("/");
        let mailboxes = [
            mailbox(1, "INBOX", Some(MailboxRole::Inbox), None, slash),
            mailbox(
                2,
                "[Gmail]/Sent Mail",
                Some(MailboxRole::Sent),
                Some("[Gmail]"),
                slash,
            ),
            mailbox(
                3,
                "[Gmail]/Bin",
                Some(MailboxRole::Trash),
                Some("[Gmail]"),
                slash,
            ),
            // The child is listed before its stored parent.
            mailbox(4, "Projects/Website", None, Some("Projects"), slash),
            mailbox(5, "Projects", None, None, slash),
            mailbox(6, "Lists/rust/announce", None, Some("Lists/rust"), slash),
        ];
        assert_eq!(
            outline(&folder_tree(mailboxes.iter())),
            [
                "INBOX",
                "[Gmail] (group)",
                "  [Gmail]/Sent Mail",
                "  [Gmail]/Bin",
                "Projects",
                "  Projects/Website",
                "Lists (group)",
                "  Lists/rust (group)",
                "    Lists/rust/announce",
            ]
        );
        let tree = folder_tree(mailboxes.iter());
        assert!(tree[1].has_children);
        assert!(!tree[0].has_children);
    }

    #[test]
    fn jmap_parents_need_no_delimiter() {
        let mailboxes = [
            mailbox(1, "Inbox", Some(MailboxRole::Inbox), None, None),
            mailbox(2, "Website", None, Some("Projects"), None),
            mailbox(3, "Projects", None, Some("Work"), None),
            mailbox(4, "Work", None, None, None),
            // A parent that never arrived goes to the top as a group.
            mailbox(5, "Orphan", None, Some("Gone"), None),
        ];
        assert_eq!(
            outline(&folder_tree(mailboxes.iter())),
            [
                "Inbox",
                "Work",
                "  Projects",
                "    Website",
                "Gone (group)",
                "  Orphan",
            ]
        );
    }

    #[test]
    fn a_parent_cycle_does_not_hang() {
        let mailboxes = [
            mailbox(1, "A", None, Some("B"), None),
            mailbox(2, "B", None, Some("A"), None),
        ];
        let tree = folder_tree(mailboxes.iter());
        assert_eq!(tree.len(), 2, "both folders are listed once: {tree:?}");
    }
}
