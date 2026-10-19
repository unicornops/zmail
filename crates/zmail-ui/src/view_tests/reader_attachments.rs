//! Attachment cards (UI plan Phase 4): Open into the private cache, the
//! warning before opening a program, Save As and Save All.

use std::{fs, path::PathBuf};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zmail_core::{
    AccountSyncSnapshot, MailboxSnapshot, MessageAttachment, MessageSnapshot, ThreadSnapshot,
};
use zmail_store::LocalStore;

use super::{Harness, Setup, click, click_on, open_workspace_with};
use crate::actions::{SaveAttachments, SaveMessageAttachments};
use crate::{InitialView, WorkspaceView};

/// The seeded attachments: file name, MIME type and contents.
const FILES: &[(&str, &str, &[u8])] = &[
    ("report.pdf", "application/pdf", b"%PDF report"),
    ("notes.txt", "text/plain", b"plain notes"),
    ("setup.exe", "application/octet-stream", b"MZ program"),
];

/// One thread whose message carries [`FILES`], with their blobs written
/// to the store's attachment directory.
fn seed(store: &LocalStore) {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    let target = store
        .sync_target(account_id)
        .expect("the target loads")
        .expect("the account exists");
    let blobs = PathBuf::from(&target.attachments_dir);
    fs::create_dir_all(&blobs).expect("the attachment dir is created");
    let attachments = FILES
        .iter()
        .map(|(name, content_type, bytes)| {
            let storage_path = format!("blob-{name}");
            fs::write(blobs.join(&storage_path), bytes).expect("the blob is written");
            MessageAttachment {
                filename: (*name).to_owned(),
                content_type: (*content_type).to_owned(),
                size_bytes: bytes.len() as u64,
                storage_path,
            }
        })
        .collect();
    let at = "2026-03-24T09:00:00Z".to_owned();
    let thread = ThreadSnapshot {
        remote_thread_id: "files".to_owned(),
        subject: "Files for the review".to_owned(),
        participants: "Kai Santos <kai@example.test>".to_owned(),
        preview: "Three files attached".to_owned(),
        is_flagged: false,
        has_attachments: true,
        unread_count: 1,
        latest_message_at: at.clone(),
        messages: vec![MessageSnapshot {
            remote_id: "files-1".to_owned(),
            identity_email: "me@example.test".to_owned(),
            sender: "Kai Santos <kai@example.test>".to_owned(),
            reply_to: None,
            recipients: "me@example.test".to_owned(),
            subject: "Files for the review".to_owned(),
            body_preview: "Three files attached".to_owned(),
            body_text: Some("Three files attached".to_owned()),
            sanitized_html: None,
            has_remote_content: false,
            is_read: false,
            is_flagged: false,
            received_at: at,
            attachments,
        }],
    };
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

/// The thread id and its attachment ids by file name.
fn thread_and_attachments(harness: &Harness) -> (i64, Vec<(String, i64)>) {
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
    let attachments = thread.messages[0]
        .attachments
        .iter()
        .map(|attachment| (attachment.filename.clone(), attachment.id))
        .collect();
    (thread_id, attachments)
}

struct Opened<'a> {
    view: Entity<WorkspaceView>,
    cx: &'a mut VisualTestContext,
    harness: Harness,
    attachments: Vec<(String, i64)>,
}

impl Opened<'_> {
    fn id(&self, name: &str) -> i64 {
        self.attachments
            .iter()
            .find(|(filename, _)| filename == name)
            .map(|(_, id)| *id)
            .expect("the attachment is seeded")
    }

    fn opened(&mut self) -> Vec<PathBuf> {
        self.cx
            .read_entity(&self.view, |view, _| view.attachments.opened.clone())
    }

    fn warning_pending(&mut self) -> bool {
        self.cx.read_entity(&self.view, |view, _| {
            view.attachments.pending_open().is_some()
        })
    }
}

fn open(cx: &mut TestAppContext) -> Opened<'_> {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Account,
        |harness| InitialView::Thread(thread_and_attachments(harness).0),
        seed,
    );
    let attachments = thread_and_attachments(&harness).1;
    Opened {
        view,
        cx,
        harness,
        attachments,
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zmail-view-attachments-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the scratch dir is created");
    dir
}

#[gpui::test]
fn every_attachment_gets_a_card(cx: &mut TestAppContext) {
    let mut t = open(cx);
    for (name, _, _) in FILES {
        let id = t.id(name);
        assert!(
            t.cx.debug_bounds(Box::leak(format!("attachment-{id}").into_boxed_str()))
                .is_some(),
            "{name} has a card"
        );
    }
    assert!(t.opened().is_empty());
}

#[gpui::test]
fn open_copies_into_the_private_cache_then_opens(cx: &mut TestAppContext) {
    let mut t = open(cx);
    let id = t.id("report.pdf");
    click_on(format!("attachment-open-{id}"), t.cx);

    let opened = t.opened();
    assert_eq!(opened.len(), 1, "a document opens without a warning");
    let store_dir = t
        .harness
        .store
        .path()
        .parent()
        .expect("the store has a directory")
        .to_owned();
    assert!(opened[0].starts_with(store_dir.join("opened-attachments")));
    assert_eq!(
        opened[0].file_name().and_then(|name| name.to_str()),
        Some("report.pdf")
    );
    assert_eq!(
        fs::read(&opened[0]).expect("the copy reads"),
        b"%PDF report"
    );
}

#[gpui::test]
fn a_program_opens_only_after_the_warning(cx: &mut TestAppContext) {
    let mut t = open(cx);
    let id = t.id("setup.exe");

    click_on(format!("attachment-open-{id}"), t.cx);
    assert!(t.warning_pending(), "the warning shows");
    assert!(t.opened().is_empty(), "nothing opens yet");
    click("attachment-open-cancel", t.cx);
    assert!(!t.warning_pending());
    assert!(t.opened().is_empty(), "Cancel opens nothing");

    click_on(format!("attachment-open-{id}"), t.cx);
    click("attachment-open-anyway", t.cx);
    let opened = t.opened();
    assert_eq!(opened.len(), 1);
    assert_eq!(fs::read(&opened[0]).expect("the copy reads"), b"MZ program");
}

#[gpui::test]
fn save_as_writes_where_the_dialog_points(cx: &mut TestAppContext) {
    let t = open(cx);
    let id = t.id("notes.txt");
    let dir = scratch("save-as");
    let destination = dir.join("my notes.txt");

    click_on(format!("attachment-save-{id}"), t.cx);
    assert!(t.cx.did_prompt_for_new_path());
    let chosen = destination.clone();
    t.cx.simulate_new_path_selection(move |_| Some(chosen));
    t.cx.run_until_parked();
    assert_eq!(
        fs::read(&destination).expect("the file is saved"),
        b"plain notes"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[gpui::test]
fn save_all_saves_every_file_without_overwriting(cx: &mut TestAppContext) {
    let t = open(cx);
    let folder = scratch("save-all");
    fs::write(folder.join("report.pdf"), b"already here").expect("the old file is written");

    t.cx.dispatch_action(SaveAttachments);
    t.cx.run_until_parked();
    assert!(t.cx.did_prompt_for_paths(), "Save All asks for a folder");
    let chosen = folder.clone();
    t.cx.simulate_path_prompt_response(move |options| {
        assert!(options.directories && !options.files);
        Some(vec![chosen])
    });
    t.cx.run_until_parked();

    assert_eq!(
        fs::read(folder.join("report.pdf")).expect("reads"),
        b"already here"
    );
    assert_eq!(
        fs::read(folder.join("report (1).pdf")).expect("reads"),
        b"%PDF report"
    );
    assert_eq!(
        fs::read(folder.join("notes.txt")).expect("reads"),
        b"plain notes"
    );
    assert_eq!(
        fs::read(folder.join("setup.exe")).expect("reads"),
        b"MZ program"
    );
    let _ = fs::remove_dir_all(&folder);
}

#[gpui::test]
fn save_all_for_one_message_saves_its_files(cx: &mut TestAppContext) {
    let t = open(cx);
    let folder = scratch("save-message");
    let message_id = t
        .harness
        .store
        .thread_view(thread_and_attachments(&t.harness).0)
        .expect("the thread loads")
        .expect("the thread exists")
        .messages[0]
        .id;

    t.cx.dispatch_action(SaveMessageAttachments { message_id });
    t.cx.run_until_parked();
    let chosen = folder.clone();
    t.cx.simulate_path_prompt_response(move |_| Some(vec![chosen]));
    t.cx.run_until_parked();
    for (name, _, bytes) in FILES {
        assert_eq!(fs::read(folder.join(name)).expect("reads"), *bytes);
    }
    let _ = fs::remove_dir_all(&folder);
}
