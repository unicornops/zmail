//! Beside the Outbox: the selected message with its error, Retry Now,
//! Edit and Delete, and no conversation commands (which used to act on
//! whatever conversation the hidden reader still held).

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zmail_core::{DraftState, OutboxJobState};

use super::{Setup, click, keys, mailbox_of, open_workspace, queue_test_message, toasts};
use crate::actions::ShowOutbox;
use crate::{DetailMode, WorkspaceView};

fn refresh(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    view.update(cx, |view, cx| {
        view.refresh_from_store(cx);
        cx.notify();
    });
    cx.run_until_parked();
}

fn show_outbox(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    refresh(view, cx);
    cx.dispatch_action(ShowOutbox);
    cx.run_until_parked();
    assert_eq!(
        cx.read_entity(view, |view, _| view.detail_mode),
        DetailMode::Outbox
    );
}

/// The job shown beside the Outbox.
fn shown_job(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<i64> {
    cx.read_entity(view, |view, cx| {
        view.outbox_detail
            .read(cx)
            .state()
            .job
            .as_ref()
            .map(|job| job.id)
    })
}

fn delete_question(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<bool> {
    cx.update(|window, cx| {
        view.read(cx)
            .outbox_detail
            .read(cx)
            .delete_confirmation(window)
    })
}

fn job_state(
    view: &Entity<WorkspaceView>,
    job_id: i64,
    cx: &mut VisualTestContext,
) -> Option<OutboxJobState> {
    cx.read_entity(view, |view, _| {
        view.store
            .list_outbox_jobs(None)
            .expect("the jobs load")
            .into_iter()
            .find(|job| job.id == job_id)
            .map(|job| job.state)
    })
}

#[gpui::test]
fn conversation_commands_never_reach_the_inbox_from_the_outbox(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let open = harness.threads[0];
    assert_eq!(super::selected_thread(&view, cx), Some(open));
    let job = queue_test_message(&harness.store, "Stuck");
    harness
        .store
        .record_outbox_failure(job, "550 mailbox unavailable")
        .expect("the failure is recorded");
    show_outbox(&view, cx);
    assert_eq!(shown_job(&view, cx), Some(job));

    // Archive, Flag, Read and Delete: none touches the conversation the
    // reader held before.
    let flags = || {
        harness
            .store
            .thread_state(open)
            .expect("the thread loads")
            .map(|thread| (thread.is_flagged(), thread.is_read()))
    };
    let before = flags();
    keys("secondary-e secondary-shift-l secondary-shift-u", cx);
    keys("backspace", cx);
    assert_eq!(mailbox_of(&harness, open), harness.inbox_id);
    assert_eq!(flags(), before);
    assert!(toasts(&view, cx).is_empty(), "no mail change was made");
    // Delete asks about the Outbox message instead.
    assert_eq!(delete_question(&view, cx), Some(true));
    keys("escape", cx);
    assert_eq!(delete_question(&view, cx), None);
    assert_eq!(job_state(&view, job, cx), Some(OutboxJobState::Failed));
}

#[gpui::test]
fn the_outbox_detail_shows_the_error_and_retries_edits_and_deletes(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let job = queue_test_message(&harness.store, "Report");
    harness
        .store
        .record_outbox_failure(job, "550 mailbox unavailable")
        .expect("the failure is recorded");
    show_outbox(&view, cx);
    assert!(cx.debug_bounds("outbox-detail-error").is_some());

    // Retry Now requeues it.
    click("outbox-detail-retry", cx);
    assert_ne!(job_state(&view, job, cx), Some(OutboxJobState::Failed));

    // Edit opens a failed message in a compose window.
    harness
        .store
        .record_outbox_failure(job, "550 mailbox unavailable")
        .expect("the failure is recorded");
    refresh(&view, cx);
    click("outbox-detail-edit", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.compose_windows.len()),
        1
    );
    let (_, mut w) = super::compose_window(&view, cx);
    w.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // Delete asks, Cancel focused, then deletes the message and its draft.
    let draft_id = cx.read_entity(&view, |view, _| view.outbox_jobs[0].draft_id);
    click("outbox-detail-delete", cx);
    assert_eq!(delete_question(&view, cx), Some(true));
    click("outbox-delete-confirm", cx);
    assert_eq!(job_state(&view, job, cx), None);
    assert!(
        harness
            .store
            .draft_detail(draft_id)
            .expect("the draft lookup")
            .is_none()
    );
    assert_eq!(toasts(&view, cx), ["Message deleted"]);
    assert_eq!(shown_job(&view, cx), None);
}

#[gpui::test]
fn editing_a_waiting_message_takes_it_back(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let job = queue_test_message(&harness.store, "Not yet");
    show_outbox(&view, cx);
    let draft_id = cx.read_entity(&view, |view, _| view.outbox_jobs[0].draft_id);
    click("outbox-detail-edit", cx);
    assert_eq!(job_state(&view, job, cx), None, "taken back");
    let draft = harness
        .store
        .draft_detail(draft_id)
        .expect("the draft lookup")
        .expect("the draft stays");
    assert_eq!(draft.state, DraftState::Editing);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.compose_windows.len()),
        1
    );
}
