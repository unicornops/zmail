//! Message-list rows (UI plan Phase 3): the comfortable and compact
//! densities and what a row shows.

use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext};
use zmail_core::{
    AccountSyncSnapshot, MailboxRole, MailboxSnapshot, MessageSnapshot, ThreadFilter,
    ThreadSnapshot, VirtualMailbox, preferences::Density,
};
use zmail_store::LocalStore;

use super::{
    Setup, click, focused_pane, keys, open_workspace, open_workspace_with, selected_thread,
};
use crate::InitialView;
use crate::actions::{ToggleDensity, Undo};
use crate::pane::Pane;
use crate::styles::DensityTokens;

fn message(sender: &str, at: &str, is_read: bool) -> MessageSnapshot {
    MessageSnapshot {
        remote_id: format!("{sender}-{at}"),
        identity_email: "me@example.test".to_owned(),
        sender: sender.to_owned(),
        reply_to: None,
        recipients: "me@example.test".to_owned(),
        subject: "Q4 roadmap review".to_owned(),
        body_preview: "Moved offline search to November".to_owned(),
        body_text: None,
        sanitized_html: None,
        has_remote_content: false,
        is_read,
        is_flagged: false,
        received_at: at.to_owned(),
        attachments: Vec::new(),
    }
}

/// Replaces the account's mail with a read single-message thread and a
/// newer, unread, flagged three-message thread with an attachment.
fn seed_inbox(store: &LocalStore) {
    let account_id = store
        .list_accounts()
        .expect("the accounts load")
        .first()
        .expect("an account")
        .id;
    let roadmap = vec![
        message(
            "Priya Raman <priya@example.test>",
            "2026-03-24T09:00:00Z",
            true,
        ),
        message(
            "Marcus Chen <marcus@example.test>",
            "2026-03-24T10:00:00Z",
            true,
        ),
        message(
            "Priya Raman <priya@example.test>",
            "2026-03-24T11:00:00Z",
            false,
        ),
    ];
    let threads = vec![
        ThreadSnapshot {
            remote_thread_id: "roadmap".to_owned(),
            subject: "Q4 roadmap review".to_owned(),
            participants: "Priya Raman <priya@example.test>, Marcus Chen <marcus@example.test>"
                .to_owned(),
            preview: "Moved offline search to November".to_owned(),
            is_flagged: true,
            has_attachments: true,
            unread_count: 1,
            latest_message_at: "2026-03-24T11:00:00Z".to_owned(),
            messages: roadmap,
        },
        ThreadSnapshot {
            remote_thread_id: "invoice".to_owned(),
            subject: "Invoice for September".to_owned(),
            participants: "Billing <billing@example.test>".to_owned(),
            preview: "Your invoice is attached".to_owned(),
            is_flagged: false,
            has_attachments: false,
            unread_count: 0,
            latest_message_at: "2026-03-23T08:00:00Z".to_owned(),
            messages: vec![message(
                "Billing <billing@example.test>",
                "2026-03-23T08:00:00Z",
                true,
            )],
        },
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
                    delimiter: None,
                    parent: None,
                    threads,
                }],
            },
        )
        .expect("the mail is stored");
}

/// The Inbox thread ids, newest first.
fn inbox_threads(store: &LocalStore) -> Vec<i64> {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    let inbox = store
        .mailbox_id_by_role(account_id, "inbox")
        .expect("the lookup works")
        .expect("an inbox");
    store
        .list_threads(inbox, 0..10)
        .expect("the threads load")
        .into_iter()
        .map(|thread| thread.id)
        .collect()
}

// `debug_bounds` takes `&'static str`; these per-thread selectors are
// built at run time, so they are leaked (a few bytes per test).

fn drawn(selector: String, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(Box::leak(selector.into_boxed_str()))
        .is_some()
}

fn row_height(index: usize, cx: &mut VisualTestContext) -> f32 {
    let selector = Box::leak(format!("thread-row-{index}").into_boxed_str());
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    f32::from(bounds.size.height)
}

#[gpui::test]
fn comfortable_rows_show_the_avatar_sender_and_indicators(cx: &mut TestAppContext) {
    let (_view, cx, harness) =
        open_workspace_with(cx, Setup::Account, |_| InitialView::Inbox, seed_inbox);
    let threads = inbox_threads(&harness.store);
    let (roadmap, invoice) = (threads[0], threads[1]);
    let part = |thread: i64, part: &str| format!("thread-row-{thread}-{part}");

    assert_eq!(
        row_height(0, cx),
        DensityTokens::COMFORTABLE.message_row_height
    );
    for thread in [roadmap, invoice] {
        assert!(drawn(part(thread, "avatar"), cx), "every row has an avatar");
        assert!(drawn(part(thread, "date"), cx), "and a date");
    }
    // The unread, flagged three-message thread with an attachment.
    for indicator in ["unread", "count", "flag", "attachment"] {
        assert!(drawn(part(roadmap, indicator), cx), "{indicator} is shown");
    }
    // The read single message shows none of them.
    for indicator in ["unread", "count", "flag", "attachment"] {
        assert!(
            !drawn(part(invoice, indicator), cx),
            "{indicator} is hidden"
        );
    }
}

#[gpui::test]
fn the_density_toggle_switches_rows_and_is_saved(cx: &mut TestAppContext) {
    let (_view, cx, harness) =
        open_workspace_with(cx, Setup::Account, |_| InitialView::Inbox, seed_inbox);
    let roadmap = inbox_threads(&harness.store)[0];
    let saved = || {
        harness
            .store
            .ui_preferences()
            .expect("the preferences load")
            .density
    };

    cx.dispatch_action(ToggleDensity);
    cx.run_until_parked();
    assert_eq!(saved(), Density::Compact);
    assert_eq!(row_height(0, cx), DensityTokens::COMPACT.message_row_height);
    assert!(
        !drawn(format!("thread-row-{roadmap}-avatar"), cx),
        "compact rows have no avatar"
    );
    for indicator in ["unread", "count", "flag", "attachment", "date"] {
        assert!(
            drawn(format!("thread-row-{roadmap}-{indicator}"), cx),
            "compact rows keep the {indicator}"
        );
    }

    cx.dispatch_action(ToggleDensity);
    cx.run_until_parked();
    assert_eq!(saved(), Density::Comfortable);
    assert_eq!(
        row_height(0, cx),
        DensityTokens::COMFORTABLE.message_row_height
    );
}

#[gpui::test]
fn a_saved_compact_density_applies_at_startup(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace_with(
        cx,
        Setup::Account,
        |_| InitialView::Inbox,
        |store| {
            seed_inbox(store);
            store
                .update_ui_preferences(|preferences| preferences.density = Density::Compact)
                .expect("the preferences save");
        },
    );
    assert_eq!(row_height(0, cx), DensityTokens::COMPACT.message_row_height);
}

#[gpui::test]
fn cross_account_rows_name_their_account(cx: &mut TestAppContext) {
    let (view, cx, harness) = super::open_workspace(cx, Setup::Accounts);
    let work_thread = harness.threads[0];
    let chip = |thread: i64| format!("thread-row-{thread}-account");

    assert!(
        !drawn(chip(work_thread), cx),
        "one account's mailbox has no account chips"
    );
    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Unified(MailboxRole::Inbox), cx)
    });
    cx.run_until_parked();
    assert!(drawn(chip(work_thread), cx));
    assert!(drawn(chip(harness.home_threads[0]), cx));
}

// ---------------------------------------------------------------------------
// Header, filters and quick actions
// ---------------------------------------------------------------------------

fn list_filter(
    view: &gpui::Entity<crate::WorkspaceView>,
    cx: &mut VisualTestContext,
) -> ThreadFilter {
    cx.read_entity(view, |view, cx| view.message_list.read(cx).filter())
}

/// Picks the filter `steps` entries below the current one in the menu.
fn pick_filter(steps: usize, cx: &mut VisualTestContext) {
    click("list-filter", cx);
    keys(&format!("{}enter", "down ".repeat(steps)), cx);
}

#[gpui::test]
fn the_header_filters_rows_in_the_store(cx: &mut TestAppContext) {
    let (view, cx, harness) =
        open_workspace_with(cx, Setup::Account, |_| InitialView::Inbox, seed_inbox);
    let roadmap = inbox_threads(&harness.store)[0];
    assert!(cx.debug_bounds("list-unread-count").is_some());
    assert!(cx.debug_bounds("thread-row-1").is_some());

    // All -> Unread: only the unread roadmap thread is left, and focus is
    // back on the rows.
    pick_filter(1, cx);
    assert_eq!(list_filter(&view, cx), ThreadFilter::Unread);
    assert!(cx.debug_bounds("thread-row-0").is_some());
    assert!(cx.debug_bounds("thread-row-1").is_none());
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    // Unread -> Flagged -> Has attachments: the same thread each time.
    pick_filter(1, cx);
    assert_eq!(list_filter(&view, cx), ThreadFilter::Flagged);
    assert!(cx.debug_bounds("thread-row-1").is_none());
    pick_filter(1, cx);
    assert_eq!(list_filter(&view, cx), ThreadFilter::HasAttachments);
    assert!(cx.debug_bounds("thread-row-0").is_some());
    assert!(cx.debug_bounds("thread-row-1").is_none());

    // Under Unread, reading the last unread thread empties the list; the
    // empty state's one action shows everything again.
    click("list-filter", cx);
    keys("up up enter", cx);
    assert_eq!(list_filter(&view, cx), ThreadFilter::Unread);
    harness
        .store
        .set_thread_read(roadmap, true)
        .expect("the thread is marked read");
    view.update(cx, |view, cx| view.refresh_from_store(cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-row-0").is_none());
    click("list-filter-show-all", cx);
    assert_eq!(list_filter(&view, cx), ThreadFilter::All);
    assert!(cx.debug_bounds("thread-row-1").is_some());

    // Another mailbox starts unfiltered.
    pick_filter(2, cx);
    assert_eq!(list_filter(&view, cx), ThreadFilter::Flagged);
    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Unified(MailboxRole::Inbox), cx)
    });
    cx.run_until_parked();
    assert_eq!(list_filter(&view, cx), ThreadFilter::All);
}

fn hover(selector: &'static str, cx: &mut VisualTestContext) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn hover_quick_actions_replace_the_date_and_go_through_mail_actions(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let (first, second) = (harness.threads[0], harness.threads[1]);
    let part = |thread: i64, part: &str| format!("thread-row-{thread}-{part}");
    assert_eq!(selected_thread(&view, cx), Some(first));
    assert!(drawn(part(second, "date"), cx));
    assert!(!drawn(part(second, "actions"), cx));

    hover("thread-row-1", cx);
    assert!(
        !drawn(part(second, "date"), cx),
        "the actions replace the date"
    );
    for action in ["quick-archive", "quick-delete", "quick-flag", "quick-read"] {
        assert!(drawn(part(second, action), cx), "{action} is shown");
    }
    assert!(drawn(part(first, "date"), cx), "other rows keep their date");

    // Flag the hovered (not the selected) thread.
    click(Box::leak(part(second, "quick-flag").into_boxed_str()), cx);
    let thread = |id| {
        harness
            .store
            .thread_view(id)
            .expect("the thread loads")
            .expect("the thread exists")
    };
    assert!(thread(second).is_flagged);
    assert_eq!(
        selected_thread(&view, cx),
        Some(first),
        "the click did not open it"
    );
    // Mark it read, then undo that like any other change.
    click(Box::leak(part(second, "quick-read").into_boxed_str()), cx);
    assert_eq!(thread(second).unread_count, 0);
    cx.dispatch_action(Undo);
    cx.run_until_parked();
    assert_eq!(thread(second).unread_count, 1);
    assert!(thread(second).is_flagged, "only the last change was undone");

    // Archive moves it out of the Inbox.
    hover("thread-row-1", cx);
    click(
        Box::leak(part(second, "quick-archive").into_boxed_str()),
        cx,
    );
    assert_eq!(
        thread(second).mailbox_id,
        harness.archive_id.expect("an archive")
    );
    assert_eq!(selected_thread(&view, cx), Some(first));
}
