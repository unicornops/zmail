//! A folder longer than any page of rows: the open conversation stays the
//! one the user opened, however deep in the folder it is, across the
//! store refreshes a sync or an autosave causes.

use std::time::Duration;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zmail_core::{
    AccountSyncSnapshot, MailboxRole, MailboxSnapshot, MessageSnapshot, ThreadSnapshot,
    VirtualMailbox,
};
use zmail_store::LocalStore;

use super::{Harness, Setup, keys, open_workspace_with, search_input, selected_thread};
use crate::{InitialView, ListMode, WorkspaceView};

/// More conversations than the shell ever reads in one page.
const THREADS: usize = 260;

/// The word only the oldest conversation's subject has.
const OLDEST_WORD: &str = "albatross";

/// Conversation `index` of the large Inbox: 0 is the newest.
fn thread(index: usize) -> ThreadSnapshot {
    let subject = if index == THREADS - 1 {
        format!("Weekly digest {index} {OLDEST_WORD}")
    } else {
        format!("Weekly digest {index}")
    };
    // A minute apart, newest first.
    let minutes = 24 * 60 - 1 - index;
    let at = format!("2026-03-20T{:02}:{:02}:00Z", minutes / 60, minutes % 60);
    ThreadSnapshot {
        remote_thread_id: format!("digest-{index}"),
        subject: subject.clone(),
        participants: "Kai Santos".to_owned(),
        preview: "This week".to_owned(),
        is_flagged: false,
        has_attachments: false,
        unread_count: 1,
        latest_message_at: at.clone(),
        messages: vec![MessageSnapshot {
            remote_id: format!("digest-message-{index}"),
            identity_email: "me@example.test".to_owned(),
            sender: "Kai Santos <kai@example.test>".to_owned(),
            reply_to: None,
            recipients: "me@example.test".to_owned(),
            subject,
            body_preview: "This week".to_owned(),
            body_text: Some("This week".to_owned()),
            sanitized_html: None,
            raw_html: None,
            has_remote_content: false,
            is_read: false,
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

/// Replaces the account's mail with an Inbox of [`THREADS`] unread
/// conversations, and an empty Archive and Trash.
fn seed_large_inbox(store: &LocalStore) {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    let mailbox = |name: &str, role: &str, threads| MailboxSnapshot {
        name: name.to_owned(),
        role: Some(role.to_owned()),
        threads,
        ..Default::default()
    };
    store
        .replace_account_snapshot(
            account_id,
            &AccountSyncSnapshot {
                checkpoint: "test".to_owned(),
                detail: "test".to_owned(),
                identities: None,
                mailboxes: vec![
                    mailbox("Inbox", "inbox", (0..THREADS).map(thread).collect()),
                    mailbox("Archive", "archive", Vec::new()),
                    mailbox("Trash", "trash", Vec::new()),
                ],
            },
        )
        .expect("the mail is stored");
}

/// The large Inbox's conversations, newest first.
fn inbox_threads(store: &LocalStore) -> Vec<i64> {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    let inbox = store
        .mailbox_id_by_role(account_id, "inbox")
        .expect("the lookup works")
        .expect("an inbox");
    store
        .list_threads(inbox, 0..THREADS)
        .expect("the threads load")
        .into_iter()
        .map(|thread| thread.id)
        .collect()
}

/// The window on the large Inbox, the harness that keeps it alive, and
/// the Inbox's conversations, newest first.
fn open_large_inbox(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceView>,
    &mut VisualTestContext,
    Harness,
    Vec<i64>,
) {
    let (view, cx, harness) =
        open_workspace_with(cx, Setup::Account, |_| InitialView::Inbox, seed_large_inbox);
    let threads = inbox_threads(&harness.store);
    assert_eq!(threads.len(), THREADS);
    (view, cx, harness, threads)
}

/// Re-reads the store, as a sync tick or an autosave does.
fn refresh(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    view.update(cx, |view, cx| {
        view.refresh_from_store(cx);
        cx.notify();
    });
    cx.run_until_parked();
}

fn unread(view: &Entity<WorkspaceView>, thread_id: i64, cx: &mut VisualTestContext) -> usize {
    cx.read_entity(view, |view, _| {
        view.store
            .thread_view(thread_id)
            .expect("the thread loads")
            .expect("the thread exists")
            .unread_count
    })
}

#[gpui::test]
fn a_conversation_deep_in_a_large_folder_stays_open_across_refreshes(cx: &mut TestAppContext) {
    let (view, cx, _harness, threads) = open_large_inbox(cx);
    let (newest, oldest) = (threads[0], threads[THREADS - 1]);
    assert_eq!(selected_thread(&view, cx), Some(newest));

    keys("end", cx);
    assert_eq!(selected_thread(&view, cx), Some(oldest));
    refresh(&view, cx);
    assert_eq!(
        selected_thread(&view, cx),
        Some(oldest),
        "a refresh keeps the conversation the user opened"
    );

    // The sync ticks and the mark-read delay pass: the opened conversation
    // is the one marked read, never the newest.
    cx.executor().advance_clock(Duration::from_secs(12));
    cx.run_until_parked();
    assert_eq!(selected_thread(&view, cx), Some(oldest));
    assert_eq!(unread(&view, oldest, cx), 0);
    assert_eq!(unread(&view, newest, cx), 1, "the newest stays unread");
}

#[gpui::test]
fn a_deep_selection_keeps_its_cursor_across_refreshes(cx: &mut TestAppContext) {
    let (view, cx, _harness, threads) = open_large_inbox(cx);
    keys("end", cx);
    keys("shift-up", cx);
    let selection = |cx: &mut VisualTestContext| {
        let mut ids = cx.read_entity(&view, |view, _| view.list_selection.thread_ids.clone());
        ids.sort_unstable();
        ids
    };
    let mut expected = vec![threads[THREADS - 1], threads[THREADS - 2]];
    expected.sort_unstable();
    assert_eq!(selection(cx), expected);
    let cursor = selected_thread(&view, cx);
    assert_eq!(cursor, Some(threads[THREADS - 2]));

    refresh(&view, cx);
    assert_eq!(selection(cx), expected);
    assert_eq!(selected_thread(&view, cx), cursor);
}

#[gpui::test]
fn a_deep_conversation_in_the_unified_inbox_stays_open(cx: &mut TestAppContext) {
    let (view, cx, _harness, threads) = open_large_inbox(cx);
    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Unified(MailboxRole::Inbox), cx)
    });
    cx.run_until_parked();
    assert_eq!(selected_thread(&view, cx), Some(threads[0]));

    keys("end", cx);
    let oldest = threads[THREADS - 1];
    assert_eq!(selected_thread(&view, cx), Some(oldest));
    refresh(&view, cx);
    assert_eq!(selected_thread(&view, cx), Some(oldest));
    cx.executor().advance_clock(Duration::from_secs(12));
    cx.run_until_parked();
    assert_eq!(selected_thread(&view, cx), Some(oldest));
    assert_eq!(unread(&view, threads[0], cx), 1, "the newest stays unread");
}

#[gpui::test]
fn an_old_search_hit_stays_open_across_refreshes(cx: &mut TestAppContext) {
    let (view, cx, _harness, threads) = open_large_inbox(cx);
    let search = search_input(&view, cx);
    super::focus(&search, cx);
    cx.simulate_input(OLDEST_WORD);
    keys("enter", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.list_mode),
        ListMode::Search
    );
    // Into the results, onto the one hit.
    keys("down", cx);
    let oldest = threads[THREADS - 1];
    assert_eq!(selected_thread(&view, cx), Some(oldest));

    refresh(&view, cx);
    assert_eq!(selected_thread(&view, cx), Some(oldest));
    cx.executor().advance_clock(Duration::from_secs(12));
    cx.run_until_parked();
    assert_eq!(selected_thread(&view, cx), Some(oldest));
    assert_eq!(unread(&view, threads[0], cx), 1, "the newest stays unread");
}
