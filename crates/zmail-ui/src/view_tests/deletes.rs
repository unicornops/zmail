//! Delete: one keypress (with Undo) to move conversations to Trash, and a
//! question, with Cancel focused, before deleting any for good (from the
//! Trash, or on an account with no Trash).

use gpui::{Entity, TestAppContext, VisualTestContext};

use super::{Setup, click, keys, mailbox_of, open_workspace, selected_thread, toasts};
use crate::WorkspaceView;
use crate::pane::Pane;

/// The delete dialog's title while it shows, and whether Cancel has focus.
fn dialog(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<(String, bool)> {
    cx.update(|window, cx| view.read(cx).delete_confirm.showing(window))
}

fn show_mailbox(view: &Entity<WorkspaceView>, mailbox_id: i64, cx: &mut VisualTestContext) {
    view.update_in(cx, |view, window, cx| {
        view.select_mailbox(mailbox_id, cx);
        view.focus_pane(Pane::MessageList, window, cx);
    });
    cx.run_until_parked();
}

#[gpui::test]
fn deleting_from_the_trash_asks_first(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let first = harness.threads[0];
    let trash = harness.trash_id.expect("a trash");

    // To Trash: at once, with Undo.
    keys("delete", cx);
    assert_eq!(dialog(&view, cx), None);
    assert_eq!(mailbox_of(&harness, first), Some(trash));
    assert_eq!(toasts(&view, cx), ["Conversation moved to Trash"]);

    // From Trash: asked first, Cancel focused; Escape keeps it.
    show_mailbox(&view, trash, cx);
    assert_eq!(selected_thread(&view, cx), Some(first));
    keys("delete", cx);
    assert_eq!(
        dialog(&view, cx),
        Some(("Delete this conversation for good?".to_owned(), true))
    );
    assert_eq!(mailbox_of(&harness, first), Some(trash));
    keys("escape", cx);
    assert_eq!(dialog(&view, cx), None);
    assert_eq!(mailbox_of(&harness, first), Some(trash));
    // So does Cancel (Backspace asks too).
    keys("backspace", cx);
    assert!(dialog(&view, cx).is_some());
    click("delete-confirm-cancel", cx);
    assert_eq!(dialog(&view, cx), None);
    assert_eq!(mailbox_of(&harness, first), Some(trash));

    keys("delete", cx);
    click("delete-confirm-delete", cx);
    assert_eq!(dialog(&view, cx), None);
    assert_eq!(mailbox_of(&harness, first), None, "deleted for good");
    assert!(toasts(&view, cx).contains(&"Conversation deleted".to_owned()));
}

#[gpui::test]
fn select_all_without_a_trash_asks_once_and_names_the_count(cx: &mut TestAppContext) {
    // Home has no Trash.
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let home_inbox = harness.home_inbox_id.expect("Home's inbox");
    show_mailbox(&view, home_inbox, cx);
    keys("secondary-a", cx);
    keys("delete", cx);
    assert_eq!(
        dialog(&view, cx),
        Some(("Delete 2 conversations for good?".to_owned(), true))
    );
    for thread_id in &harness.home_threads {
        assert_eq!(mailbox_of(&harness, *thread_id), Some(home_inbox));
    }
    click("delete-confirm-delete", cx);
    for thread_id in &harness.home_threads {
        assert_eq!(mailbox_of(&harness, *thread_id), None);
    }
}

#[gpui::test]
fn the_window_can_open_on_the_question(cx: &mut TestAppContext) {
    let (view, cx, harness) = super::open_workspace_on(cx, Setup::Mail, |harness| {
        let first = harness.threads[0];
        harness.store.delete_thread(first).expect("to the Trash");
        crate::InitialView::DeleteForGood(first)
    });
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[0]));
    assert_eq!(
        dialog(&view, cx),
        Some(("Delete this conversation for good?".to_owned(), true))
    );
}
