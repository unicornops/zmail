//! Dragging conversations from the message list onto sidebar folders.
//!
//! A thread row starts the drag with a [`DraggedThreads`] payload: the row,
//! or the whole selection when the row is part of it. The payload is also
//! the preview that follows the pointer ("3 conversations"). Sidebar folder
//! rows accept it when [`DraggedThreads::accepts`] says so, highlight while
//! it hovers over them, and report a drop; the shell then moves the
//! conversations through `MailActions`, so the move can be undone.

use gpui::{Context, IntoElement, Render, Window, div, prelude::*, px, rgb};
use zmail_core::text::count_noun;

use crate::components::ComponentTheme;
use crate::styles::{ICON_SIZE_SM, RADIUS_LG, SPACE_2, SPACE_3, TYPE_BODY, TypeStyleExt};
use crate::widgets::icon;

/// Conversations being dragged out of the message list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraggedThreads {
    pub(crate) thread_ids: Vec<i64>,
    /// The account they all belong to; `None` when they span accounts,
    /// and then no folder takes them.
    pub(crate) account_id: Option<i64>,
    /// The mailbox they are all in, if one: dropping them there would
    /// change nothing.
    pub(crate) mailbox_id: Option<i64>,
}

impl DraggedThreads {
    /// Whether the folder `mailbox_id` of account `account_id` takes these
    /// conversations: it is in their account and is not where they are.
    pub(crate) fn accepts(&self, mailbox_id: i64, account_id: i64) -> bool {
        self.account_id == Some(account_id) && self.mailbox_id != Some(mailbox_id)
    }
}

/// The preview under the pointer: a folder icon and the count.
impl Render for DraggedThreads {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_2))
            .px(px(SPACE_3))
            .py(px(SPACE_2))
            .rounded(px(RADIUS_LG))
            .bg(rgb(colors.selection_background))
            .text_color(rgb(colors.selection_foreground))
            .shadow_lg()
            .type_style(TYPE_BODY)
            .child(icon("folder", ICON_SIZE_SM, colors.selection_foreground))
            .child(count_noun(
                self.thread_ids.len(),
                "conversation",
                "conversations",
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::DraggedThreads;

    #[test]
    fn a_folder_takes_threads_of_its_account_that_are_elsewhere() {
        let dragged = DraggedThreads {
            thread_ids: vec![1, 2],
            account_id: Some(7),
            mailbox_id: Some(10),
        };
        assert!(dragged.accepts(11, 7));
        assert!(!dragged.accepts(10, 7), "not the mailbox they are in");
        assert!(!dragged.accepts(11, 8), "not another account's folder");

        let mixed = DraggedThreads {
            account_id: None,
            ..dragged
        };
        assert!(!mixed.accepts(11, 7), "threads of two accounts go nowhere");
    }
}
