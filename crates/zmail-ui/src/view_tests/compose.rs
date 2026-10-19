//! Compose (UI plan Phase 5): autosave that never moves the cursor,
//! recipient chips with autocomplete, and the Cc, Bcc and From fields.

use std::time::Duration;

use gpui::{
    AppContext as _, ClipboardItem, Entity, Focusable as _, TestAppContext, VisualTestContext,
};
use zmail_core::{AccountSyncSnapshot, NewDraft, NewIdentity};

use super::{Harness, Setup, click, compose_inputs, cursor, focus, keys, open_workspace, text};
use crate::WorkspaceView;
use crate::actions::{SaveDraft, ShowBcc, ShowCc};
use crate::compose::{AUTOSAVE_DELAY, ComposeState, RecipientField};

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

/// Reads something from the open draft's compose state.
fn compose<T>(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    read: impl FnOnce(&ComposeState) -> T,
) -> T {
    cx.read_entity(view, |view, cx| {
        read(
            view.compose
                .read(cx)
                .compose
                .as_ref()
                .expect("a draft is open"),
        )
    })
}

/// A recipient field's chips as saved, and which are valid.
fn chips(field: &Entity<RecipientField>, cx: &mut VisualTestContext) -> Vec<(String, bool)> {
    cx.read_entity(field, |field, _| {
        field
            .chips()
            .iter()
            .map(|chip| (chip.header_value(), chip.is_valid()))
            .collect()
    })
}

fn suggestions(field: &Entity<RecipientField>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.read_entity(field, |field, _| {
        field
            .suggestions()
            .iter()
            .map(|suggestion| suggestion.email.clone())
            .collect()
    })
}

/// Opens a new draft with focus in To.
fn new_draft(
    setup: Setup,
    cx: &mut TestAppContext,
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Harness) {
    let (view, cx, harness) = open_workspace(cx, setup);
    // Focus events (leaving a field) only fire in an active window.
    cx.update(|window, _| window.activate_window());
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    (view, cx, harness)
}

#[gpui::test]
fn separators_enter_and_leaving_the_field_make_chips(cx: &mut TestAppContext) {
    let (view, cx, harness) = new_draft(Setup::Account, cx);
    let to = compose(&view, cx, |compose| compose.to.clone());
    let input = compose_inputs(&view, cx).1;

    cx.simulate_input("alice@example.com,");
    cx.simulate_input("\"Lee, Carol\" <carol@example.com>;");
    assert_eq!(text(&input, cx), "");
    cx.simulate_input("not an address");
    keys("enter", cx);
    cx.simulate_input("dan@example.net");
    // Tab moves on and keeps the typed address as a chip.
    keys("tab", cx);

    assert_eq!(
        chips(&to, cx),
        [
            ("alice@example.com".to_owned(), true),
            ("\"Lee, Carol\" <carol@example.com>".to_owned(), true),
            ("not an address".to_owned(), false),
            ("dan@example.net".to_owned(), true),
        ]
    );
    // The same address twice is one chip.
    focus(&input, cx);
    cx.simulate_input("ALICE@example.com,");
    assert_eq!(chips(&to, cx).len(), 4);

    wait(AUTOSAVE_DELAY, cx);
    let draft_id = compose_inputs(&view, cx).0;
    let stored = harness
        .store
        .draft_detail(draft_id)
        .expect("the draft loads")
        .expect("the draft exists");
    assert_eq!(
        stored.to,
        [
            "alice@example.com",
            "\"Lee, Carol\" <carol@example.com>",
            "not an address",
            "dan@example.net"
        ]
    );
}

#[gpui::test]
fn backspace_and_the_remove_button_take_chips_away(cx: &mut TestAppContext) {
    let (view, cx, _harness) = new_draft(Setup::Account, cx);
    let to = compose(&view, cx, |compose| compose.to.clone());
    cx.simulate_input("a@example.com, b@example.com, c@example.com, d");
    // Backspace edits the typed text first, then removes the last chip.
    keys("backspace", cx);
    assert_eq!(chips(&to, cx).len(), 3);
    keys("backspace", cx);
    assert_eq!(
        chips(&to, cx)
            .into_iter()
            .map(|(value, _)| value)
            .collect::<Vec<_>>(),
        ["a@example.com", "b@example.com"]
    );
    click("recipient-To-0-remove", cx);
    assert_eq!(
        chips(&to, cx)
            .into_iter()
            .map(|(value, _)| value)
            .collect::<Vec<_>>(),
        ["b@example.com"]
    );
}

#[gpui::test]
fn pasting_a_list_makes_a_chip_of_each_address(cx: &mut TestAppContext) {
    let (view, cx, _harness) = new_draft(Setup::Account, cx);
    let to = compose(&view, cx, |compose| compose.to.clone());
    let input = compose_inputs(&view, cx).1;
    cx.write_to_clipboard(ClipboardItem::new_string(
        "Ann <ann@example.com>; ben@example.com\ncat@example.com".to_owned(),
    ));
    keys("secondary-v", cx);
    assert_eq!(
        chips(&to, cx)
            .into_iter()
            .map(|(value, _)| value)
            .collect::<Vec<_>>(),
        [
            "Ann <ann@example.com>",
            "ben@example.com",
            "cat@example.com"
        ]
    );
    assert_eq!(text(&input, cx), "");
    // A single address pastes as text, to edit before it becomes a chip.
    cx.write_to_clipboard(ClipboardItem::new_string("dee@example.com".to_owned()));
    keys("secondary-v", cx);
    assert_eq!(text(&input, cx), "dee@example.com");
    assert_eq!(chips(&to, cx).len(), 3);
}

#[gpui::test]
fn autocomplete_suggests_from_the_address_history(cx: &mut TestAppContext) {
    // The Inbox has mail from Kai Santos <kai@example.test>.
    let (view, cx, _harness) = new_draft(Setup::Mail, cx);
    let to = compose(&view, cx, |compose| compose.to.clone());
    let input = compose_inputs(&view, cx).1;

    cx.simulate_input("sant");
    assert_eq!(suggestions(&to, cx), ["kai@example.test"]);
    // Escape closes the list and keeps the text.
    keys("escape", cx);
    assert!(suggestions(&to, cx).is_empty());
    assert_eq!(text(&input, cx), "sant");
    // Enter adds the highlighted suggestion.
    cx.simulate_input("o");
    keys("down enter", cx);
    assert_eq!(
        chips(&to, cx),
        [("Kai Santos <kai@example.test>".to_owned(), true)]
    );
    assert_eq!(text(&input, cx), "");

    // A suggestion that is already a chip is not offered again.
    cx.simulate_input("kai");
    assert!(suggestions(&to, cx).is_empty());
    keys("backspace backspace backspace", cx);

    // My own address is never suggested.
    cx.simulate_input("me@");
    assert!(suggestions(&to, cx).is_empty());
}

#[gpui::test]
fn clicking_a_suggestion_adds_it(cx: &mut TestAppContext) {
    let (view, cx, _harness) = new_draft(Setup::Mail, cx);
    let to = compose(&view, cx, |compose| compose.to.clone());
    cx.simulate_input("kai");
    click("suggestion-To-0", cx);
    assert_eq!(
        chips(&to, cx),
        [("Kai Santos <kai@example.test>".to_owned(), true)],
        "the click does not first turn the typed text into a chip"
    );
}

#[gpui::test]
fn cc_and_bcc_hide_behind_toggles(cx: &mut TestAppContext) {
    let (view, cx, _harness) = new_draft(Setup::Account, cx);
    let shown = |cx: &mut VisualTestContext| {
        compose(&view, cx, |compose| (compose.show_cc, compose.show_bcc))
    };
    assert_eq!(shown(cx), (false, false));
    assert!(cx.debug_bounds("recipient-Cc-0").is_none());
    assert!(cx.debug_bounds("compose-show-cc").is_some());

    click("compose-show-cc", cx);
    assert_eq!(shown(cx), (true, false));
    assert!(cx.debug_bounds("compose-show-cc").is_none());
    let cc = compose(&view, cx, |compose| compose.cc.clone());
    assert!(
        cx.update(|window, cx| cc.focus_handle(cx).is_focused(window)),
        "the Cc field takes focus"
    );
    cx.simulate_input("cc@example.com,");
    assert_eq!(chips(&cc, cx).len(), 1);

    cx.dispatch_action(ShowBcc);
    cx.run_until_parked();
    assert_eq!(shown(cx), (true, true));
    // Showing an already shown field just focuses it.
    cx.dispatch_action(ShowCc);
    cx.run_until_parked();
    assert_eq!(shown(cx), (true, true));
}

#[gpui::test]
fn a_draft_with_cc_opens_with_the_field_shown(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    let account_id = harness.store.list_accounts().expect("accounts load")[0].id;
    let identity_id = harness
        .store
        .list_identities(account_id)
        .expect("identities load")[0]
        .id;
    let draft_id = harness
        .store
        .create_draft(&NewDraft {
            account_id,
            identity_id,
            to: vec!["to@example.com".to_owned()],
            cc: vec!["cc@example.com".to_owned()],
            bcc: Vec::new(),
            subject: String::new(),
            body: String::new(),
        })
        .expect("the draft is created");
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_draft(draft_id, window, cx)));
    cx.run_until_parked();
    assert_eq!(
        compose(&view, cx, |compose| (compose.show_cc, compose.show_bcc)),
        (true, false)
    );
}

#[gpui::test]
fn the_from_menu_shows_only_with_more_than_one_identity(cx: &mut TestAppContext) {
    let (view, cx, harness) = new_draft(Setup::Account, cx);
    let has_menu = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, cx| view.compose.read(cx).has_from_menu())
    };
    assert!(!has_menu(cx));

    let account_id = harness.store.list_accounts().expect("accounts load")[0].id;
    let identity = |name: &str, email: &str, is_primary| NewIdentity {
        display_name: name.to_owned(),
        email_address: email.to_owned(),
        signature: String::new(),
        is_primary,
    };
    harness
        .store
        .replace_account_snapshot(
            account_id,
            &AccountSyncSnapshot {
                checkpoint: "test".to_owned(),
                detail: "test".to_owned(),
                identities: Some(vec![
                    identity("Me", "me@example.test", true),
                    identity("Support", "support@example.test", false),
                ]),
                mailboxes: Vec::new(),
            },
        )
        .expect("the identities are stored");
    // The shell's background refresh picks the new identity up.
    wait(Duration::from_secs(6), cx);
    assert!(has_menu(cx));
}
