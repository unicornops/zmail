//! The reader's banners (UI plan Phase 4): the remote-content notice and
//! the tiered sender-safety banners with "Looks safe" and "Show details".

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zmail_core::{AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, ThreadSnapshot};
use zmail_store::LocalStore;

use super::{Harness, Setup, click_on, drawn, open_workspace_with};
use crate::{InitialView, WorkspaceView};

/// A one-message thread from `sender` with an optional Reply-To.
fn thread(id: &str, sender: &str, reply_to: Option<&str>, remote_content: bool) -> ThreadSnapshot {
    let at = "2026-03-24T09:00:00Z".to_owned();
    ThreadSnapshot {
        remote_thread_id: id.to_owned(),
        subject: format!("Thread {id}"),
        participants: sender.to_owned(),
        preview: "Please confirm your account".to_owned(),
        is_flagged: false,
        has_attachments: false,
        unread_count: 1,
        latest_message_at: at.clone(),
        messages: vec![MessageSnapshot {
            remote_id: format!("message-{id}"),
            identity_email: "me@example.test".to_owned(),
            sender: sender.to_owned(),
            reply_to: reply_to.map(str::to_owned),
            recipients: "me@example.test".to_owned(),
            subject: format!("Thread {id}"),
            body_preview: "Please confirm your account".to_owned(),
            body_text: Some("Please confirm your account".to_owned()),
            sanitized_html: None,
            has_remote_content: remote_content,
            is_read: false,
            is_flagged: false,
            received_at: at,
            attachments: Vec::new(),
        }],
    }
}

fn seed(store: &LocalStore, thread: ThreadSnapshot) {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
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

/// The only Inbox thread and its message id.
fn only_thread(harness: &Harness) -> (i64, i64) {
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
    (thread_id, thread.messages[0].id)
}

fn open_thread(
    cx: &mut TestAppContext,
    thread: ThreadSnapshot,
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Harness) {
    open_workspace_with(
        cx,
        Setup::Account,
        |harness| InitialView::Thread(only_thread(harness).0),
        move |store| seed(store, thread),
    )
}

/// The id of the conversation the reader shows.
fn reader_thread(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<i64> {
    cx.read_entity(view, |view, cx| {
        view.reader
            .read(cx)
            .state()
            .thread
            .as_ref()
            .map(|thread| thread.id)
    })
}

#[gpui::test]
fn a_lookalike_sender_shows_a_danger_banner_until_looks_safe(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_thread(
        cx,
        thread(
            "lookalike",
            "PayPal <security@paypa1.com>",
            Some("evil@evil.example"),
            true,
        ),
    );
    let (thread_id, message_id) = only_thread(&harness);
    assert_eq!(reader_thread(&view, cx), Some(thread_id));

    assert!(drawn(format!("remote-content-{thread_id}"), cx));
    assert!(drawn(format!("sender-safety-{message_id}"), cx));
    // The evidence stays hidden until asked for.
    assert!(!drawn(format!("sender-safety-{message_id}-details"), cx));
    click_on(format!("sender-safety-{message_id}-toggle-details"), cx);
    assert!(drawn(format!("sender-safety-{message_id}-details"), cx));

    click_on(format!("sender-safety-dismiss-{message_id}"), cx);
    assert!(!drawn(format!("sender-safety-{message_id}"), cx));
    // Only the sender warning goes; the remote-content notice stays.
    assert!(drawn(format!("remote-content-{thread_id}"), cx));
    let thread = harness
        .store
        .thread_view(thread_id)
        .expect("the thread loads")
        .expect("the thread exists");
    assert!(thread.messages[0].sender_warning_dismissed);
}

#[gpui::test]
fn a_safe_sender_without_remote_content_shows_no_banners(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_thread(
        cx,
        thread("plain", "Kai Santos <kai@example.test>", None, false),
    );
    let (thread_id, message_id) = only_thread(&harness);
    assert_eq!(reader_thread(&view, cx), Some(thread_id));
    assert!(!drawn(format!("remote-content-{thread_id}"), cx));
    assert!(!drawn(format!("sender-safety-{message_id}"), cx));
}

#[gpui::test]
fn a_reply_to_on_another_domain_shows_a_caution_banner(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_thread(
        cx,
        thread(
            "reply-to",
            "Billing <billing@example.test>",
            Some("collect@elsewhere.example"),
            false,
        ),
    );
    let (thread_id, message_id) = only_thread(&harness);
    assert_eq!(reader_thread(&view, cx), Some(thread_id));
    assert!(drawn(format!("sender-safety-{message_id}"), cx));
}
