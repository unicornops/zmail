//! Compose (UI plan Phase 5): the compose window, autosave that never
//! moves the cursor, recipient chips with autocomplete, the Cc, Bcc and
//! From fields, and attachments.

use std::time::Duration;

use gpui::{
    AppContext as _, ClipboardItem, Entity, FileDropEvent, Focusable as _, TestAppContext,
    VisualTestContext, point, px,
};
use zmail_core::{AccountSyncSnapshot, NewDraft, NewIdentity};

use super::{
    Harness, Setup, click, click_on, compose_inputs, compose_window, compose_window_toasts, cursor,
    focus, keys, open_workspace, text,
};
use crate::actions::{AddAttachment, SaveDraft, ShowBcc, ShowCc};
use crate::components::TextInput;
use crate::compose::{AUTOSAVE_DELAY, ComposeState, ComposeView, RecipientField};
use crate::{DetailMode, WorkspaceView};

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

/// Reads something from a compose view's open draft.
fn state<T>(
    compose: &Entity<ComposeView>,
    cx: &mut VisualTestContext,
    read: impl FnOnce(&ComposeState, &gpui::App) -> T,
) -> T {
    cx.read_entity(compose, |compose, cx| {
        read(compose.compose.as_ref().expect("a draft is open"), cx)
    })
}

/// A compose view's draft and its To and body inputs.
fn inputs(
    compose: &Entity<ComposeView>,
    cx: &mut VisualTestContext,
) -> (i64, Entity<TextInput>, Entity<TextInput>) {
    state(compose, cx, |compose, cx| {
        (
            compose.draft_id,
            compose.to.read(cx).input().clone(),
            compose.body.clone(),
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

/// Opens a new message, which opens in a compose window with focus in
/// To. Returns the main window's context and the compose window's editor
/// and context.
fn new_draft(
    setup: Setup,
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceView>,
    &mut VisualTestContext,
    Entity<ComposeView>,
    VisualTestContext,
    Harness,
) {
    let (view, cx, harness) = open_workspace(cx, setup);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let (compose, mut window_cx) = compose_window(&view, cx);
    // Focus events (leaving a field) only fire in an active window.
    window_cx.update(|window, _| window.activate_window());
    window_cx.run_until_parked();
    (view, cx, compose, window_cx, harness)
}

#[gpui::test]
fn autosave_waits_for_a_pause_and_keeps_the_cursor(cx: &mut TestAppContext) {
    let (_view, _cx, compose, mut w, harness) = new_draft(Setup::Account, cx);
    let w = &mut w;
    let (draft_id, to, body) = inputs(&compose, w);
    assert!(
        w.update(|window, cx| to.focus_handle(cx).is_focused(window)),
        "Compose focuses the To field"
    );

    // Type, then edit the middle of the body.
    focus(&body, w);
    w.simulate_input("Hello world");
    w.simulate_keystrokes("left left left left left");
    w.simulate_input("big ");
    assert_eq!(text(&body, w), "Hello big world");
    assert_eq!(cursor(&body, w), "Hello big ".len());
    assert_eq!(
        stored_body(&harness, draft_id),
        "",
        "nothing saves while typing"
    );

    // Keep typing just inside the delay: the save keeps waiting.
    wait(AUTOSAVE_DELAY - Duration::from_millis(100), w);
    w.simulate_input("and ");
    wait(AUTOSAVE_DELAY - Duration::from_millis(100), w);
    assert_eq!(stored_body(&harness, draft_id), "");

    // A pause saves, and the save, the debounced refresh and the
    // five-second background refresh leave the inputs and cursor alone.
    wait(Duration::from_millis(200), w);
    assert_eq!(stored_body(&harness, draft_id), "Hello big and world");
    wait(Duration::from_secs(6), w);
    let (_, _, same_body) = inputs(&compose, w);
    assert!(
        same_body == body,
        "the refresh does not rebuild the compose inputs"
    );
    assert_eq!(text(&body, w), "Hello big and world");
    assert_eq!(
        cursor(&body, w),
        "Hello big and ".len(),
        "the cursor stays put"
    );
    w.simulate_input("x");
    assert_eq!(text(&body, w), "Hello big and xworld");

    // Saving explicitly saves at once and keeps the inputs.
    w.dispatch_action(SaveDraft);
    w.run_until_parked();
    assert_eq!(stored_body(&harness, draft_id), "Hello big and xworld");
    assert_eq!(cursor(&body, w), "Hello big and x".len());
    let (_, _, same_body) = inputs(&compose, w);
    assert!(
        same_body == body,
        "saving does not rebuild the compose inputs"
    );
}

#[gpui::test]
fn opening_another_draft_saves_the_last_keystrokes(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    let draft_id = cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_compose_in_pane_for_test(window, cx)
        })
    });
    cx.run_until_parked();
    let (_, _, body) = compose_inputs(&view, cx);
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
fn the_drafts_list_and_window_title_follow_the_subject(cx: &mut TestAppContext) {
    let (view, cx, compose, mut w, _harness) = new_draft(Setup::Account, cx);
    let subject = state(&compose, &mut w, |compose, _| compose.subject.clone());
    assert_eq!(w.window_title().as_deref(), Some("New Message"));
    focus(&subject, &mut w);
    w.simulate_input("Quarterly report");
    assert_eq!(w.window_title().as_deref(), Some("Quarterly report"));
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

#[gpui::test]
fn separators_enter_and_leaving_the_field_make_chips(cx: &mut TestAppContext) {
    let (_view, _cx, compose, mut w, harness) = new_draft(Setup::Account, cx);
    let cx = &mut w;
    let to = state(&compose, cx, |compose, _| compose.to.clone());
    let input = inputs(&compose, cx).1;

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
    let draft_id = inputs(&compose, cx).0;
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
    let (_view, _cx, compose, mut w, _harness) = new_draft(Setup::Account, cx);
    let cx = &mut w;
    let to = state(&compose, cx, |compose, _| compose.to.clone());
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
    let (_view, _cx, compose, mut w, _harness) = new_draft(Setup::Account, cx);
    let cx = &mut w;
    let to = state(&compose, cx, |compose, _| compose.to.clone());
    let input = inputs(&compose, cx).1;
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
    let (_view, _cx, compose, mut w, _harness) = new_draft(Setup::Mail, cx);
    let cx = &mut w;
    let to = state(&compose, cx, |compose, _| compose.to.clone());
    let input = inputs(&compose, cx).1;

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
    let (_view, _cx, compose, mut w, _harness) = new_draft(Setup::Mail, cx);
    let cx = &mut w;
    let to = state(&compose, cx, |compose, _| compose.to.clone());
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
    let (_view, _cx, compose, mut w, _harness) = new_draft(Setup::Account, cx);
    let cx = &mut w;
    let shown = |cx: &mut VisualTestContext| {
        state(&compose, cx, |compose, _| {
            (compose.show_cc, compose.show_bcc)
        })
    };
    assert_eq!(shown(cx), (false, false));
    assert!(cx.debug_bounds("recipient-Cc-0").is_none());
    assert!(cx.debug_bounds("compose-show-cc").is_some());

    click("compose-show-cc", cx);
    assert_eq!(shown(cx), (true, false));
    assert!(cx.debug_bounds("compose-show-cc").is_none());
    let cc = state(&compose, cx, |compose, _| compose.cc.clone());
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
    let pane = cx.read_entity(&view, |view, _| view.compose.clone());
    assert_eq!(
        state(&pane, cx, |compose, _| (compose.show_cc, compose.show_bcc)),
        (true, false)
    );
}

#[gpui::test]
fn the_from_menu_shows_only_with_more_than_one_identity(cx: &mut TestAppContext) {
    let (_view, _cx, compose, mut w, harness) = new_draft(Setup::Account, cx);
    let cx = &mut w;
    let has_menu =
        |cx: &mut VisualTestContext| cx.read_entity(&compose, |compose, _| compose.has_from_menu());
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
    // The shell's background refresh reaches the compose window too.
    wait(Duration::from_secs(6), cx);
    assert!(has_menu(cx));
}

/// Writes a small file to attach and returns its path.
fn attachment_file(harness: &Harness, name: &str) -> std::path::PathBuf {
    let directory = harness
        .store
        .path()
        .parent()
        .expect("the store has a directory")
        .join("to-attach");
    std::fs::create_dir_all(&directory).expect("the directory is created");
    let path = directory.join(name);
    std::fs::write(&path, b"Q4 numbers").expect("the file is written");
    path
}

fn attachment_names(compose: &Entity<ComposeView>, cx: &mut VisualTestContext) -> Vec<String> {
    state(compose, cx, |compose, _| {
        compose
            .attachments
            .iter()
            .map(|attachment| attachment.filename.clone())
            .collect()
    })
}

#[gpui::test]
fn new_mail_opens_in_its_own_window_and_escape_keeps_the_draft(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, harness) = new_draft(Setup::Account, cx);
    // The main window carries on as it was.
    assert_eq!(
        main.read_entity(&view, |view, _| view.detail_mode),
        DetailMode::Reader
    );
    let (draft_id, _, body) = inputs(&compose, &mut w);
    focus(&body, &mut w);
    w.simulate_input("Kept");

    // Composing again opens a second window: one window per draft.
    main.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    main.run_until_parked();
    assert_eq!(
        main.read_entity(&view, |view, _| view.compose_windows.len()),
        2
    );

    // Escape closes the window and saves what was typed, even inside the
    // autosave delay.
    keys("escape", &mut w);
    assert_eq!(stored_body(&harness, draft_id), "Kept");
    assert_eq!(
        main.read_entity(&view, |view, _| view.compose_windows.len()),
        1
    );
    assert!(
        harness
            .store
            .list_drafts(None)
            .expect("drafts load")
            .iter()
            .any(|draft| draft.id == draft_id),
        "the draft stays in Drafts"
    );
}

#[gpui::test]
fn closing_the_window_keeps_the_draft(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, harness) = new_draft(Setup::Account, cx);
    let (draft_id, _, body) = inputs(&compose, &mut w);
    focus(&body, &mut w);
    w.simulate_input("Typed just before closing");
    // The close button asks the window whether it may close (and the
    // draft saves then); the test platform leaves the closing to us.
    assert!(w.simulate_close(), "the window agrees to close");
    assert_eq!(stored_body(&harness, draft_id), "Typed just before closing");
    w.update(|window, _| window.remove_window());
    w.run_until_parked();
    assert_eq!(stored_body(&harness, draft_id), "Typed just before closing");
    assert!(main.read_entity(&view, |view, _| view.compose_windows.is_empty()));
}

#[gpui::test]
fn a_draft_is_edited_in_one_place(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Account);
    let draft_id = cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_compose_in_pane_for_test(window, cx)
        })
    });
    cx.run_until_parked();
    let pane = cx.read_entity(&view, |view, _| view.compose.clone());

    // Open in Window moves the draft out of the pane.
    click("compose-pop-out", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.compose_windows.len()),
        1
    );
    assert_eq!(
        cx.read_entity(&pane, |pane, _| (pane.draft_id(), pane.showing_in_window())),
        (None, Some(draft_id))
    );
    // Selecting it again in the list keeps it in its window.
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_draft(draft_id, window, cx)));
    cx.run_until_parked();
    assert_eq!(cx.read_entity(&pane, |pane, _| pane.draft_id()), None);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.compose_windows.len()),
        1
    );

    // Once the window closes, the pane edits it again.
    let (_, mut w) = compose_window(&view, cx);
    keys("escape", &mut w);
    cx.run_until_parked();
    assert_eq!(
        cx.read_entity(&pane, |pane, _| pane.draft_id()),
        Some(draft_id)
    );
}

#[gpui::test]
fn attachments_come_from_the_file_picker_and_dropped_files(cx: &mut TestAppContext) {
    let (_view, _main, compose, mut w, harness) = new_draft(Setup::Account, cx);
    let w = &mut w;
    let report = attachment_file(&harness, "report.pdf");
    let notes = attachment_file(&harness, "notes.txt");

    // Attach Files… opens the system picker; its files become cards.
    let picked = vec![report.clone(), notes.clone()];
    w.dispatch_action(AddAttachment);
    w.run_until_parked();
    assert!(w.did_prompt_for_paths());
    w.simulate_path_prompt_response(move |options| {
        assert!(options.files && options.multiple && !options.directories);
        Some(picked)
    });
    w.run_until_parked();
    assert_eq!(attachment_names(&compose, w), ["report.pdf", "notes.txt"]);

    // A cancelled picker attaches nothing.
    keys("secondary-shift-a", w);
    w.simulate_path_prompt_response(|_| None);
    w.run_until_parked();
    assert_eq!(attachment_names(&compose, w).len(), 2);

    // Remove takes a card away.
    let first = state(&compose, w, |compose, _| compose.attachments[0].id);
    click_on(format!("compose-attachment-remove-{first}"), w);
    assert_eq!(attachment_names(&compose, w), ["notes.txt"]);

    // Files dropped onto the window are attached.
    let position = point(px(300.), px(300.));
    w.simulate_event(FileDropEvent::Entered {
        position,
        paths: gpui::ExternalPaths(vec![report].into()),
    });
    w.simulate_event(FileDropEvent::Submit { position });
    w.run_until_parked();
    assert_eq!(attachment_names(&compose, w), ["notes.txt", "report.pdf"]);
}

#[gpui::test]
fn send_closes_the_window_and_queues_the_message(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, harness) = new_draft(Setup::Account, cx);
    let (draft_id, _, _) = inputs(&compose, &mut w);
    // Sending with no recipients explains why, in the compose window.
    keys("secondary-enter", &mut w);
    assert_eq!(compose_window_toasts(&view, main).len(), 1);

    w.simulate_input("kai@example.test,");
    keys("secondary-enter", &mut w);
    assert!(main.read_entity(&view, |view, _| view.compose_windows.is_empty()));
    let draft = harness
        .store
        .draft_detail(draft_id)
        .expect("the draft loads")
        .expect("the draft exists");
    assert_eq!(draft.to, ["kai@example.test"]);
    assert_ne!(draft.state, zmail_core::DraftState::Editing);
}

/// The outbox jobs' ids.
fn outbox(harness: &Harness) -> Vec<i64> {
    harness
        .store
        .list_outbox_jobs(None)
        .expect("jobs load")
        .into_iter()
        .map(|job| job.id)
        .collect()
}

fn draft_state(harness: &Harness, draft_id: i64) -> zmail_core::DraftState {
    harness
        .store
        .draft_detail(draft_id)
        .expect("the draft loads")
        .expect("the draft exists")
        .state
}

#[gpui::test]
fn undo_send_holds_the_message_and_reopens_its_window(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, harness) = new_draft(Setup::Account, cx);
    let (draft_id, _, _) = inputs(&compose, &mut w);
    w.simulate_input("kai@example.test,");
    keys("secondary-enter", &mut w);

    // The window closes; the main window offers Undo while the message
    // waits out the default ten seconds.
    assert!(main.read_entity(&view, |view, _| view.compose_windows.is_empty()));
    assert_eq!(super::toasts(&view, main), ["Sending message…"]);
    assert_eq!(outbox(&harness).len(), 1);
    assert_eq!(
        draft_state(&harness, draft_id),
        zmail_core::DraftState::Queued
    );

    // Undo takes it back and reopens the draft in a window.
    super::click_toast_undo(&view, main);
    assert!(outbox(&harness).is_empty());
    assert_eq!(
        draft_state(&harness, draft_id),
        zmail_core::DraftState::Editing
    );
    let (reopened, mut w) = compose_window(&view, main);
    assert_eq!(inputs(&reopened, &mut w).0, draft_id);
    assert_eq!(
        state(&reopened, &mut w, |compose, cx| compose
            .to
            .read(cx)
            .recipients(cx)),
        ["kai@example.test"]
    );
    assert_eq!(super::toasts(&view, main), ["Sending undone"]);
}

#[gpui::test]
fn the_undo_toast_lasts_as_long_as_the_undo_window(cx: &mut TestAppContext) {
    let (view, main, _compose, mut w, harness) = new_draft(Setup::Account, cx);
    // A five-second window.
    let mut preferences = harness.store.ui_preferences().expect("preferences load");
    preferences.undo_send_delay_secs = 5;
    harness
        .store
        .save_ui_preferences(&preferences)
        .expect("preferences save");
    w.simulate_input("kai@example.test,");
    keys("secondary-enter", &mut w);
    assert_eq!(super::toasts(&view, main), ["Sending message…"]);
    wait(Duration::from_millis(4900), main);
    assert_eq!(super::toasts(&view, main), ["Sending message…"]);
    wait(Duration::from_millis(200), main);
    assert!(super::toasts(&view, main).is_empty());
}

#[gpui::test]
fn undo_after_sending_started_says_it_is_too_late(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, harness) = new_draft(Setup::Account, cx);
    let (draft_id, _, _) = inputs(&compose, &mut w);
    w.simulate_input("kai@example.test,");
    keys("secondary-enter", &mut w);
    // The outbox claims the job (as it would once the window ends).
    let job_id = outbox(&harness)[0];
    harness.store.retry_outbox_job(job_id).expect("due now");
    let account_id = harness.store.list_accounts().expect("accounts load")[0].id;
    harness
        .store
        .claim_due_outbox_job(Some(account_id))
        .expect("claim works")
        .expect("the job is claimed");

    super::click_toast_undo(&view, main);
    assert_eq!(
        super::toasts(&view, main),
        ["Too late to undo: the message has started sending."]
    );
    assert_eq!(
        draft_state(&harness, draft_id),
        zmail_core::DraftState::Sending
    );
    assert!(main.read_entity(&view, |view, _| view.compose_windows.is_empty()));
}

#[gpui::test]
fn undoing_a_reply_sent_from_the_pane_reopens_it_there(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    keys("secondary-r", cx);
    let (draft_id, _, _) = compose_inputs(&view, cx);
    keys("secondary-enter", cx);
    let pane = cx.read_entity(&view, |view, _| view.compose.clone());
    assert_eq!(cx.read_entity(&pane, |pane, _| pane.draft_id()), None);
    assert_eq!(super::toasts(&view, cx), ["Sending message…"]);

    super::click_toast_undo(&view, cx);
    assert_eq!(
        cx.read_entity(&pane, |pane, _| pane.draft_id()),
        Some(draft_id)
    );
    assert_eq!(
        draft_state(&harness, draft_id),
        zmail_core::DraftState::Editing
    );
    assert!(cx.read_entity(&view, |view, _| view.compose_windows.is_empty()));
}

#[gpui::test]
fn with_undo_send_off_the_message_goes_at_once(cx: &mut TestAppContext) {
    let (view, main, _compose, mut w, harness) = new_draft(Setup::Account, cx);
    let mut preferences = harness.store.ui_preferences().expect("preferences load");
    preferences.undo_send_delay_secs = 0;
    harness
        .store
        .save_ui_preferences(&preferences)
        .expect("preferences save");
    w.simulate_input("kai@example.test,");
    keys("secondary-enter", &mut w);
    assert!(super::toasts(&view, main).is_empty());
    assert_eq!(
        main.read_entity(&view, |view, _| view.status_message.clone()),
        Some("Message queued for sending.".to_owned())
    );
}
