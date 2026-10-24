//! New-mail notifications (#288): what the sync service reports is posted
//! as a system notification, and clicking it opens that conversation.
//! GPUI's test platform records the posted notifications and simulates
//! the click; the operating systems' own click delivery is not covered.

use std::sync::Arc;

use gpui::{AppContext as _, Entity, SystemNotificationResponse, VisualTestContext};
use zmail_notify::{NewMailArrival, NewMailBatch};

use super::{Harness, Setup, focused_pane, open_workspace, open_workspace_on, selected_thread};
use crate::notifications::MailNotifier;
use crate::pane::Pane;
use crate::{InitialView, ListMode, WorkspaceView};

fn notifier(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Arc<MailNotifier> {
    cx.read_entity(view, |view, _| view.notifier.clone())
}

/// One arrival in Home's Inbox, as the sync service reports it.
fn home_arrival(harness: &Harness, remote_thread_id: &str) -> NewMailBatch {
    NewMailBatch {
        account_id: harness.home_account_id.expect("the Home account"),
        account: "Home".to_owned(),
        arrivals: vec![NewMailArrival {
            subject: format!("Home {remote_thread_id}"),
            sender: "Kai Santos".to_owned(),
            mailbox: "INBOX".to_owned(),
            remote_thread_id: remote_thread_id.to_owned(),
        }],
    }
}

/// Reports `batch` with the window in the background and returns the tag
/// of the notification it posted.
fn post(view: &Entity<WorkspaceView>, batch: &NewMailBatch, cx: &mut VisualTestContext) -> String {
    let notifier = notifier(view, cx);
    notifier.set_quiet(false);
    notifier.notify_new_mail(batch);
    cx.run_until_parked();
    let shown = cx.shown_system_notifications();
    let posted = shown.last().expect("a notification is posted");
    posted.tag.to_string()
}

fn click(tag: &str, cx: &mut VisualTestContext) {
    cx.simulate_system_notification_response(SystemNotificationResponse {
        tag: tag.to_owned().into(),
        action_id: None,
    });
    cx.run_until_parked();
}

#[gpui::test]
fn reported_mail_is_posted_with_an_opaque_tag(cx: &mut gpui::TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let tag = post(&view, &home_arrival(&harness, "oldest"), cx);

    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title.as_ref(), "New mail: Home oldest");
    assert!(shown[0].body.contains("Home"), "{}", shown[0].body);
    assert!(shown[0].actions.is_empty());
    assert!(
        !tag.contains("oldest") && !tag.contains("INBOX"),
        "the tag carries no server ids: {tag}"
    );

    // While the window is focused, nothing is posted.
    let notifier = notifier(&view, cx);
    notifier.set_quiet(true);
    notifier.notify_new_mail(&home_arrival(&harness, "newest"));
    cx.run_until_parked();
    assert_eq!(cx.shown_system_notifications().len(), 1);
}

#[gpui::test]
fn clicking_a_notification_opens_its_conversation_in_another_account(
    cx: &mut gpui::TestAppContext,
) {
    // A search is open in the Work account's Inbox.
    let (view, cx, harness) =
        open_workspace_on(cx, Setup::Accounts, |_| InitialView::Search("Quarterly"));
    let work_inbox = harness.inbox_id.expect("Work's Inbox");
    assert_eq!(
        cx.read_entity(&view, |view, _| (view.selected_mailbox_id, view.list_mode)),
        (Some(work_inbox), ListMode::Search)
    );

    let tag = post(&view, &home_arrival(&harness, "oldest"), cx);
    click(&tag, cx);

    let oldest = harness.home_threads[1];
    assert_eq!(selected_thread(&view, cx), Some(oldest));
    assert_eq!(
        cx.read_entity(&view, |view, _| (
            view.selected_account_id,
            view.selected_mailbox_id,
            view.list_mode,
        )),
        (
            harness.home_account_id,
            harness.home_inbox_id,
            ListMode::Mail
        ),
        "the conversation opens selected in its own mailbox"
    );
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    assert!(
        cx.read_entity(&view, |view, cx| view
            .reader
            .read(cx)
            .state()
            .thread
            .as_ref()
            .is_some_and(|thread| thread.id == oldest)),
        "the reader shows the conversation"
    );
}

#[gpui::test]
fn clicking_a_notification_for_a_gone_conversation_changes_nothing(cx: &mut gpui::TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let before = cx.read_entity(&view, |view, _| {
        (view.selected_mailbox_id, view.selected_thread_id)
    });

    let tag = post(&view, &home_arrival(&harness, "moved-away"), cx);
    click(&tag, cx);
    // A tag from an earlier run is not known at all.
    click("new-mail-999", cx);

    assert_eq!(
        cx.read_entity(&view, |view, _| (
            view.selected_mailbox_id,
            view.selected_thread_id
        )),
        before
    );
}
