//! Attachments copy in off the UI thread: a card per file while it
//! copies, Send waiting for them, a card that says which file could not
//! be attached and why, and a warning past 25 MB in all.

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zmail_core::ReceivedAttachment;

use super::{Harness, Setup, click_on, compose_window, keys, open_workspace};
use crate::WorkspaceView;
use crate::actions::Cancel;
use crate::compose::ComposeView;
use crate::compose::send_checks::WAIT_FOR_ATTACHMENTS;

/// Opens a new message in its compose window, with a recipient and a
/// subject so Send has nothing else to ask.
fn new_message(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceView>,
    &mut VisualTestContext,
    Entity<ComposeView>,
    VisualTestContext,
    Harness,
) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let (compose, mut w) = compose_window(&view, cx);
    w.update(|window, _| window.activate_window());
    w.simulate_input("kai@example.test,");
    let subject = w.read_entity(&compose, |compose, _| {
        compose.compose.as_ref().expect("a draft").subject.clone()
    });
    w.update(|_, cx| subject.update(cx, |input, cx| input.set_text("Logs", cx)));
    w.run_until_parked();
    (view, cx, compose, w, harness)
}

fn file(harness: &Harness, name: &str, contents: &[u8]) -> std::path::PathBuf {
    let dir = harness
        .store
        .path()
        .parent()
        .expect("the store has a folder")
        .join("attach-sources");
    std::fs::create_dir_all(&dir).expect("the folder is made");
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("the file is written");
    path
}

/// The cards still pending: each file name and what it says (`None`
/// while copying).
fn pending(
    compose: &Entity<ComposeView>,
    cx: &mut VisualTestContext,
) -> Vec<(String, Option<String>)> {
    cx.read_entity(compose, |compose, _| {
        compose
            .pending_attachments()
            .iter()
            .map(|pending| (pending.filename.clone(), pending.failure.clone()))
            .collect()
    })
}

fn attached(compose: &Entity<ComposeView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.read_entity(compose, |compose, _| {
        compose
            .compose
            .as_ref()
            .expect("a draft")
            .attachments
            .iter()
            .map(|attachment| attachment.filename.clone())
            .collect()
    })
}

#[gpui::test]
fn files_copy_in_the_background_and_send_waits_for_them(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, harness) = new_message(cx);
    let path = file(&harness, "report.pdf", b"%PDF-1.7 report");

    // Attaching returns at once: the file is still to be copied, shown as
    // a card, and Send says to wait.
    let (attaching, problem) = w.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.attach_paths(vec![path], cx);
            let attaching = compose.attaching();
            compose.send_draft(window, cx);
            (attaching, compose.send_problem().map(str::to_owned))
        })
    });
    assert!(attaching);
    assert_eq!(problem.as_deref(), Some(WAIT_FOR_ATTACHMENTS));
    assert_eq!(pending(&compose, &mut w), [("report.pdf".to_owned(), None)]);
    assert!(attached(&compose, &mut w).is_empty());
    assert!(!main.read_entity(&view, |view, _| view.compose_windows.is_empty()));

    // Once copied it is the draft's, Send's reason goes, and Send sends.
    w.run_until_parked();
    assert!(pending(&compose, &mut w).is_empty());
    assert_eq!(attached(&compose, &mut w), ["report.pdf"]);
    assert_eq!(
        w.read_entity(&compose, |compose, _| compose
            .send_problem()
            .map(str::to_owned)),
        None
    );
    keys("secondary-enter", &mut w);
    assert!(main.read_entity(&view, |view, _| view.compose_windows.is_empty()));
    assert_eq!(
        harness
            .store
            .list_outbox_jobs(None)
            .expect("jobs load")
            .len(),
        1
    );
}

#[gpui::test]
fn a_file_that_cannot_be_attached_says_which_and_why(cx: &mut TestAppContext) {
    let (_view, _main, compose, mut w, harness) = new_message(cx);
    let folder = file(&harness, "notes.txt", b"notes")
        .parent()
        .expect("a folder")
        .to_path_buf();
    let missing = folder.join("gone.pdf");
    w.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.attach_paths(vec![missing, folder], cx)
        })
    });
    w.run_until_parked();
    assert_eq!(
        pending(&compose, &mut w),
        [
            (
                "gone.pdf".to_owned(),
                Some("the file isn't there any more".to_owned())
            ),
            (
                "attach-sources".to_owned(),
                Some("it's a folder; attach the files in it instead".to_owned())
            ),
        ]
    );
    // They do not hold Send up, and each can be dismissed.
    assert!(!w.read_entity(&compose, |compose, _| compose.attaching()));
    click_on("compose-attachment-dismiss-0".to_owned(), &mut w);
    assert_eq!(pending(&compose, &mut w).len(), 1);
}

#[gpui::test]
fn large_attachments_warn_without_blocking(cx: &mut TestAppContext) {
    let (_view, _main, compose, mut w, _harness) = new_message(cx);
    // A forward's 30 MB attachment, counted while it copies; this one
    // is not in the store, so it then fails as no longer stored.
    let notice = w.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.attach_received(
                vec![ReceivedAttachment {
                    id: 4242,
                    filename: "video.mp4".to_owned(),
                    content_type: "video/mp4".to_owned(),
                    size_bytes: 30 * 1024 * 1024,
                    storage_path: String::new(),
                }],
                cx,
            );
            compose.large_attachments_notice()
        })
    });
    assert_eq!(
        notice.as_deref(),
        Some(
            "The attachments come to 30.0 MB. Many providers refuse messages over 25 MB, so \
             this one may not send."
        )
    );
    w.run_until_parked();
    assert_eq!(
        pending(&compose, &mut w),
        [(
            "video.mp4".to_owned(),
            Some("it isn't stored any more".to_owned())
        )]
    );
    assert_eq!(
        w.read_entity(&compose, |compose, _| compose.large_attachments_notice()),
        None
    );
}

#[gpui::test]
fn closing_the_window_while_attaching_still_attaches(cx: &mut TestAppContext) {
    let (view, main, compose, mut w, harness) = new_message(cx);
    let draft_id = w.read_entity(&compose, |compose, _| compose.draft_id().expect("a draft"));
    let path = file(&harness, "logs.txt", b"log lines");
    w.update(|window, cx| {
        compose.update(cx, |compose, cx| compose.attach_paths(vec![path], cx));
        window.dispatch_action(Box::new(Cancel), cx);
    });
    w.run_until_parked();
    main.run_until_parked();
    assert!(main.read_entity(&view, |view, _| view.compose_windows.is_empty()));
    let draft = harness
        .store
        .draft_detail(draft_id)
        .expect("the draft loads")
        .expect("the draft exists");
    assert_eq!(draft.attachments.len(), 1);
    assert_eq!(draft.attachments[0].filename, "logs.txt");
}
