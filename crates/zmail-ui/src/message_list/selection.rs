//! The message list's multi-row selection and its bulk action bar.
//!
//! The shell keeps the selection (it decides what the conversation
//! commands act on) and pushes it back in [`MessageListState::selection`];
//! the list changes it from clicks and keys and reports the result with
//! [`MessageListEvent::SelectThreads`], or [`MessageListEvent::OpenThread`]
//! once a single row is left.
//!
//! - Cmd/Ctrl-click adds or removes a row.
//! - Shift-click and Shift+Up/Down select the rows from the anchor (the
//!   row last clicked or reached without Shift) to the row clicked or
//!   reached.
//! - Secondary+A selects every conversation in the list.
//!
//! With more than one row selected, the bulk action bar above the rows
//! dispatches the same actions as the reader's buttons, so every change
//! goes through `MailActions` and can be undone.

use std::ops::RangeInclusive;

use gpui::{
    AnyElement, Context, DismissEvent, FontWeight, IntoElement, Modifiers, ScrollStrategy, Window,
    div, prelude::*, rgb,
};
use zmail_core::{MailboxSelection, MailboxSummary};

use super::{MessageListEvent, MessageListView};
use crate::ListMode;
use crate::actions::{
    Archive, Cancel, Delete, ExtendSelectionDown, ExtendSelectionUp, MoveToMailbox, SelectAll,
    ToggleFlag, ToggleRead,
};
use crate::components::{IconButton, popover_below_end};
use crate::move_picker::{MovePicker, MovePickerEvent};
use crate::pane::dispatch_from;
use crate::styles::UiColors;
use crate::widgets::EllipsisExt;

/// The selection the shell keeps, as the list needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ListSelection {
    /// The selected conversations when more than one is selected;
    /// otherwise empty, and the selected row is `selected_thread_id`.
    pub(crate) thread_ids: Vec<i64>,
    /// Whether every selected conversation is flagged (the bar offers
    /// Unflag).
    pub(crate) all_flagged: bool,
    /// Whether any selected conversation has unread mail (the bar offers
    /// Mark Read).
    pub(crate) any_unread: bool,
    /// The account the selected conversations belong to (the open one's
    /// with a single row selected), whose folders Move to… offers; `None`
    /// when a cross-account list's selection spans accounts.
    pub(crate) account_id: Option<i64>,
}

impl ListSelection {
    /// Whether more than one row is selected.
    pub(crate) fn is_multiple(&self) -> bool {
        self.thread_ids.len() > 1
    }
}

/// `ids` with `id` added, or removed if it was there.
pub(crate) fn toggled(mut ids: Vec<i64>, id: i64) -> Vec<i64> {
    match ids.iter().position(|selected| *selected == id) {
        Some(position) => {
            ids.remove(position);
        }
        None => ids.push(id),
    }
    ids
}

/// The rows from `anchor` to `to`, in list order whichever way they run.
pub(crate) fn span(anchor: usize, to: usize) -> RangeInclusive<usize> {
    anchor.min(to)..=anchor.max(to)
}

/// The palette for a row on the focused selection's accent fill: text,
/// icons, the unread dot and the badge all take the selection foreground.
pub(crate) fn selected_row_colors(colors: &UiColors) -> UiColors {
    UiColors {
        foreground: colors.selection_foreground,
        muted: colors.selection_foreground,
        accent: colors.selection_foreground,
        warning: colors.selection_foreground,
        background: colors.selection_background,
        ..*colors
    }
}

/// The palette a row's content is drawn with: [`selected_row_colors`] on
/// the focused selection's fill, the list's own otherwise.
pub(crate) fn row_colors(selected: bool, list_focused: bool, colors: &UiColors) -> UiColors {
    if selected && list_focused {
        selected_row_colors(colors)
    } else {
        *colors
    }
}

/// The fill of a row: the accent while the list has focus, grey when it
/// doesn't (plan §2).
pub(crate) fn row_background(selected: bool, list_focused: bool, colors: &UiColors) -> u32 {
    match (selected, list_focused) {
        (false, _) => colors.surface,
        (true, true) => colors.selection_background,
        (true, false) => colors.selected_background,
    }
}

impl MessageListView {
    /// Whether the row for `thread_id` is drawn selected.
    pub(super) fn is_thread_selected(&self, thread_id: i64) -> bool {
        if self.state.selection.is_multiple() {
            self.state.selection.thread_ids.contains(&thread_id)
        } else {
            self.state.selected_thread_id == Some(thread_id)
        }
    }

    /// A click on the thread or search row at `index`.
    pub(super) fn click_thread_row(
        &mut self,
        index: usize,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some((thread_id, mailbox_id)) = self.row_thread(index, cx) else {
            return;
        };
        if modifiers.shift {
            let anchor = self.selection_anchor.unwrap_or(index);
            self.select_span(anchor, index, cx);
        } else if modifiers.secondary() {
            let mut ids = self.state.selection.thread_ids.clone();
            if ids.is_empty() {
                ids.extend(self.state.selected_thread_id);
            }
            let ids = toggled(ids, thread_id);
            self.selection_anchor = Some(index);
            let cursor = if ids.contains(&thread_id) {
                Some((thread_id, mailbox_id))
            } else {
                ids.last().map(|&id| (id, mailbox_id))
            };
            if let Some(cursor) = cursor {
                self.set_selection(ids, cursor, cx);
            }
        } else {
            self.selection_anchor = Some(index);
            self.selected_index_hint = Some((thread_id, index));
            self.set_selection(Vec::new(), (thread_id, mailbox_id), cx);
            cx.emit(MessageListEvent::RowClicked);
        }
    }

    pub(super) fn extend_selection_down(
        &mut self,
        _: &ExtendSelectionDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.extend_selection(true, cx);
    }

    pub(super) fn extend_selection_up(
        &mut self,
        _: &ExtendSelectionUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.extend_selection(false, cx);
    }

    /// Shift+Down or Shift+Up: moves the end of the selection one row.
    fn extend_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        if !matches!(self.state.mode, ListMode::Mail | ListMode::Search) {
            return;
        }
        let Some(current) = self.current_index(cx) else {
            return;
        };
        let next = if down {
            (current + 1).min(self.row_count().saturating_sub(1))
        } else {
            current.saturating_sub(1)
        };
        let anchor = *self.selection_anchor.get_or_insert(current);
        self.select_span(anchor, next, cx);
        self.thread_scroll
            .scroll_to_item(next, ScrollStrategy::Nearest);
    }

    pub(super) fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<i64> = match self.state.mode {
            ListMode::Mail => {
                let Some(selection) = self.state.mailbox.as_ref().map(|mailbox| mailbox.selection)
                else {
                    return;
                };
                // Straight from the store: the cached page stays the
                // visible rows.
                match self
                    .store
                    .list_selection_threads(&selection, 0..self.row_count())
                {
                    Ok(rows) => rows.into_iter().map(|row| row.id).collect(),
                    Err(error) => {
                        cx.emit(MessageListEvent::Error(error.to_string()));
                        return;
                    }
                }
            }
            ListMode::Search => self
                .state
                .search_results
                .iter()
                .map(|hit| hit.thread_id)
                .collect(),
            ListMode::Drafts | ListMode::Outbox => return,
        };
        let index = self.current_index(cx).unwrap_or(0);
        let Some(cursor) = self.row_thread(index, cx) else {
            return;
        };
        self.set_selection(ids, cursor, cx);
    }

    /// Selects the rows from `anchor` to `to`, with the cursor on `to`.
    fn select_span(&mut self, anchor: usize, to: usize, cx: &mut Context<Self>) {
        let range = span(anchor, to);
        let ids: Vec<i64> = match self.state.mode {
            ListMode::Mail => self
                .thread_rows(*range.start()..*range.end() + 1, cx)
                .into_iter()
                .map(|row| row.id)
                .collect(),
            ListMode::Search => self
                .state
                .search_results
                .get(range)
                .unwrap_or_default()
                .iter()
                .map(|hit| hit.thread_id)
                .collect(),
            ListMode::Drafts | ListMode::Outbox => return,
        };
        let Some(cursor) = self.row_thread(to, cx) else {
            return;
        };
        self.selected_index_hint = Some((cursor.0, to));
        self.set_selection(ids, cursor, cx);
    }

    /// Makes `ids` the selection with the cursor on `cursor` (thread and
    /// mailbox), and tells the shell. One row or none is a plain
    /// selection of the cursor row.
    fn set_selection(&mut self, ids: Vec<i64>, cursor: (i64, i64), cx: &mut Context<Self>) {
        let (thread_id, mailbox_id) = cursor;
        self.state.selected_thread_id = Some(thread_id);
        if ids.len() > 1 {
            self.state.selection.thread_ids = ids.clone();
            cx.emit(MessageListEvent::SelectThreads {
                thread_ids: ids,
                thread_id,
                mailbox_id,
            });
        } else {
            self.state.selection.thread_ids.clear();
            cx.emit(MessageListEvent::OpenThread {
                thread_id,
                mailbox_id,
            });
        }
        cx.notify();
    }

    /// The thread and mailbox of row `index` of a thread or search list.
    fn row_thread(&mut self, index: usize, cx: &mut Context<Self>) -> Option<(i64, i64)> {
        match self.state.mode {
            ListMode::Mail => {
                let thread = self.thread_rows(index..index + 1, cx).into_iter().next()?;
                Some((thread.id, thread.mailbox_id))
            }
            ListMode::Search => self
                .state
                .search_results
                .get(index)
                .map(|hit| (hit.thread_id, hit.mailbox_id)),
            ListMode::Drafts | ListMode::Outbox => None,
        }
    }

    /// The first selected row's index in the mailbox, if the cached page
    /// holds it.
    pub(super) fn first_selected_index(&self) -> Option<usize> {
        let page = self.page.as_ref()?;
        page.rows
            .iter()
            .position(|row| self.state.selection.thread_ids.contains(&row.id))
            .map(|position| page.start + position)
    }

    // -----------------------------------------------------------------------
    // Move to…
    // -----------------------------------------------------------------------

    /// The folders the selection can move to: its account's folders,
    /// other than the one the list shows. None when the selection spans
    /// accounts.
    fn move_destinations(&self) -> Vec<MailboxSummary> {
        let Some(account_id) = self.state.selection.account_id else {
            return Vec::new();
        };
        let current = match self.state.mailbox.as_ref().map(|mailbox| mailbox.selection) {
            Some(MailboxSelection::Mailbox { mailbox_id, .. })
                if self.state.mode == ListMode::Mail =>
            {
                Some(mailbox_id)
            }
            _ => None,
        };
        self.state
            .mailboxes
            .iter()
            .filter(|mailbox| mailbox.account_id == account_id && Some(mailbox.id) != current)
            .cloned()
            .collect()
    }

    /// Opens the Move to… picker. Picking a folder dispatches
    /// [`MoveToMailbox`] from the list, so the shell moves the selection.
    pub(super) fn open_move_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let destinations = self.move_destinations();
        if destinations.is_empty() {
            return;
        }
        let picker = cx.new(|cx| MovePicker::new(destinations, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &picker,
                window,
                |list, _, event: &MovePickerEvent, window, cx| {
                    let MovePickerEvent::Picked(mailbox_id) = *event;
                    // The shell reads the list while it moves the
                    // selection, so dispatch once this update is over.
                    let focus_handle = list.focus_handle.clone();
                    window.defer(cx, move |window, cx| {
                        focus_handle.dispatch_action(&MoveToMailbox { mailbox_id }, window, cx);
                    });
                },
            ),
            cx.subscribe_in(&picker, window, |list, _, _: &DismissEvent, window, cx| {
                list.move_picker = None;
                window.focus(&list.focus_handle, cx);
                cx.notify();
            }),
        ];
        self.move_picker = Some((picker, subscriptions));
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Bulk action bar
    // -----------------------------------------------------------------------

    /// The bar shown while more than one row is selected.
    pub(super) fn render_bulk_bar(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let selection = &self.state.selection;
        if !selection.is_multiple() || !matches!(self.state.mode, ListMode::Mail | ListMode::Search)
        {
            return None;
        }
        let focus = &self.focus_handle;
        let can_move = !self.move_destinations().is_empty();
        Some(
            div()
                .id("bulk-bar")
                .debug_selector(|| "bulk-bar".into())
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .bg(rgb(colors.surface))
                .border_b_1()
                .border_color(rgb(colors.border))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .ellipsis()
                        .child(format!("{} selected", selection.thread_ids.len())),
                )
                .child(
                    IconButton::new("bulk-archive", "archive", "Archive")
                        .shortcut("secondary-e")
                        .on_click(dispatch_from(focus, Archive)),
                )
                .child(
                    IconButton::new("bulk-delete", "trash-2", "Delete")
                        .shortcut("secondary-backspace")
                        .on_click(dispatch_from(focus, Delete)),
                )
                .when(can_move, |this| {
                    this.child(
                        div()
                            .debug_selector(|| "bulk-move".into())
                            .relative()
                            .child(
                                IconButton::new("bulk-move", "folder", "Move to…")
                                    .selected(self.move_picker.is_some())
                                    .on_click(cx.listener(|list, _, window, cx| {
                                        if list.move_picker.take().is_none() {
                                            list.open_move_picker(window, cx);
                                        }
                                    })),
                            )
                            .when_some(self.move_picker.as_ref(), |this, (picker, _)| {
                                this.child(popover_below_end(picker.clone()))
                            }),
                    )
                })
                .child(
                    IconButton::new(
                        "bulk-flag",
                        "flag",
                        if selection.all_flagged {
                            "Unflag"
                        } else {
                            "Flag"
                        },
                    )
                    .selected(selection.all_flagged)
                    .shortcut("secondary-shift-l")
                    .on_click(dispatch_from(focus, ToggleFlag)),
                )
                .child(
                    if selection.any_unread {
                        IconButton::new("bulk-read", "eye", "Mark Read")
                    } else {
                        IconButton::new("bulk-read", "eye-off", "Mark Unread")
                    }
                    .shortcut("secondary-shift-u")
                    .on_click(dispatch_from(focus, ToggleRead)),
                )
                .child(
                    IconButton::new("bulk-clear", "x", "Clear selection")
                        .shortcut("escape")
                        .on_click(dispatch_from(focus, Cancel)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{row_background, span, toggled};
    use crate::styles::UiColors;

    #[test]
    fn toggling_adds_and_removes_in_click_order() {
        assert_eq!(toggled(vec![1, 2], 3), [1, 2, 3]);
        assert_eq!(toggled(vec![1, 2, 3], 2), [1, 3]);
        assert_eq!(toggled(Vec::new(), 7), [7]);
    }

    #[test]
    fn selected_rows_are_accent_while_the_list_has_focus_and_grey_otherwise() {
        let colors =
            UiColors::from_theme(&crate::view_tests::theme(zmail_core::ThemeAppearance::Dark));
        assert_eq!(
            row_background(true, true, &colors),
            colors.selection_background
        );
        assert_eq!(
            row_background(true, false, &colors),
            colors.selected_background
        );
        assert_eq!(row_background(false, true, &colors), colors.surface);
        assert_ne!(colors.selection_background, colors.selected_background);
    }

    #[test]
    fn a_span_runs_in_list_order_either_way() {
        assert_eq!(span(2, 5), 2..=5);
        assert_eq!(span(5, 2), 2..=5);
        assert_eq!(span(4, 4), 4..=4);
    }
}
