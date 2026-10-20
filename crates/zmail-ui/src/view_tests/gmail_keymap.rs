//! The optional Gmail keymap (UI plan Phase 7): single-key shortcuts in
//! the list and reader, a two-key g i, never while typing, switchable at
//! runtime.

use gpui::{AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext};
use zmail_core::{MailboxRole, MailboxSelection, VirtualMailbox, preferences::KeymapName};
use zmail_store::LocalStore;

use super::{
    Harness, Setup, focus, focused_pane, inline_reply, keys, mailbox_of, open_workspace,
    open_workspace_with, search_input, selected_thread, text,
};
use crate::pane::Pane;
use crate::{InitialView, WorkspaceView, keymap};

/// Opens the workspace with the Gmail keymap saved, as a restart after
/// choosing it would.
fn open_with_gmail(
    cx: &mut TestAppContext,
    setup: Setup,
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Harness) {
    open_workspace_with(
        cx,
        setup,
        |_| InitialView::Inbox,
        |store: &LocalStore| {
            store
                .update_ui_preferences(|preferences| preferences.keymap = KeymapName::Gmail)
                .expect("the preference saves");
        },
    )
}

fn flagged(harness: &Harness, thread_id: i64) -> bool {
    harness
        .store
        .thread_view(thread_id)
        .expect("the thread loads")
        .expect("the thread exists")
        .is_flagged
}

fn compose_windows(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> usize {
    cx.read_entity(view, |view, _| view.compose_windows.len())
}

#[gpui::test]
fn single_keys_act_on_the_list(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_with_gmail(cx, Setup::Mail);
    let threads = harness.threads.clone();
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    assert_eq!(selected_thread(&view, cx), Some(threads[0]));

    keys("j", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[1]));
    keys("j k", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[1]));
    keys("k", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[0]));

    // s stars (flags), e archives, z undoes, # deletes.
    keys("s", cx);
    assert!(flagged(&harness, threads[0]));
    keys("s", cx);
    assert!(!flagged(&harness, threads[0]));
    keys("e", cx);
    assert_eq!(mailbox_of(&harness, threads[0]), harness.archive_id);
    keys("z", cx);
    assert_eq!(mailbox_of(&harness, threads[0]), harness.inbox_id);
    keys("#", cx);
    assert_eq!(mailbox_of(&harness, threads[0]), harness.trash_id);
    keys("z", cx);
    assert_eq!(mailbox_of(&harness, threads[0]), harness.inbox_id);

    // r replies below the conversation.
    keys("r", cx);
    assert!(inline_reply(&view, cx).is_some());
}

#[gpui::test]
fn c_composes_and_g_i_goes_to_the_inbox(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_with_gmail(cx, Setup::Mail);
    keys("c", cx);
    assert_eq!(compose_windows(&view, cx), 1, "c opens a compose window");

    // From Archive, g then i goes back to the Inbox, with the list focused.
    view.update(cx, |view, cx| {
        view.select_mailbox(harness.archive_id.expect("an Archive"), cx)
    });
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    keys("g i", cx);
    let selection = cx.read_entity(&view, |view, _| view.selected_mailbox_id);
    assert_eq!(selection, harness.inbox_id);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
}

#[gpui::test]
fn g_i_opens_the_unified_inbox_with_several_accounts(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_with_gmail(cx, Setup::Accounts);
    view.update(cx, |view, cx| {
        view.select_mailbox(harness.home_inbox_id.expect("a Home inbox"), cx)
    });
    keys("g i", cx);
    let selection = cx.read_entity(&view, |view, _| view.mailbox_selection());
    assert_eq!(
        selection,
        Some(MailboxSelection::Virtual(VirtualMailbox::Unified(
            MailboxRole::Inbox
        )))
    );
}

#[gpui::test]
fn j_and_k_in_the_reader_move_through_the_list(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_with_gmail(cx, Setup::Mail);
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    keys("j", cx);
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[1]));
    assert_eq!(
        focused_pane(&view, cx),
        Some(Pane::Detail),
        "reading goes on in the reader"
    );
    keys("k", cx);
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[0]));
}

#[gpui::test]
fn gmail_keys_type_text_in_a_text_field(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_with_gmail(cx, Setup::Mail);
    let first = harness.threads[0];
    let search = search_input(&view, cx);
    focus(&search, cx);

    // Every Gmail key, typed as text: none of them may act.
    for key in ["j", "k", "e", "#", "r", "a", "f", "s", "c", "z", "g", "i"] {
        cx.simulate_input(key);
        cx.run_until_parked();
    }
    assert_eq!(text(&search, cx), "jke#rafsczgi");
    // And as key presses, which is how a keyboard delivers them.
    keys("j k e # r a f s c z g i", cx);
    assert_eq!(selected_thread(&view, cx), Some(first));
    assert_eq!(mailbox_of(&harness, first), harness.inbox_id);
    assert!(!flagged(&harness, first));
    assert!(inline_reply(&view, cx).is_none());
    assert_eq!(compose_windows(&view, cx), 0);
    let search_focused = cx.update(|window, cx| search.focus_handle(cx).is_focused(window));
    assert!(search_focused, "focus stays in the field");
}

#[gpui::test]
fn gmail_keys_type_text_in_the_inline_reply(cx: &mut TestAppContext) {
    // The reply is a text field inside the reader, where the pane keys
    // (e, s, j, #) are bound: they must type rather than act.
    let (view, cx, harness) = open_with_gmail(cx, Setup::Mail);
    let first = harness.threads[0];
    keys("r", cx);
    let reply = inline_reply(&view, cx).expect("an inline reply");
    let body = cx.read_entity(&reply, |reply, _| {
        reply
            .compose
            .as_ref()
            .expect("a draft is open")
            .body
            .clone()
    });
    focus(&body, cx);
    keys("e s j k # a f z c g i", cx);
    assert_eq!(selected_thread(&view, cx), Some(first));
    assert_eq!(mailbox_of(&harness, first), harness.inbox_id);
    assert!(!flagged(&harness, first));
    assert_eq!(compose_windows(&view, cx), 0);
    assert!(cx.update(|window, cx| body.focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn the_keymap_switches_without_a_restart(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    keys("j", cx);
    assert_eq!(
        selected_thread(&view, cx),
        Some(threads[0]),
        "the standard keymap leaves j alone"
    );

    let store = harness.store.clone();
    cx.update(|_, cx| keymap::use_keymap(&store, KeymapName::Gmail, cx))
        .expect("the choice saves");
    cx.run_until_parked();
    keys("j", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[1]));
    assert_eq!(
        harness.store.ui_preferences().expect("loads").keymap,
        KeymapName::Gmail
    );

    cx.update(|_, cx| keymap::use_keymap(&store, KeymapName::Default, cx))
        .expect("the choice saves");
    cx.run_until_parked();
    keys("j", cx);
    assert_eq!(
        selected_thread(&view, cx),
        Some(threads[1]),
        "j is gone again"
    );
    // The standard keys still work after both switches.
    keys("down", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[2]));
}
