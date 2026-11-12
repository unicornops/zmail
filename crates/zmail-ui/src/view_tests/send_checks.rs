//! What Send checks before it queues a draft: recipients that stop it,
//! with a plain sentence and focus in the field at fault; an empty subject
//! and a forgotten attachment, which ask first; and the main window
//! saying where a draft went when its compose window closes.

use gpui::{AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext};
use zmail_core::{DraftState, NewDraft};

use super::{
    Harness, Setup, click, compose_window, compose_window_toasts, keys, open_workspace, toasts,
};
use crate::WorkspaceView;
use crate::components::TextInput;
use crate::compose::send_checks::{ALREADY_QUEUED, NO_RECIPIENTS, SendCheck};
use crate::compose::{ComposeView, RecipientField};

/// A stored draft for the test account.
struct Draft<'a> {
    to: &'a [&'a str],
    cc: &'a [&'a str],
    subject: &'a str,
    body: &'a str,
}

/// Opens the workspace with `draft` stored and opened in its compose
/// window. Returns the main window, the draft's editor and window, the
/// draft and the harness.
fn open_draft<'a>(
    draft: Draft<'_>,
    cx: &'a mut TestAppContext,
) -> (
    Entity<WorkspaceView>,
    &'a mut VisualTestContext,
    Entity<ComposeView>,
    VisualTestContext,
    i64,
    Harness,
) {
    let (view, main, harness) = open_workspace(cx, Setup::Account);
    let account_id = harness.store.list_accounts().expect("accounts load")[0].id;
    let identity_id = harness
        .store
        .list_identities(account_id)
        .expect("identities load")[0]
        .id;
    let owned = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
    let draft_id = harness
        .store
        .create_draft(&NewDraft {
            account_id,
            identity_id,
            to: owned(draft.to),
            cc: owned(draft.cc),
            bcc: Vec::new(),
            subject: draft.subject.to_owned(),
            body: draft.body.to_owned(),
            threading: Default::default(),
        })
        .expect("the draft is stored");
    main.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_draft_in_window(draft_id, true, window, cx)
        })
    });
    main.run_until_parked();
    let (compose, mut w) = compose_window(&view, main);
    // Focus events (leaving a field) only fire in an active window.
    w.update(|window, _| window.activate_window());
    w.run_until_parked();
    (view, main, compose, w, draft_id, harness)
}

fn outbox_len(harness: &Harness) -> usize {
    harness
        .store
        .list_outbox_jobs(None)
        .expect("jobs load")
        .len()
}

fn problem(compose: &Entity<ComposeView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.read_entity(compose, |compose, _| {
        compose.send_problem().map(str::to_owned)
    })
}

fn check(compose: &Entity<ComposeView>, cx: &mut VisualTestContext) -> Option<SendCheck> {
    cx.read_entity(compose, |compose, _| compose.send_check())
}

fn field(
    compose: &Entity<ComposeView>,
    cx: &mut VisualTestContext,
    pick: impl FnOnce(&crate::compose::ComposeState) -> &Entity<RecipientField>,
) -> Entity<RecipientField> {
    cx.read_entity(compose, |compose, _| {
        pick(compose.compose.as_ref().expect("a draft is open")).clone()
    })
}

fn input(
    compose: &Entity<ComposeView>,
    cx: &mut VisualTestContext,
    pick: impl FnOnce(&crate::compose::ComposeState) -> &Entity<TextInput>,
) -> Entity<TextInput> {
    cx.read_entity(compose, |compose, _| {
        pick(compose.compose.as_ref().expect("a draft is open")).clone()
    })
}

fn field_focused(field: &Entity<RecipientField>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| field.read(cx).input().focus_handle(cx).is_focused(window))
}

fn input_focused(input: &Entity<TextInput>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| input.focus_handle(cx).is_focused(window))
}

fn window_open(view: &Entity<WorkspaceView>, main: &mut VisualTestContext) -> bool {
    !main.read_entity(view, |view, _| view.compose_windows.is_empty())
}

#[gpui::test]
fn send_with_no_recipients_says_what_to_fix_in_the_window(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, _, harness) = open_draft(
        Draft {
            to: &[],
            cc: &[],
            subject: "Plans",
            body: "Hello",
        },
        cx,
    );
    keys("secondary-enter", &mut w);

    // The window stays open, says what to fix and puts the cursor in To;
    // nothing is queued, and no toast is the only word on it.
    assert!(window_open(&view, main));
    assert_eq!(problem(&compose, &mut w).as_deref(), Some(NO_RECIPIENTS));
    let to = field(&compose, &mut w, |compose| &compose.to);
    assert!(field_focused(&to, &mut w));
    assert_eq!(outbox_len(&harness), 0);
    assert!(compose_window_toasts(&view, main).is_empty());

    // Typing an address clears it, and Send goes.
    w.simulate_input("kai@example.test");
    w.run_until_parked();
    assert_eq!(problem(&compose, &mut w), None);
    keys("secondary-enter", &mut w);
    assert!(!window_open(&view, main));
    assert_eq!(outbox_len(&harness), 1);
}

#[gpui::test]
fn an_invalid_recipient_stops_the_send_and_focuses_its_field(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, draft_id, harness) = open_draft(
        Draft {
            to: &["Priya <priya@example.test>"],
            cc: &["design team"],
            subject: "Demo",
            body: "Ready to try.",
        },
        cx,
    );
    keys("secondary-enter", &mut w);
    assert!(window_open(&view, main));
    assert_eq!(
        problem(&compose, &mut w).as_deref(),
        Some(
            "“design team” in Cc isn't a valid email address. Correct it or remove it, then \
             send again."
        )
    );
    let cc = field(&compose, &mut w, |compose| &compose.cc);
    assert!(field_focused(&cc, &mut w));
    assert_eq!(outbox_len(&harness), 0);

    // An address still being typed is checked as well.
    w.simulate_input("a@b..org");
    w.run_until_parked();
    assert_eq!(
        problem(&compose, &mut w).as_deref(),
        Some(
            "These aren't valid email addresses: “design team” in Cc and “a@b..org” in Cc. \
             Correct or remove them, then send again."
        )
    );

    // Fixing them clears the banner, and the message goes.
    let input = cx_field_input(&cc, &mut w);
    w.update(|_, cx| input.update(cx, |input, cx| input.set_text("", cx)));
    w.run_until_parked();
    w.update(|_, cx| cc.update(cx, |field, cx| field.remove_chip(0, cx)));
    w.run_until_parked();
    assert_eq!(problem(&compose, &mut w), None);
    keys("secondary-enter", &mut w);
    assert!(!window_open(&view, main));
    assert_ne!(
        harness
            .store
            .draft_detail(draft_id)
            .expect("the draft loads")
            .expect("the draft exists")
            .state,
        DraftState::Editing
    );
}

fn cx_field_input(field: &Entity<RecipientField>, cx: &mut VisualTestContext) -> Entity<TextInput> {
    cx.read_entity(field, |field, _| field.input().clone())
}

#[gpui::test]
fn an_empty_subject_asks_first(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, _, harness) = open_draft(
        Draft {
            to: &["kai@example.test"],
            cc: &[],
            subject: "",
            body: "Lunch?",
        },
        cx,
    );
    keys("secondary-enter", &mut w);
    assert_eq!(check(&compose, &mut w), Some(SendCheck::NoSubject));
    assert!(w.debug_bounds("send-check-cancel").is_some());
    assert_eq!(outbox_len(&harness), 0);

    // Cancel (focused, so Escape) goes back to the subject.
    keys("escape", &mut w);
    assert_eq!(check(&compose, &mut w), None);
    assert!(window_open(&view, main), "Escape closed only the question");
    let subject = input(&compose, &mut w, |compose| &compose.subject);
    assert!(input_focused(&subject, &mut w));
    assert_eq!(outbox_len(&harness), 0);

    // Send Anyway sends it as it is.
    keys("secondary-enter", &mut w);
    assert_eq!(check(&compose, &mut w), Some(SendCheck::NoSubject));
    click("send-check-confirm", &mut w);
    assert!(!window_open(&view, main));
    assert_eq!(outbox_len(&harness), 1);
}

#[gpui::test]
fn a_mentioned_attachment_that_is_missing_asks_first(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, _, harness) = open_draft(
        Draft {
            to: &["kai@example.test"],
            cc: &[],
            subject: "",
            body: "The report is attached.\n\nOn Monday, Kai wrote:\n> Anything enclosed?",
        },
        cx,
    );
    // Both questions, one after the other.
    keys("secondary-enter", &mut w);
    assert_eq!(check(&compose, &mut w), Some(SendCheck::NoSubject));
    click("send-check-confirm", &mut w);
    assert_eq!(check(&compose, &mut w), Some(SendCheck::NoAttachment));
    assert!(window_open(&view, main));

    // Cancel goes back to the message, and the next Send asks again.
    click("send-check-cancel", &mut w);
    let body = input(&compose, &mut w, |compose| &compose.body);
    assert!(input_focused(&body, &mut w));
    keys("secondary-enter", &mut w);
    assert_eq!(check(&compose, &mut w), Some(SendCheck::NoSubject));
    click("send-check-confirm", &mut w);
    click("send-check-confirm", &mut w);
    assert!(!window_open(&view, main));
    assert_eq!(outbox_len(&harness), 1);
}

#[gpui::test]
fn a_draft_already_in_the_outbox_says_so(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, draft_id, harness) = open_draft(
        Draft {
            to: &["kai@example.test"],
            cc: &[],
            subject: "Plans",
            body: "Hello",
        },
        cx,
    );
    // Queued elsewhere (another window, the command line) while open.
    harness.store.queue_draft(draft_id).expect("it queues");
    w.update(|_, cx| compose.update(cx, |compose, cx| compose.refresh_from_store(cx)));
    keys("secondary-enter", &mut w);
    assert_eq!(problem(&compose, &mut w).as_deref(), Some(ALREADY_QUEUED));
    assert!(window_open(&view, main));
    assert_eq!(outbox_len(&harness), 1);
}

#[gpui::test]
fn closing_a_compose_window_says_the_draft_is_in_drafts(cx: &mut TestAppContext) {
    let (view, main, _compose, mut w, _, _harness) = open_draft(
        Draft {
            to: &["kai@example.test"],
            cc: &[],
            subject: "Plans",
            body: "Hello",
        },
        cx,
    );
    keys("escape", &mut w);
    main.run_until_parked();
    assert!(!window_open(&view, main));
    assert_eq!(toasts(&view, main), ["Draft saved to Drafts"]);
}

#[gpui::test]
fn the_close_button_says_the_draft_is_in_drafts_too(cx: &mut TestAppContext) {
    let (view, main, _compose, mut w, _, _harness) = open_draft(
        Draft {
            to: &[],
            cc: &[],
            subject: "Plans",
            body: "Hello",
        },
        cx,
    );
    assert!(w.simulate_close(), "the window agrees to close");
    w.update(|window, _| window.remove_window());
    main.run_until_parked();
    assert_eq!(toasts(&view, main), ["Draft saved to Drafts"]);
}

#[gpui::test]
fn reply_and_forward_need_an_open_conversation(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Account);
    cx.update(|window, cx| view.update(cx, |view, cx| view.forward_selected_thread(window, cx)));
    assert_eq!(
        cx.read_entity(&view, |view, _| view.store_error.clone()),
        Some("Open a conversation first.".to_owned())
    );
}
