//! The right-click menu on thread and search rows.
//!
//! It acts on the clicked row, or on the whole selection when the clicked
//! row is part of it: a right-click on a row outside the selection selects
//! that row first, as a plain click would. Each entry dispatches the same
//! action as the keyboard shortcut and the bulk bar, so the change goes
//! through `MailActions` and can be undone. Move to… opens the folder
//! picker where the menu was.

use gpui::{
    AnyElement, Context, DismissEvent, Focusable, IntoElement, Modifiers, Pixels, Point, Window,
};

use super::MessageListView;
use crate::ListMode;
use crate::actions::{Archive, Delete, ToggleFlag, ToggleRead};
use crate::components::{Menu, MenuEntry, overlay_at};

impl MessageListView {
    /// A right-click on the thread or search row at `index`, at
    /// `position` in the window.
    pub(super) fn open_context_menu(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.state.mode, ListMode::Mail | ListMode::Search) {
            return;
        }
        let Some((thread_id, flagged, unread)) = self.row_marks(index, cx) else {
            return;
        };
        let in_selection = self.state.selection.is_multiple()
            && self.state.selection.thread_ids.contains(&thread_id);
        if !in_selection && self.state.selected_thread_id != Some(thread_id) {
            self.click_thread_row(index, Modifiers::none(), cx);
        }
        window.focus(&self.focus_handle, cx);

        let selection = &self.state.selection;
        let (count, flagged, unread) = if in_selection {
            (
                selection.thread_ids.len(),
                selection.all_flagged,
                selection.any_unread,
            )
        } else {
            (1, flagged, unread)
        };
        let can_move = !self.move_destinations().is_empty();
        let focus_handle = self.focus_handle.clone();
        let list = cx.entity().downgrade();
        let dispatch = |action: Box<dyn gpui::Action>| {
            let focus_handle = focus_handle.clone();
            move |window: &mut Window, cx: &mut gpui::App| {
                focus_handle.dispatch_action(action.as_ref(), window, cx);
            }
        };
        let menu = Menu::build(window, cx, |menu| {
            let menu = menu
                .entry(
                    MenuEntry::new(if count > 1 {
                        format!("Archive {count} Conversations")
                    } else {
                        "Archive".to_owned()
                    })
                    .icon("archive")
                    .shortcut("secondary-e")
                    .on_select(dispatch(Box::new(Archive))),
                )
                .entry(
                    MenuEntry::new(if count > 1 {
                        format!("Delete {count} Conversations")
                    } else {
                        "Delete".to_owned()
                    })
                    .icon("trash-2")
                    .shortcut("secondary-backspace")
                    .on_select(dispatch(Box::new(Delete))),
                )
                .entry(
                    MenuEntry::new("Move to…")
                        .icon("folder")
                        .disabled(!can_move)
                        .on_select(move |window, cx| {
                            let _ = list.update(cx, |list, cx| {
                                list.open_move_picker(Some(position), window, cx);
                            });
                        }),
                )
                .separator();
            menu.entry(
                MenuEntry::new(if flagged { "Unflag" } else { "Flag" })
                    .icon("flag")
                    .shortcut("secondary-shift-l")
                    .on_select(dispatch(Box::new(ToggleFlag))),
            )
            .entry(
                MenuEntry::new(if unread {
                    "Mark as Read"
                } else {
                    "Mark as Unread"
                })
                .icon(if unread { "eye" } else { "eye-off" })
                .shortcut("secondary-shift-u")
                .on_select(dispatch(Box::new(ToggleRead))),
            )
        });
        let subscription =
            cx.subscribe_in(&menu, window, |list, menu, _: &DismissEvent, window, cx| {
                // Move to… hands focus to the picker; otherwise it comes
                // back to the list.
                let refocus = menu.focus_handle(cx).contains_focused(window, cx)
                    || window.focused(cx).is_none();
                list.context_menu = None;
                if refocus {
                    window.focus(&list.focus_handle, cx);
                }
                cx.notify();
            });
        window.focus(&menu.focus_handle(cx), cx);
        self.move_picker = None;
        self.context_menu = Some((menu, position, subscription));
        cx.notify();
    }

    /// The thread at row `index` and whether it is flagged and unread.
    fn row_marks(&mut self, index: usize, cx: &mut Context<Self>) -> Option<(i64, bool, bool)> {
        match self.state.mode {
            ListMode::Mail => self
                .thread_rows(index..index + 1, cx)
                .into_iter()
                .next()
                .map(|row| (row.id, row.is_flagged, row.unread_count > 0)),
            ListMode::Search => self
                .state
                .search_results
                .get(index)
                .map(|hit| (hit.thread_id, hit.is_flagged, hit.unread_count > 0)),
            ListMode::Drafts | ListMode::Outbox => None,
        }
    }

    /// The context menu, and the folder picker it opened, floating at the
    /// pointer.
    pub(super) fn render_row_overlays(&self) -> Vec<AnyElement> {
        let menu = self
            .context_menu
            .as_ref()
            .map(|(menu, position, _)| overlay_at(*position, menu.clone()).into_any_element());
        let picker = self.move_picker.as_ref().and_then(|open| {
            open.at
                .map(|position| overlay_at(position, open.picker.clone()).into_any_element())
        });
        menu.into_iter().chain(picker).collect()
    }

    /// The open context menu.
    #[cfg(test)]
    pub(crate) fn context_menu(&self) -> Option<gpui::Entity<Menu>> {
        self.context_menu.as_ref().map(|(menu, _, _)| menu.clone())
    }
}
