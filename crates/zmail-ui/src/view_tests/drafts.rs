//! Drafts and sent mail: a sent message leaves Drafts and the Outbox,
//! Discard deletes a draft after asking (from compose, the Delete key in
//! Drafts and a draft row's menu), and an empty new message closed again
//! leaves nothing behind.

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zmail_core::NewDraft;
use zmail_store::LocalStore;

use super::{
    Harness, Setup, click, compose_window, deliver, focus, keys, open_workspace,
    queue_test_message, toasts,
};
use crate::actions::{DiscardDraft, ShowDrafts};
use crate::compose::ComposeView;
use crate::{ListMode, WorkspaceView};

/// Stores a draft being written, with a recipient and `subject`.
fn store_draft(store: &LocalStore, subject: &str) -> i64 {
    let account_id = store.list_accounts().expect("accounts")[0].id;
    let identity_id = store.list_identities(account_id).expect("identities")[0].id;
    store
        .create_draft(&NewDraft {
            account_id,
            identity_id,
            to: vec!["kai@example.test".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_owned(),
            body: "Notes".to_owned(),
            threading: Default::default(),
        })
        .expect("the draft is stored")
}

fn refresh(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    view.update(cx, |view, cx| {
        view.refresh_from_store(cx);
        cx.notify();
    });
    cx.run_until_parked();
}

/// The drafts Drafts lists and the Outbox's jobs, as the shell holds them.
fn listed(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> (Vec<i64>, Vec<i64>) {
    cx.read_entity(view, |view, _| {
        (
            view.drafts.iter().map(|draft| draft.id).collect(),
            view.outbox_jobs.iter().map(|job| job.id).collect(),
        )
    })
}

/// The sidebar's Drafts badge and Outbox count.
fn sidebar_counts(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> (usize, usize) {
    cx.read_entity(view, |view, cx| {
        let state = view.sidebar.read(cx).state();
        (state.draft_count, state.outbox.waiting)
    })
}

fn stored_drafts(harness: &Harness) -> Vec<i64> {
    harness
        .store
        .list_drafts(None)
        .expect("the drafts load")
        .into_iter()
        .map(|draft| draft.id)
        .collect()
}

/// Whether `compose`'s discard confirmation shows, and whether its Cancel
/// has focus.
fn confirmation(compose: &Entity<ComposeView>, cx: &mut VisualTestContext) -> Option<bool> {
    cx.update(|window, cx| compose.read(cx).discard_confirmation(window))
}

#[gpui::test]
fn sent_mail_leaves_drafts_and_the_outbox(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let writing = store_draft(&harness.store, "Still writing");
    let job = queue_test_message(&harness.store, "On its way");
    refresh(&view, cx);
    assert_eq!(listed(&view, cx), (vec![writing], vec![job]));
    assert_eq!(sidebar_counts(&view, cx), (1, 1));
    // The Outbox row still names the queued message.
    let named = cx.read_entity(&view, |view, cx| {
        let state = view.message_list.read(cx).state();
        state
            .unsent_drafts
            .iter()
            .any(|draft| draft.subject == "On its way")
    });
    assert!(named);

    deliver(&harness.store, job);
    refresh(&view, cx);
    assert_eq!(listed(&view, cx), (vec![writing], Vec::new()));
    assert_eq!(sidebar_counts(&view, cx), (1, 0));
    assert_eq!(stored_drafts(&harness).len(), 2, "the store keeps both");
}

#[gpui::test]
fn delete_in_drafts_asks_before_discarding(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let older = store_draft(&harness.store, "Older plans");
    let newer = store_draft(&harness.store, "Newer plans");
    refresh(&view, cx);
    cx.dispatch_action(ShowDrafts);
    cx.run_until_parked();
    assert_eq!(
        cx.read_entity(&view, |view, _| view.list_mode),
        ListMode::Drafts
    );
    keys("home", cx);
    let pane = cx.read_entity(&view, |view, _| view.compose.clone());
    assert_eq!(
        cx.read_entity(&pane, |pane, _| pane.draft_id()),
        Some(newer)
    );

    // Delete asks, with Cancel focused; Escape keeps the draft.
    keys("delete", cx);
    assert_eq!(confirmation(&pane, cx), Some(true));
    keys("escape", cx);
    assert_eq!(confirmation(&pane, cx), None);
    assert_eq!(stored_drafts(&harness).len(), 2);

    keys("delete", cx);
    click("discard-draft-confirm", cx);
    assert_eq!(stored_drafts(&harness), [older]);
    assert_eq!(toasts(&view, cx), ["Draft discarded"]);
    // The draft below takes its place.
    assert_eq!(
        cx.read_entity(&pane, |pane, _| pane.draft_id()),
        Some(older)
    );
    assert_eq!(listed(&view, cx).0, [older]);

    // A draft row's menu offers the same.
    let bounds = cx
        .debug_bounds(Box::leak(format!("draft-row-{older}").into_boxed_str()))
        .expect("the row is drawn");
    cx.simulate_mouse_down(
        bounds.center(),
        gpui::MouseButton::Right,
        Default::default(),
    );
    cx.run_until_parked();
    let menu = cx
        .read_entity(&view, |view, cx| view.message_list.read(cx).context_menu())
        .expect("the menu opens");
    menu.update_in(cx, |menu, window, cx| menu.activate(0, window, cx));
    cx.run_until_parked();
    assert_eq!(confirmation(&pane, cx), Some(true));
    click("discard-draft-confirm", cx);
    assert!(stored_drafts(&harness).is_empty());
}

#[gpui::test]
fn discard_in_a_compose_window_closes_it(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    keys("secondary-n", cx);
    let (compose, mut w) = compose_window(&view, cx);
    let draft_id = cx
        .read_entity(&compose, |compose, _| compose.draft_id())
        .expect("a draft is open");
    let body = cx.read_entity(&compose, |compose, _| {
        compose.compose.as_ref().expect("a draft").body.clone()
    });
    focus(&body, &mut w);
    w.simulate_input("Second thoughts");

    w.dispatch_action(DiscardDraft);
    w.run_until_parked();
    assert_eq!(confirmation(&compose, &mut w), Some(true));
    click("discard-draft-confirm", &mut w);
    cx.run_until_parked();
    assert!(
        cx.read_entity(&view, |view, _| view.compose_windows.is_empty()),
        "the window closed"
    );
    assert!(!stored_drafts(&harness).contains(&draft_id));
    assert_eq!(toasts(&view, cx), ["Draft discarded"]);
}

#[gpui::test]
fn an_empty_new_message_closed_again_leaves_no_draft(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    keys("secondary-n", cx);
    let (_, mut w) = compose_window(&view, cx);
    assert_eq!(stored_drafts(&harness).len(), 1);
    keys("escape", &mut w);
    cx.run_until_parked();
    assert!(stored_drafts(&harness).is_empty(), "nothing to keep");

    // The window's close button too.
    keys("secondary-n", cx);
    let (_, mut w) = compose_window(&view, cx);
    assert!(w.simulate_close());
    w.update(|window, _| window.remove_window());
    cx.run_until_parked();
    assert!(stored_drafts(&harness).is_empty());

    // An empty draft's Discard does not ask: it goes, with its window.
    keys("secondary-n", cx);
    let (_, mut w) = compose_window(&view, cx);
    w.dispatch_action(DiscardDraft);
    cx.run_until_parked();
    assert!(cx.read_entity(&view, |view, _| view.compose_windows.is_empty()));
    assert!(stored_drafts(&harness).is_empty());
}
