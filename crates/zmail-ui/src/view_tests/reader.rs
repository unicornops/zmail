//! The reader (UI plan Phase 4): the conversation list, collapsed and
//! expanded messages, folded quoted text, and n/p/Enter.

use std::time::Duration;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use zmail_core::{AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, ThreadSnapshot};
use zmail_store::LocalStore;

use super::{Setup, click, click_row, focused_pane, keys, open_workspace_with};
use crate::pane::Pane;
use crate::reader::ReaderView;
use crate::{InitialView, WorkspaceView};

fn message(sender: &str, at: &str, is_read: bool, body: &str) -> MessageSnapshot {
    MessageSnapshot {
        remote_id: format!("{sender}-{at}"),
        identity_email: "me@example.test".to_owned(),
        sender: sender.to_owned(),
        reply_to: None,
        recipients: "me@example.test".to_owned(),
        subject: "Q4 roadmap review".to_owned(),
        body_preview: body.chars().take(80).collect(),
        body_text: Some(body.to_owned()),
        sanitized_html: None,
        has_remote_content: false,
        is_read,
        is_flagged: false,
        received_at: at.to_owned(),
        attachments: Vec::new(),
    }
}

/// Replaces the account's mail with one conversation: two read messages
/// (the second ending in a quote) and an unread reply.
fn seed_conversation(store: &LocalStore) {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    let messages = vec![
        message(
            "Priya Raman <priya@example.test>",
            "2026-03-24T09:00:00Z",
            true,
            "Attached is the draft roadmap.\n\nPriya",
        ),
        message(
            "Marcus Chen <marcus@example.test>",
            "2026-03-24T10:00:00Z",
            true,
            "Looks good overall.\n\nMarcus\n\nOn Tue, Priya Raman wrote:\n> Attached is the draft roadmap.\n>\n> Priya",
        ),
        message(
            "Priya Raman <priya@example.test>",
            "2026-03-24T11:00:00Z",
            false,
            "Thanks, I moved offline search to November.",
        ),
    ];
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
                    threads: vec![ThreadSnapshot {
                        remote_thread_id: "roadmap".to_owned(),
                        subject: "Q4 roadmap review".to_owned(),
                        participants:
                            "Priya Raman <priya@example.test>, Marcus Chen <marcus@example.test>"
                                .to_owned(),
                        preview: "Thanks, I moved offline search to November.".to_owned(),
                        is_flagged: false,
                        has_attachments: false,
                        unread_count: 1,
                        latest_message_at: "2026-03-24T11:00:00Z".to_owned(),
                        messages,
                    }],
                    ..Default::default()
                }],
            },
        )
        .expect("the mail is stored");
}

/// Opens the window on the seeded conversation (as a click on it would,
/// so it is marked read after the delay).
fn open_conversation(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceView>,
    &mut VisualTestContext,
    Entity<ReaderView>,
) {
    let (view, cx, _harness) = open_workspace_with(
        cx,
        Setup::Account,
        |_| InitialView::Inbox,
        seed_conversation,
    );
    let reader = cx.read_entity(&view, |view, _| view.reader.clone());
    (view, cx, reader)
}

/// Which of the conversation's messages are expanded, in order.
fn expanded(reader: &Entity<ReaderView>, cx: &mut VisualTestContext) -> Vec<bool> {
    cx.read_entity(reader, |reader, _| {
        let thread = reader
            .state()
            .thread
            .as_ref()
            .expect("a conversation is open");
        thread
            .messages
            .iter()
            .map(|message| reader.conversation().is_expanded(message.id))
            .collect()
    })
}

fn current(reader: &Entity<ReaderView>, cx: &mut VisualTestContext) -> Option<usize> {
    cx.read_entity(reader, |reader, _| reader.conversation().current())
}

fn drawn(selector: &'static str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(selector).is_some()
}

#[gpui::test]
fn read_messages_collapse_and_the_latest_and_unread_expand(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);

    assert_eq!(expanded(&reader, cx), [false, false, true]);
    assert!(drawn("reader-subject", cx));
    assert!(drawn("reader-message-summary-0", cx));
    assert!(drawn("reader-message-summary-1", cx));
    assert!(drawn("reader-message-header-2", cx));
    assert!(drawn("reader-footer", cx), "the inline reply's slot");
    assert_eq!(
        current(&reader, cx),
        Some(2),
        "the unread message is current"
    );

    // Marking the conversation read (it was clicked open) does not
    // collapse what is open.
    click_row(0, Modifiers::none(), cx);
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let unread = cx.read_entity(&reader, |reader, _| {
        reader
            .state()
            .thread
            .as_ref()
            .map(|thread| thread.unread_count)
    });
    assert_eq!(unread, Some(0), "the conversation was marked read");
    assert_eq!(expanded(&reader, cx), [false, false, true]);
}

#[gpui::test]
fn clicking_a_summary_expands_it_and_its_header_collapses_it(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);

    click("reader-message-summary-0", cx);
    assert_eq!(expanded(&reader, cx), [true, false, true]);
    assert_eq!(current(&reader, cx), Some(0));
    assert!(drawn("reader-message-header-0", cx));

    click("reader-message-header-0", cx);
    assert_eq!(expanded(&reader, cx), [false, false, true]);
    assert!(drawn("reader-message-summary-0", cx));
}

#[gpui::test]
fn n_and_p_move_between_messages_and_enter_toggles(cx: &mut TestAppContext) {
    let (view, cx, reader) = open_conversation(cx);
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));

    keys("p", cx);
    assert_eq!(current(&reader, cx), Some(1));
    keys("enter", cx);
    assert_eq!(expanded(&reader, cx), [false, true, true]);
    keys("p p", cx);
    assert_eq!(
        current(&reader, cx),
        Some(0),
        "p stops at the first message"
    );
    keys("n n n", cx);
    assert_eq!(current(&reader, cx), Some(2), "n stops at the last message");
    keys("enter", cx);
    assert_eq!(expanded(&reader, cx), [false, true, false]);
}

#[gpui::test]
fn quoted_text_folds_behind_a_toggle(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);
    let marcus = cx.read_entity(&reader, |reader, _| {
        reader
            .state()
            .thread
            .as_ref()
            .expect("a conversation")
            .messages[1]
            .id
    });
    click("reader-message-summary-1", cx);

    assert!(drawn("reader-fold-1", cx), "the quoted reply folds");
    let unfolded = |cx: &mut VisualTestContext| {
        cx.read_entity(&reader, |r, _| r.conversation().is_unfolded(marcus))
    };
    assert!(!unfolded(cx));
    click("reader-fold-1", cx);
    assert!(unfolded(cx));
    assert_eq!(
        expanded(&reader, cx),
        [false, true, true],
        "the toggle does not collapse the message"
    );
}
