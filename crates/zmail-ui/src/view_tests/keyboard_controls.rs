//! The controls inside the panes, by keyboard alone (accessibility audit
//! G7): Tab walks the buttons of the focused pane and stays in it, F6 and
//! Ctrl+Tab move between panes, Enter or Space presses the focused button,
//! and Escape goes back to the message list.
//!
//! A test finds a control's place in the tab order by pressing the mouse
//! on it (which focuses it) and releasing away from it (so it does not
//! click), then starts again from the pane and presses Tab until that
//! control has focus.

use gpui::{
    AppContext as _, Entity, FocusHandle, Focusable, Modifiers, MouseButton, TestAppContext,
    VisualTestContext, point, px,
};
use zmail_core::{
    AccountSyncSnapshot, MailboxSnapshot, MessageAttachment, MessageSnapshot, OutboxJobState,
    ThreadSnapshot,
};
use zmail_store::LocalStore;

use super::{
    Harness, Setup, drawn, focused_pane, keys, open_workspace, open_workspace_with,
    queue_test_message, resize,
};
use crate::pane::Pane;
use crate::{InitialView, WorkspaceView};

/// Most Tab presses any of these tests needs to reach a control.
const MAX_TABS: usize = 60;

/// The focus handle the control drawn at `selector` takes. Focus goes
/// back where it was.
fn control_focus(selector: &str, cx: &mut VisualTestContext) -> FocusHandle {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    let before = cx.update(|window, cx| window.focused(cx));
    cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(
        point(px(-100.), px(-100.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();
    let control = cx
        .update(|window, cx| window.focused(cx))
        .unwrap_or_else(|| panic!("{selector} takes focus"));
    if let Some(before) = before {
        cx.update(|window, cx| window.focus(&before, cx));
        cx.run_until_parked();
    }
    control
}

/// Presses `key` (Enter or Space) on the focused control: down and up,
/// as a keyboard does; a button clicks on the key's release.
fn press(key: &str, cx: &mut VisualTestContext) {
    crate::components::test_support::press(cx, key);
    cx.run_until_parked();
}

/// Moves focus to `pane` itself.
fn focus_pane(view: &Entity<WorkspaceView>, pane: Pane, cx: &mut VisualTestContext) {
    view.update_in(cx, |view, window, cx| view.focus_pane(pane, window, cx));
    cx.run_until_parked();
}

/// From `pane`, presses Tab until the control at `selector` has focus,
/// checking that focus never leaves the pane; returns how many presses
/// it took.
fn tab_to(
    view: &Entity<WorkspaceView>,
    pane: Pane,
    selector: &str,
    cx: &mut VisualTestContext,
) -> usize {
    focus_pane(view, pane, cx);
    let control = control_focus(selector, cx);
    for presses in 1..=MAX_TABS {
        keys("tab", cx);
        assert_eq!(
            focused_pane(view, cx),
            Some(pane),
            "Tab stays in the pane on the way to {selector}"
        );
        if cx.update(|window, _| control.is_focused(window)) {
            return presses;
        }
    }
    panic!("Tab never reached {selector}");
}

/// Presses `key` (Tab or Shift+Tab) from `pane` until focus is back on
/// the pane itself, checking it never leaves the pane; returns how many
/// presses that took.
fn tab_round(
    view: &Entity<WorkspaceView>,
    pane: Pane,
    key: &str,
    cx: &mut VisualTestContext,
) -> usize {
    assert!(pane_itself_focused(view, pane, cx));
    for presses in 1..=MAX_TABS {
        keys(key, cx);
        assert_eq!(
            focused_pane(view, cx),
            Some(pane),
            "{key} stays in {pane:?}"
        );
        if pane_itself_focused(view, pane, cx) {
            return presses;
        }
    }
    panic!("{key} never came back to {pane:?}");
}

/// Whether `pane` itself has focus (not a control in it).
fn pane_itself_focused(
    view: &Entity<WorkspaceView>,
    pane: Pane,
    cx: &mut VisualTestContext,
) -> bool {
    cx.update(|window, cx| {
        let view = view.read(cx);
        let handle = match pane {
            Pane::Sidebar => view.sidebar.focus_handle(cx),
            Pane::MessageList => view.message_list.focus_handle(cx),
            Pane::Detail => view.reader.focus_handle(cx),
        };
        handle.is_focused(window)
    })
}

// ---------------------------------------------------------------------------
// Tab, F6 and Escape
// ---------------------------------------------------------------------------

#[gpui::test]
fn tab_walks_a_pane_and_f6_moves_between_panes(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // F6 and Ctrl+Tab move between the panes, both ways.
    keys("f6", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    keys("f6", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));
    keys("shift-f6", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    keys("ctrl-tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));
    keys("ctrl-shift-tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));

    // In each pane, Tab goes through its buttons and comes back round to
    // the pane itself without leaving it; Shift+Tab goes the other way.
    for pane in Pane::ALL {
        focus_pane(&view, pane, cx);
        let stops = tab_round(&view, pane, "tab", cx);
        assert_eq!(
            tab_round(&view, pane, "shift-tab", cx),
            stops,
            "{pane:?}: Shift+Tab visits as many stops"
        );
        if pane == Pane::Detail {
            assert!(stops > 1, "the reader has buttons to reach");
        }
    }

    // Escape from a button in the reader, or from the reader, goes back to
    // the list.
    focus_pane(&view, Pane::Detail, cx);
    keys("tab", cx);
    assert!(!pane_itself_focused(&view, Pane::Detail, cx));
    keys("escape", cx);
    assert!(pane_itself_focused(&view, Pane::MessageList, cx));
    keys("f6 escape", cx);
    assert!(pane_itself_focused(&view, Pane::MessageList, cx));
    // And from the sidebar.
    focus_pane(&view, Pane::Sidebar, cx);
    keys("escape", cx);
    assert!(pane_itself_focused(&view, Pane::MessageList, cx));
}

// ---------------------------------------------------------------------------
// The reader's banners and header
// ---------------------------------------------------------------------------

/// A message from `sender` whose replies go to a subdomain (an Info
/// sender warning with details) and that references remote content.
fn banner_thread() -> ThreadSnapshot {
    let at = "2026-03-24T09:00:00Z".to_owned();
    let sender = "Kai Santos <kai@example.test>";
    ThreadSnapshot {
        remote_thread_id: "banners".to_owned(),
        subject: "Newsletter".to_owned(),
        participants: sender.to_owned(),
        preview: "This week".to_owned(),
        is_flagged: false,
        has_attachments: true,
        unread_count: 1,
        latest_message_at: at.clone(),
        messages: vec![MessageSnapshot {
            remote_id: "banners-1".to_owned(),
            identity_email: "me@example.test".to_owned(),
            sender: sender.to_owned(),
            reply_to: Some("replies@news.example.test".to_owned()),
            recipients: "me@example.test".to_owned(),
            subject: "Newsletter".to_owned(),
            body_preview: "This week".to_owned(),
            body_text: Some("This week".to_owned()),
            sanitized_html: None,
            raw_html: None,
            has_remote_content: true,
            is_read: true,
            is_flagged: false,
            received_at: at,
            attachments: Vec::new(),
            headers: zmail_core::MessageHeaders {
                captured: true,
                ..Default::default()
            },
        }],
    }
}

/// Stores `thread` as the account's only Inbox thread, with the
/// attachment `notes.txt` when `attachment` is set.
fn seed(store: &LocalStore, mut thread: ThreadSnapshot, attachment: bool) {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    if attachment {
        let target = store
            .sync_target(account_id)
            .expect("the target loads")
            .expect("the account exists");
        let storage_path = target
            .blobs
            .write("blob-notes", b"plain notes")
            .expect("the blob is written");
        thread.messages[0].attachments = vec![MessageAttachment {
            filename: "notes.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            size_bytes: 11,
            storage_path,
            content_id: None,
            inline_only: false,
        }];
    }
    store
        .replace_account_snapshot(
            account_id,
            &AccountSyncSnapshot {
                checkpoint: "test".to_owned(),
                detail: "test".to_owned(),
                identities: None,
                mailboxes: vec![MailboxSnapshot {
                    name: "Inbox".to_owned(),
                    role: Some("inbox".to_owned()),
                    threads: vec![thread],
                    ..Default::default()
                }],
            },
        )
        .expect("the mail is stored");
}

/// The only Inbox thread, its first message and that message's first
/// attachment, if any.
fn only_thread(harness: &Harness) -> (i64, i64, Option<i64>) {
    let store = &harness.store;
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    let inbox = store
        .mailbox_id_by_role(account_id, "inbox")
        .expect("the lookup works")
        .expect("an inbox");
    let thread_id = store.list_threads(inbox, 0..10).expect("the threads load")[0].id;
    let thread = store
        .thread_view(thread_id)
        .expect("the thread loads")
        .expect("the thread exists");
    let message = &thread.messages[0];
    (
        thread_id,
        message.id,
        message.attachments.first().map(|attachment| attachment.id),
    )
}

fn open_banners(
    cx: &mut TestAppContext,
    attachment: bool,
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Harness) {
    open_workspace_with(
        cx,
        Setup::Account,
        |harness| InitialView::Thread(only_thread(harness).0),
        move |store| seed(store, banner_thread(), attachment),
    )
}

fn sender_allowed(harness: &Harness) -> bool {
    let account_id = harness.store.list_accounts().expect("the accounts load")[0].id;
    harness
        .store
        .remote_content_allowed_for_sender(account_id, "kai@example.test")
        .expect("the choice loads")
}

fn remote_loaded(
    view: &Entity<WorkspaceView>,
    message_id: i64,
    cx: &mut VisualTestContext,
) -> bool {
    cx.read_entity(view, |view, cx| {
        view.reader
            .read(cx)
            .state()
            .remote_content_messages
            .contains(&message_id)
    })
}

#[gpui::test]
fn the_reader_banners_work_from_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_banners(cx, false);
    let (thread_id, message_id, _) = only_thread(&harness);

    // Show details, with Space.
    let details = format!("sender-safety-{message_id}-details");
    assert!(!drawn(details.clone(), cx));
    tab_to(
        &view,
        Pane::Detail,
        &format!("sender-safety-{message_id}-toggle-details"),
        cx,
    );
    press("space", cx);
    assert!(drawn(details, cx), "Space showed the details");

    // Load remote content, with Enter: the message does not collapse.
    tab_to(
        &view,
        Pane::Detail,
        &format!("remote-content-load-{message_id}"),
        cx,
    );
    press("enter", cx);
    assert!(remote_loaded(&view, message_id, cx));
    assert!(
        drawn(format!("remote-content-always-{message_id}"), cx),
        "Enter pressed the button rather than collapsing the message"
    );
    // The button went; focus stayed in the reader.
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));

    // Always for this sender, then Block.
    tab_to(
        &view,
        Pane::Detail,
        &format!("remote-content-always-{message_id}"),
        cx,
    );
    press("enter", cx);
    assert!(sender_allowed(&harness));
    tab_to(
        &view,
        Pane::Detail,
        &format!("remote-content-block-{message_id}"),
        cx,
    );
    press("space", cx);
    assert!(!sender_allowed(&harness));

    // Looks safe.
    tab_to(
        &view,
        Pane::Detail,
        &format!("sender-safety-dismiss-{message_id}"),
        cx,
    );
    press("enter", cx);
    let thread = harness
        .store
        .thread_view(thread_id)
        .expect("the thread loads")
        .expect("the thread exists");
    assert!(thread.messages[0].sender_warning_dismissed);

    // Escape from the reader goes back to the list.
    keys("escape", cx);
    assert!(pane_itself_focused(&view, Pane::MessageList, cx));
}

#[gpui::test]
fn the_message_header_and_more_menu_work_from_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_banners(cx, false);
    let (_, message_id, _) = only_thread(&harness);
    let details_open = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, cx| {
            view.reader.read(cx).details_shown(message_id)
        })
    };

    // The recipient summary shows and hides the From, To and Date lines.
    assert!(!details_open(cx));
    tab_to(&view, Pane::Detail, "reader-recipients-0", cx);
    press("enter", cx);
    assert!(details_open(cx));

    // The message's More menu opens from the keyboard, and Escape closes
    // it.
    tab_to(&view, Pane::Detail, "reader-message-more-0", cx);
    press("enter", cx);
    let menu_open = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, cx| {
            view.reader.read(cx).popover_anchor().is_some()
        })
    };
    assert!(menu_open(cx));
    keys("escape", cx);
    assert!(!menu_open(cx));
}

#[gpui::test]
fn attachment_buttons_work_from_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_banners(cx, true);
    let (_, _, attachment) = only_thread(&harness);
    let attachment = attachment.expect("the attachment is seeded");

    tab_to(
        &view,
        Pane::Detail,
        &format!("attachment-open-{attachment}"),
        cx,
    );
    press("enter", cx);
    let opened = cx.read_entity(&view, |view, _| view.attachments.opened.clone());
    assert_eq!(opened.len(), 1, "Enter opened the attachment");

    tab_to(
        &view,
        Pane::Detail,
        &format!("attachment-save-{attachment}"),
        cx,
    );
    press("space", cx);
    assert!(cx.did_prompt_for_new_path(), "Space asked where to save it");
}

// ---------------------------------------------------------------------------
// The sidebar and the Outbox
// ---------------------------------------------------------------------------

#[gpui::test]
fn the_sidebar_fix_button_works_from_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Accounts,
        |_| InitialView::Inbox,
        |store| {
            let home = store.list_accounts().expect("accounts")[1].id;
            store
                .record_sync_failure(home, "Sync failed", "authentication failed")
                .expect("the failure is recorded");
        },
    );
    resize(1440., cx);
    let home = harness.home_account_id.expect("Home");

    tab_to(&view, Pane::Sidebar, &format!("account-fix-{home}"), cx);
    press("enter", cx);
    let (root, window_cx) = super::settings::settings_window(cx);
    assert_eq!(
        root.read_with(&window_cx, |root, cx| root
            .accounts()
            .read(cx)
            .focused_account()),
        Some(home),
        "Enter opened Settings on the account"
    );
}

/// Shows the Outbox with `job` failed for `error`.
fn show_failed_job(
    view: &Entity<WorkspaceView>,
    harness: &Harness,
    error: String,
    cx: &mut VisualTestContext,
) -> i64 {
    let job = queue_test_message(&harness.store, "Stuck");
    harness
        .store
        .record_outbox_failure(job, error)
        .expect("the failure is recorded");
    cx.dispatch_action(crate::actions::ShowOutbox);
    view.update(cx, |view, cx| {
        view.refresh_from_store(cx);
        cx.notify();
    });
    cx.run_until_parked();
    job
}

fn job_state(harness: &Harness, job: i64) -> OutboxJobState {
    harness
        .store
        .list_outbox_jobs(None)
        .expect("the jobs load")
        .into_iter()
        .find(|summary| summary.id == job)
        .expect("the job is kept")
        .state
}

#[gpui::test]
fn outbox_retry_now_works_from_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let job = show_failed_job(&view, &harness, "550 mailbox unavailable".to_owned(), cx);
    assert_eq!(job_state(&harness, job), OutboxJobState::Failed);

    tab_to(
        &view,
        Pane::MessageList,
        &format!("outbox-row-{job}-retry"),
        cx,
    );
    press("enter", cx);
    assert_ne!(
        job_state(&harness, job),
        OutboxJobState::Failed,
        "Enter retried it"
    );
}

#[gpui::test]
fn outbox_trust_new_key_works_from_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let change = zmail_core::key_change::PinnedKeyChanged::new(
        "kai@example.test",
        ["0123456789ABCDEF0123456789ABCDEF01234567".to_owned()],
        ["89ABCDEF0123456789ABCDEF0123456789ABCDEF".to_owned()],
    );
    let job = show_failed_job(&view, &harness, change.to_string(), cx);

    tab_to(
        &view,
        Pane::MessageList,
        &format!("outbox-row-{job}-trust-key"),
        cx,
    );
    press("enter", cx);
    assert!(
        drawn("key-change-trust".to_owned(), cx),
        "the dialog opened"
    );
}

#[gpui::test]
fn outbox_re_authenticate_works_from_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let refused = zmail_core::reauth::ReauthRequired::new(
        zmail_core::reauth::ReauthReason::SendRefused,
        "535 5.7.3 Authentication unsuccessful",
    );
    let job = show_failed_job(&view, &harness, refused.to_string(), cx);

    tab_to(
        &view,
        Pane::MessageList,
        &format!("outbox-row-{job}-reauth"),
        cx,
    );
    press("space", cx);
    let (root, window_cx) = super::settings::settings_window(cx);
    let accounts = root.read_with(&window_cx, |root, _| root.accounts().clone());
    assert!(accounts.read_with(&window_cx, |accounts, _| accounts.reauth_open()));
}
