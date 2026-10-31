//! Triage in the terminal: each command and its undo through
//! `zmail-actions`, the save prompt's completion and folder list, `;`
//! selection with Apply, marking read on open, and changes made while
//! another Zmail holds the sync lock (in this process, and from a second
//! one).

use std::{sync::Arc, time::Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use zmail_core::{AccountSyncSnapshot, MailboxRole, PendingOpKind, time::HourCycle};
use zmail_store::LocalStore;

use super::{
    Mail, app, app_with, assert_printable, assert_snapshot, click, control, draw, environment, key,
    keys, mailbox, now, open_work_inbox, row_of, seed, seeded_store, text_of, thread,
};
use crate::app::{App, AppOptions, Prompt, Screen};

/// The store's accounts, Work then Home.
fn accounts(store: &LocalStore) -> (i64, i64) {
    let accounts = store.list_accounts().expect("accounts");
    let id = |name: &str| {
        accounts
            .iter()
            .find(|account| account.display_name == name)
            .expect("the account")
            .id
    };
    (id("Work"), id("Home"))
}

fn mailbox_id(store: &LocalStore, account_id: i64, role: MailboxRole) -> i64 {
    store
        .mailbox_id_by_role(account_id, role.as_str())
        .expect("the lookup")
        .expect("the mailbox")
}

/// The mailbox `thread_id` is in now.
fn mailbox_of(store: &LocalStore, thread_id: i64) -> Option<i64> {
    store
        .thread_state(thread_id)
        .expect("the thread loads")
        .map(|thread| thread.mailbox_id)
}

fn queued(store: &LocalStore, account_id: i64) -> Vec<PendingOpKind> {
    store
        .list_pending_ops(account_id)
        .expect("the ops load")
        .into_iter()
        .map(|op| op.kind)
        .collect()
}

/// The conversation under the index's cursor.
fn current(app: &App) -> i64 {
    app.index.entries[app.index.cursor].thread_id
}

fn status(app: &App) -> &str {
    app.status.as_deref().unwrap_or_default()
}

#[test]
fn each_command_changes_the_store_says_so_and_undoes() {
    let store = seeded_store();
    let (work, _) = accounts(&store);
    let trash = mailbox_id(&store, work, MailboxRole::Trash);
    let archive = mailbox_id(&store, work, MailboxRole::Archive);
    let inbox = mailbox_id(&store, work, MailboxRole::Inbox);
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);
    let launch = current(&app);

    // D: to the Trash, with the undo hint, and the write-back queued.
    keys(&mut app, "d");
    assert_eq!(mailbox_of(&store, launch), Some(trash));
    assert_eq!(status(&app), "Conversation moved to Trash. U Undo");
    assert!(queued(&store, work).contains(&PendingOpKind::Move));
    assert_ne!(current(&app), launch, "the cursor is on the next one");
    assert_eq!(app.index.cursor, 0);
    assert_snapshot("status-after-delete-80x24", &draw(&mut app, 80, 24));

    // U: back where it was, nothing left to write back, cursor on it.
    keys(&mut app, "u");
    assert_eq!(mailbox_of(&store, launch), Some(inbox));
    assert_eq!(status(&app), "Undone: Conversation moved to Trash");
    assert_eq!(queued(&store, work), []);
    assert_eq!(current(&app), launch);

    // E archives; z is U's alias.
    keys(&mut app, "e");
    assert_eq!(mailbox_of(&store, launch), Some(archive));
    assert_eq!(status(&app), "Conversation archived. U Undo");
    keys(&mut app, "z");
    assert_eq!(mailbox_of(&store, launch), Some(inbox));

    // *: the launch plan is flagged, so it is unflagged, and back.
    let flagged = |app: &App| app.index.entries[app.index.cursor].flagged;
    assert!(flagged(&app));
    let screen = text_of(&draw(&mut app, 160, 48));
    assert!(screen.contains("* Unflag"), "{screen}");
    keys(&mut app, "*");
    assert!(!flagged(&app));
    assert_eq!(status(&app), "Flag removed. U Undo");
    keys(&mut app, "*");
    assert!(flagged(&app));
    assert_eq!(status(&app), "Conversation flagged. U Undo");

    // K: partly unread, so marked read; again, unread; undo, read again.
    let unread = |app: &App| app.index.entries[app.index.cursor].unread;
    assert!(unread(&app));
    keys(&mut app, "k");
    assert!(!unread(&app));
    assert_eq!(status(&app), "Marked as read. U Undo");
    keys(&mut app, "K");
    assert!(unread(&app));
    assert_eq!(status(&app), "Marked as unread. U Undo");
    keys(&mut app, "u");
    assert!(!unread(&app));
    // Each undo takes one change back, newest first.
    keys(&mut app, "uuu");
    assert!(unread(&app) && flagged(&app));
    keys(&mut app, "u");
    assert_eq!(status(&app), "Nothing to undo");
}

#[test]
fn without_a_trash_delete_is_for_good_and_says_it_cannot_be_undone() {
    let store = seeded_store();
    let (_, home) = accounts(&store);
    let mut app = app_with(store.clone(), environment());
    keys(&mut app, "l");
    let buffer = draw(&mut app, 80, 24);
    // Home's INBOX is the second one listed.
    let row = text_of(&buffer)
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("INBOX"))
        .map(|(row, _)| row)
        .nth(1)
        .expect("Home's INBOX") as u16;
    click(&mut app, 6, row);
    let ferry = current(&app);
    keys(&mut app, "d");
    assert_eq!(mailbox_of(&store, ferry), None);
    assert_eq!(status(&app), "Conversation deleted");
    assert_eq!(queued(&store, home), [PendingOpKind::Destroy]);
    keys(&mut app, "u");
    assert!(status(&app).contains("can't be undone"), "{}", status(&app));
}

#[test]
fn undo_reaches_a_change_made_in_message_text() {
    let store = seeded_store();
    let (work, _) = accounts(&store);
    let inbox = mailbox_id(&store, work, MailboxRole::Inbox);
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Message);
    let launch = app.message.as_ref().expect("open").conversation.thread_id;

    // D in Message Text goes on to the next message, as Alpine does.
    keys(&mut app, "d");
    assert_eq!(app.screen, Screen::Message);
    let next = app.message.as_ref().expect("open").conversation.thread_id;
    assert_ne!(next, launch);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("[Conversation moved to Trash. U Undo]"),
        "{screen}"
    );
    assert!(screen.contains("Harbour"), "{screen}");

    // Back in the index, U still undoes it.
    keys(&mut app, "<");
    assert_eq!(app.screen, Screen::Index);
    keys(&mut app, "u");
    assert_eq!(mailbox_of(&store, launch), Some(inbox));
    assert_eq!(current(&app), launch);

    // The last message deleted from Message Text leaves for the index.
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Enter);
    keys(&mut app, "e");
    assert_eq!(app.screen, Screen::Message, "the one before it opens");
    keys(&mut app, "eee");
    assert_eq!(app.screen, Screen::Index);
    assert!(app.index.entries.is_empty());
    assert!(text_of(&draw(&mut app, 80, 24)).contains("no messages in this folder"));
}

#[test]
fn the_save_prompt_completes_lists_and_moves() {
    let store = seeded_store();
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);
    let launch = current(&app);

    keys(&mut app, "s");
    assert!(matches!(app.prompt, Some(Prompt::Save(_))));
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("Save to folder:"), "{screen}");
    assert!(screen.contains("Tab Complete"), "{screen}");
    // "r" starts no folder name but is in three: Tab goes through them.
    keys(&mut app, "r");
    key(&mut app, KeyCode::Tab);
    let buffer = draw(&mut app, 80, 24);
    assert_snapshot("save-prompt-80x24", &buffer);
    assert!(text_of(&buffer).contains("Save to folder: Archive"));
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    assert!(text_of(&draw(&mut app, 80, 24)).contains("Save to folder: Projects/Website"));
    key(&mut app, KeyCode::Enter);
    assert!(app.prompt.is_none());
    assert_eq!(
        status(&app),
        "Conversation moved to Projects/Website. U Undo"
    );
    assert!(
        !app.index
            .entries
            .iter()
            .any(|entry| entry.thread_id == launch)
    );
    keys(&mut app, "u");
    assert_eq!(current(&app), launch);

    // A name that matches nothing keeps the prompt open and says so.
    keys(&mut app, "snowhere");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.prompt, Some(Prompt::Save(_))));
    assert!(text_of(&draw(&mut app, 80, 24)).contains("none match \"nowhere\""));
    key(&mut app, KeyCode::Esc);
    assert!(app.prompt.is_none());

    // ^T lists the folders (not the one shown); Enter or a click saves.
    keys(&mut app, "s");
    control(&mut app, 't');
    let buffer = draw(&mut app, 80, 24);
    assert_snapshot("save-folder-list-80x24", &buffer);
    let screen = text_of(&buffer);
    assert!(screen.contains("SAVE TO FOLDER"), "{screen}");
    assert!(!screen.contains("    INBOX"), "{screen}");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(status(&app), "Conversation moved to Trash. U Undo");
    keys(&mut app, "u");
    keys(&mut app, "s");
    control(&mut app, 't');
    let buffer = draw(&mut app, 80, 24);
    let row = row_of(&buffer, "    Archive");
    click(&mut app, 8, row);
    assert_eq!(status(&app), "Conversation moved to Archive. U Undo");
}

#[test]
fn folder_names_in_the_save_prompt_are_filtered() {
    let store = seeded_store();
    let (_, home) = accounts(&store);
    store
        .replace_account_snapshot(
            home,
            &AccountSyncSnapshot {
                checkpoint: "test:2".to_owned(),
                detail: "Up to date".to_owned(),
                identities: None,
                mailboxes: vec![
                    mailbox(
                        "INBOX",
                        Some("inbox"),
                        vec![thread(
                            "ferry",
                            "Ferry tickets for August",
                            false,
                            &[Mail {
                                sender: "Harbour Ferries <bookings@ferries.example>",
                                minutes_ago: 50,
                                read: false,
                                text: Some("Your booking is confirmed."),
                                html: None,
                                attachments: &[],
                            }],
                        )],
                    ),
                    mailbox(
                        "Bills\x1b]0;pwned\x07\u{202E}2026\x1b[31m",
                        None,
                        Vec::new(),
                    ),
                ],
            },
        )
        .expect("the Home mail is stored");
    let mut app = app_with(store, environment());
    keys(&mut app, "l");
    let buffer = draw(&mut app, 80, 24);
    let row = text_of(&buffer)
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("INBOX"))
        .map(|(row, _)| row)
        .nth(1)
        .expect("Home's INBOX") as u16;
    click(&mut app, 6, row);
    keys(&mut app, "sb");
    key(&mut app, KeyCode::Tab);
    let buffer = draw(&mut app, 80, 24);
    assert_printable(&buffer);
    let screen = text_of(&buffer);
    assert!(screen.contains("Save to folder: Bills2026"), "{screen}");
    assert!(!screen.contains("pwned"), "{screen}");
    control(&mut app, 't');
    let buffer = draw(&mut app, 80, 24);
    assert_printable(&buffer);
    key(&mut app, KeyCode::Enter);
    let buffer = draw(&mut app, 80, 24);
    assert_printable(&buffer);
    assert_eq!(status(&app), "Conversation moved to Bills2026. U Undo");
}

#[test]
fn selection_marks_conversations_and_apply_acts_on_them_all() {
    let store = seeded_store();
    let (work, _) = accounts(&store);
    let archive = mailbox_id(&store, work, MailboxRole::Archive);
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);

    // A with nothing selected says how.
    keys(&mut app, "a");
    assert!(app.prompt.is_none());
    assert!(status(&app).contains("Select messages with ;"));

    // ; marks the row and moves on; a click on the marker column too.
    let first = current(&app);
    keys(&mut app, ";");
    assert_eq!(app.index.cursor, 1);
    let buffer = draw(&mut app, 80, 24);
    let tokyo_row = row_of(&buffer, "東京");
    click(&mut app, 0, tokyo_row);
    assert_eq!(app.index.cursor, 1, "the marker click doesn't open it");
    assert_eq!(app.screen, Screen::Index);
    assert_eq!(app.selected.len(), 2);
    assert_eq!(
        status(&app),
        "2 conversations selected. A applies a command to them"
    );
    let buffer = draw(&mut app, 80, 24);
    assert_snapshot("selection-80x24", &buffer);
    let screen = text_of(&buffer);
    assert!(
        screen
            .lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("X "))
    );
    assert!(
        screen
            .lines()
            .nth(tokyo_row as usize)
            .is_some_and(|line| line.starts_with("X "))
    );
    // A second marker click unselects; ; puts it back.
    click(&mut app, 0, tokyo_row);
    assert_eq!(app.selected.len(), 1);
    click(&mut app, 0, tokyo_row);
    assert_eq!(app.selected.len(), 2);

    // A asks; E archives both as one change, and one undo brings both.
    keys(&mut app, "a");
    assert_eq!(app.prompt, Some(Prompt::Apply));
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("Apply to 2 conversations"), "{screen}");
    assert!(screen.contains("E Archive"), "{screen}");
    keys(&mut app, "e");
    assert_eq!(status(&app), "2 conversations archived. U Undo");
    assert_eq!(mailbox_of(&store, first), Some(archive));
    assert!(app.selected.is_empty(), "archived out of the folder");
    assert_eq!(app.index.entries.len(), 2);
    keys(&mut app, "u");
    assert_eq!(app.index.entries.len(), 4);

    // Flag and read keep the selection; ; in Apply unselects all.
    key(&mut app, KeyCode::Home);
    keys(&mut app, ";;");
    keys(&mut app, "a*");
    assert_eq!(status(&app), "2 conversations flagged. U Undo");
    assert_eq!(app.selected.len(), 2);
    keys(&mut app, "a;");
    assert!(app.selected.is_empty());
    assert_eq!(status(&app), "Unselected 2 conversations");
}

#[test]
fn a_message_row_selects_its_whole_conversation() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, "/");
    // Grace's and Ada's messages are one conversation.
    keys(&mut app, ";");
    let screen = text_of(&draw(&mut app, 80, 24));
    let marked: Vec<&str> = screen
        .lines()
        .filter(|line| line.starts_with("X "))
        .collect();
    assert_eq!(marked.len(), 2, "{screen}");
}

#[test]
fn opening_a_message_marks_it_read_after_the_gui_delay() {
    let store = seeded_store();
    let (work, _) = accounts(&store);
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);
    // The newsletter: unread.
    key(&mut app, KeyCode::Down);
    let newsletter = current(&app);
    key(&mut app, KeyCode::Enter);
    let unread = |store: &LocalStore| {
        !store
            .thread_state(newsletter)
            .expect("loads")
            .expect("exists")
            .is_read()
    };
    app.tick();
    assert!(unread(&store), "not before the delay");
    let (_, due) = app.mark_read.expect("marking read is due");
    assert!(due > Instant::now());
    app.mark_read = Some((newsletter, Instant::now()));
    app.tick();
    assert!(!unread(&store), "read once it has been open long enough");
    assert_eq!(queued(&store, work), [PendingOpKind::MarkRead]);
    // Opening isn't a change to undo.
    keys(&mut app, "u");
    assert_eq!(status(&app), "Nothing to undo");

    // K before the delay keeps it unread.
    keys(&mut app, "k");
    assert!(unread(&store));
    keys(&mut app, "<");
    key(&mut app, KeyCode::Enter);
    keys(&mut app, "k");
    assert!(!unread(&store), "K on an unread message marks it read");
    keys(&mut app, "k");
    assert!(unread(&store));
    assert!(app.mark_read.is_none());

    // Leaving before the delay leaves it unread too.
    keys(&mut app, "<");
    key(&mut app, KeyCode::Enter);
    keys(&mut app, "<");
    let (thread_id, _) = app.mark_read.expect("still pending");
    app.mark_read = Some((thread_id, Instant::now()));
    app.tick();
    assert!(unread(&store));
}

/// An app on `store` whose sync service finds the sync lock held by
/// another Zmail, as the TUI is while the GUI runs.
fn app_without_the_sync_lock(store: Arc<LocalStore>) -> (App, zmail_sync::BackgroundRuntime) {
    let runtime = zmail_sync::BackgroundRuntime::start(1).expect("a runtime");
    let sync = zmail_sync::SyncService::with_sync_lock(
        runtime.handle().clone(),
        store.clone(),
        zmail_security::InMemorySecretStore::shared(),
    )
    .expect("the service starts");
    assert!(!sync.runs_sync(), "another Zmail syncs");
    let app = App::new(AppOptions {
        store,
        sync: Some(sync),
        runtime: Some(runtime.handle().clone()),
        env: environment(),
        clock: Arc::new(now),
        hour_cycle: HourCycle::H24,
        version: "0.0.0-test".to_owned(),
        arrivals: None,
    });
    (app, runtime)
}

#[test]
fn changes_wait_in_the_store_for_the_zmail_that_syncs() {
    let store = seeded_store();
    let (work, _) = accounts(&store);
    let trash = mailbox_id(&store, work, MailboxRole::Trash);
    let held = store.try_sync_lock().expect("the lock file opens");
    assert!(held.is_some());
    let (mut app, _runtime) = app_without_the_sync_lock(store.clone());
    open_work_inbox(&mut app, 80, 24);
    let launch = current(&app);
    keys(&mut app, "d");
    assert_eq!(mailbox_of(&store, launch), Some(trash));
    assert_eq!(
        queued(&store, work),
        [PendingOpKind::Move, PendingOpKind::Move],
        "one move per message, for the holder to send"
    );
    keys(&mut app, "u");
    assert_eq!(queued(&store, work), []);
}

/// Set in the child process that
/// `a_second_process_triages_while_the_first_holds_the_sync_lock` starts:
/// the store it opens.
const CHILD_STORE: &str = "ZMAIL_TUI_TRIAGE_CHILD_STORE";

/// The fixed key both processes open the store with.
fn fixture_key() -> zmail_security::vault::StoreKey {
    zmail_security::vault::StoreKey::from_fixture(
        zmail_security::vault::KeyId::from_bytes([7; 16]),
        [42; 32],
    )
}

#[test]
fn a_second_process_triages_while_the_first_holds_the_sync_lock() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("zmail-tui-triage-{}-{nanos}", std::process::id()));
    let path = directory.join("store.sqlite3");
    let store = LocalStore::open(&path, fixture_key()).expect("the store opens");
    seed(&store);
    let (work, _) = accounts(&store);
    let trash = mailbox_id(&store, work, MailboxRole::Trash);
    let inbox = mailbox_id(&store, work, MailboxRole::Inbox);
    let launch = store.list_threads(inbox, 0..1).expect("threads")[0].id;
    // This process stands in for the GUI: it holds the sync lock.
    let held = store
        .try_sync_lock()
        .expect("the lock file opens")
        .expect("the lock is free");

    let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", "tests::triage::triage_child", "--nocapture"])
        .env(CHILD_STORE, &path)
        .output()
        .expect("the child runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "child failed: {stdout}");
    assert!(stdout.contains("child: sync lock refused"), "{stdout}");
    assert!(
        stdout.contains("child: [Conversation moved to Trash. U Undo]"),
        "{stdout}"
    );

    // The change is in the store this process syncs from, queued for it.
    assert_eq!(mailbox_of(&store, launch), Some(trash));
    assert_eq!(
        queued(&store, work),
        [PendingOpKind::Move, PendingOpKind::Move]
    );
    drop(held);
    drop(store);
    let _ = std::fs::remove_dir_all(directory);
}

/// The child half of
/// `a_second_process_triages_while_the_first_holds_the_sync_lock`: opens
/// the store, finds the sync lock taken, and deletes the Work INBOX's
/// first conversation with D. Does nothing in a normal test run.
#[test]
fn triage_child() {
    let Some(path) = std::env::var_os(CHILD_STORE) else {
        return;
    };
    let store = Arc::new(LocalStore::open(&path, fixture_key()).expect("the store opens"));
    let (mut app, _runtime) = app_without_the_sync_lock(store);
    println!("child: sync lock refused");
    open_work_inbox(&mut app, 80, 24);
    app.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char('d'),
        KeyModifiers::NONE,
    )));
    println!("child: [{}]", status(&app));
}
