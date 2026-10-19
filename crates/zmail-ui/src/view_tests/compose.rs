//! Compose (UI plan Phase 5): autosave that never moves the cursor.

use std::time::Duration;

use gpui::{AppContext as _, Focusable as _, TestAppContext, VisualTestContext};
use zmail_core::NewDraft;

use super::{Harness, Setup, compose_inputs, cursor, focus, open_workspace, text};
use crate::actions::SaveDraft;
use crate::compose::AUTOSAVE_DELAY;

/// The stored body of a draft.
fn stored_body(harness: &Harness, draft_id: i64) -> String {
    harness
        .store
        .draft_detail(draft_id)
        .expect("the draft loads")
        .expect("the draft exists")
        .body
}

/// Lets the autosave timer fire and the shell's refreshes run.
fn wait(duration: Duration, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(duration);
    cx.run_until_parked();
}

#[gpui::test]
fn autosave_waits_for_a_pause_and_keeps_the_cursor(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let (draft_id, to, body, attachment_path) = compose_inputs(&view, cx);
    assert!(
        cx.update(|window, cx| to.focus_handle(cx).is_focused(window)),
        "Compose focuses the To field"
    );

    // Type, then edit the middle of the body.
    focus(&body, cx);
    cx.simulate_input("Hello world");
    cx.simulate_keystrokes("left left left left left");
    cx.simulate_input("big ");
    assert_eq!(text(&body, cx), "Hello big world");
    assert_eq!(cursor(&body, cx), "Hello big ".len());
    assert_eq!(
        stored_body(&harness, draft_id),
        "",
        "nothing saves while typing"
    );

    // Keep typing just inside the delay: the save keeps waiting.
    wait(AUTOSAVE_DELAY - Duration::from_millis(100), cx);
    cx.simulate_input("and ");
    wait(AUTOSAVE_DELAY - Duration::from_millis(100), cx);
    assert_eq!(stored_body(&harness, draft_id), "");

    // A pause saves, and the save, the debounced refresh and the
    // five-second background refresh leave the inputs and cursor alone.
    wait(Duration::from_millis(200), cx);
    assert_eq!(stored_body(&harness, draft_id), "Hello big and world");
    wait(Duration::from_secs(6), cx);
    let (_, _, same_body, same_path) = compose_inputs(&view, cx);
    assert!(
        same_body == body && same_path == attachment_path,
        "the refresh does not rebuild the compose inputs"
    );
    assert_eq!(text(&body, cx), "Hello big and world");
    assert_eq!(
        cursor(&body, cx),
        "Hello big and ".len(),
        "the cursor stays put"
    );
    cx.simulate_input("x");
    assert_eq!(text(&body, cx), "Hello big and xworld");

    // A half-typed attachment path survives the refreshes too.
    focus(&attachment_path, cx);
    cx.simulate_input("/tmp/half");
    wait(Duration::from_secs(6), cx);
    assert_eq!(text(&attachment_path, cx), "/tmp/half");

    // Saving explicitly saves at once and keeps the inputs.
    focus(&body, cx);
    cx.simulate_input("y");
    cx.dispatch_action(SaveDraft);
    cx.run_until_parked();
    assert_eq!(stored_body(&harness, draft_id), "Hello big and xyworld");
    assert_eq!(cursor(&body, cx), "Hello big and xy".len());
    let (_, _, same_body, _) = compose_inputs(&view, cx);
    assert!(
        same_body == body,
        "saving does not rebuild the compose inputs"
    );
}

#[gpui::test]
fn opening_another_draft_saves_the_last_keystrokes(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let (draft_id, _, body, _) = compose_inputs(&view, cx);
    focus(&body, cx);
    cx.simulate_input("Unsaved");

    let account_id = harness.store.list_accounts().expect("accounts load")[0].id;
    let identity_id = harness
        .store
        .list_identities(account_id)
        .expect("identities load")[0]
        .id;
    let other = harness
        .store
        .create_draft(&NewDraft {
            account_id,
            identity_id,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Other".to_owned(),
            body: String::new(),
        })
        .expect("the draft is created");
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_draft(other, window, cx)));
    cx.run_until_parked();

    assert_eq!(stored_body(&harness, draft_id), "Unsaved");
    assert_eq!(compose_inputs(&view, cx).0, other);
}

#[gpui::test]
fn the_drafts_list_follows_the_subject_after_autosave(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Account);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let subject = cx.read_entity(&view, |view, cx| {
        view.compose
            .read(cx)
            .compose
            .as_ref()
            .expect("a draft is open")
            .subject
            .clone()
    });
    focus(&subject, cx);
    cx.simulate_input("Quarterly report");
    // The autosave, then the shell's debounced refresh.
    wait(AUTOSAVE_DELAY + Duration::from_secs(1), cx);
    let subjects = cx.read_entity(&view, |view, _| {
        view.drafts
            .iter()
            .map(|draft| draft.subject.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(subjects, vec!["Quarterly report".to_owned()]);
}
