//! Saving attachments from Message Text: the Attachment Index (`V`), save
//! one with its path (Tab completion, asked before replacing a file) or
//! all into a folder, and hostile names and types filtered on screen and
//! made safe on disk.

use std::{
    path::{MAIN_SEPARATOR, PathBuf},
    sync::Arc,
};

use crossterm::event::KeyCode;
use zmail_core::{AccountSyncSnapshot, MessageAttachment};
use zmail_store::LocalStore;

use super::{
    Mail, account, app, app_with, assert_printable, assert_snapshot, draw, environment, key, keys,
    mailbox, open_work_inbox, text_of, thread,
};
use crate::app::{App, Prompt, Screen};

fn status(app: &App) -> &str {
    app.status.as_deref().unwrap_or_default()
}

/// A hostile attachment name: an OSC title, a bidi override and a path.
const HOSTILE_NAME: &str = "../\x1b]0;pwned\x07invoice\u{202E}fdp.exe";

/// A store whose Work inbox holds one conversation of two messages with
/// three attachments: a contract and a file with a hostile name and type
/// whose contents the store holds, and notes it doesn't (never
/// downloaded).
fn store_with_attachments() -> Arc<LocalStore> {
    let store = Arc::new(LocalStore::open_ephemeral().expect("the store opens"));
    let work = account(&store, "Work", "me@work.example");
    let blobs = store
        .sync_target(work)
        .expect("the target loads")
        .expect("Work exists")
        .blobs;
    let mut contract = thread(
        "contract",
        "Signed contract",
        false,
        &[
            Mail {
                sender: "Ada Lovelace <ada@work.example>",
                minutes_ago: 60,
                read: true,
                text: Some("The contract, signed.\n\nAda"),
                html: None,
                attachments: &[],
            },
            Mail {
                sender: "Grace Hopper <grace@work.example>",
                minutes_ago: 30,
                read: true,
                text: Some("My notes.\n\nGrace"),
                html: None,
                attachments: &[],
            },
        ],
    );
    let attachment =
        |filename: &str, content_type: &str, storage_path: String, size| MessageAttachment {
            filename: filename.to_owned(),
            content_type: content_type.to_owned(),
            size_bytes: size,
            storage_path,
            content_id: None,
            inline_only: false,
        };
    contract.messages[0].attachments = vec![
        attachment(
            "contract.pdf",
            "application/pdf",
            blobs
                .write("contract", b"%PDF signed")
                .expect("the blob is written"),
            11,
        ),
        attachment(
            HOSTILE_NAME,
            "application/x-msdownload\x1b[2J",
            blobs.write("hostile", b"MZ").expect("the blob is written"),
            2,
        ),
    ];
    contract.messages[1].attachments = vec![attachment(
        "notes.txt",
        "text/plain",
        "missing/notes".to_owned(),
        2_400,
    )];
    store
        .replace_account_snapshot(
            work,
            &AccountSyncSnapshot {
                checkpoint: "test:1".to_owned(),
                detail: "Up to date".to_owned(),
                identities: None,
                mailboxes: vec![mailbox("INBOX", Some("inbox"), vec![contract])],
            },
        )
        .expect("the mail is stored");
    store
}

/// A fresh folder to save into.
fn scratch(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let folder = std::env::temp_dir().join(format!(
        "zmail-tui-save-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&folder).expect("the folder is made");
    folder
}

/// The app on the Attachment Index of the contract conversation, saving
/// into `folder`.
fn attachment_index(folder: Option<PathBuf>, width: u16, height: u16) -> App {
    let mut app = app_with(store_with_attachments(), environment());
    app.save_folder = folder;
    // The only folder, so its title bar names it: Enter opens it.
    keys(&mut app, "l");
    draw(&mut app, width, height);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Index);
    draw(&mut app, width, height);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Message);
    keys(&mut app, "v");
    assert_eq!(app.screen, Screen::Attachments);
    app
}

/// The prompt's field, as typed.
fn field(app: &App) -> String {
    match &app.prompt {
        Some(Prompt::Path(prompt)) => prompt.field.input.clone(),
        other => panic!("no path prompt: {other:?}"),
    }
}

#[test]
fn the_attachment_index_matches_its_snapshot_and_filters_names() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = attachment_index(Some(PathBuf::from("/saved")), width, height);
        let buffer = draw(&mut app, width, height);
        assert_printable(&buffer);
        let screen = text_of(&buffer);
        assert!(!screen.contains("pwned"), "{screen}");
        assert!(screen.contains("invoicefdp.exe"), "{screen}");
        assert_snapshot(&format!("attachments-{width}x{height}"), &buffer);
        // The save prompt, with the sanitised name in the folder saved to.
        keys(&mut app, "s");
        let mut buffer = draw(&mut app, width, height);
        assert_eq!(field(&app), slashes_back("/saved/contract.pdf"));
        if MAIN_SEPARATOR != '/' {
            // The snapshot has `/`.
            for cell in buffer.content.iter_mut() {
                if cell.symbol() == MAIN_SEPARATOR.to_string() {
                    cell.set_symbol("/");
                }
            }
        }
        assert_snapshot(&format!("attachment-save-{width}x{height}"), &buffer);
    }
}

/// The path the prompt's field names, with `~` expanded: the field shows
/// a folder under the home folder from `~` (Windows' temporary folder is
/// one).
fn field_path(app: &App) -> PathBuf {
    match &app.prompt {
        Some(Prompt::Path(prompt)) => prompt.field.path(),
        other => panic!("no path prompt: {other:?}"),
    }
}

/// `/` in `path` as this platform's separator after the first folder, as
/// the field shows a folder it was given with `/`.
fn slashes_back(path: &str) -> String {
    let (folder, rest) = path.rsplit_once('/').expect("a folder");
    format!("{folder}{MAIN_SEPARATOR}{rest}")
}

#[test]
fn message_text_offers_view_attachments_and_says_when_there_are_none() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    // The launch plan has no attachments.
    key(&mut app, KeyCode::Enter);
    let bar = app.commands();
    assert!(
        bar.iter()
            .any(|entry| entry.key == "V" && entry.label == "ViewAttch")
    );
    keys(&mut app, "v");
    assert_eq!(app.screen, Screen::Message);
    assert_eq!(status(&app), "This conversation has no attachments");
}

#[test]
fn saving_one_asks_before_replacing_and_goes_back_to_the_path_on_no() {
    let folder = scratch("one");
    let mut app = attachment_index(Some(folder.clone()), 80, 24);
    keys(&mut app, "s");
    assert_eq!(field_path(&app), folder.join("contract.pdf"));
    key(&mut app, KeyCode::Enter);
    let saved = folder.join("contract.pdf");
    assert_eq!(std::fs::read(&saved).expect("saved"), b"%PDF signed");
    assert!(
        status(&app).starts_with("Saved contract.pdf (11 B) to "),
        "{}",
        status(&app)
    );
    // Nothing partial is left beside it.
    assert_eq!(std::fs::read_dir(&folder).expect("listed").count(), 1);

    // Again: the file is there, so Zmail asks; n goes back to the path.
    std::fs::write(&saved, b"mine").expect("written");
    keys(&mut app, "s");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.prompt, Some(Prompt::ReplaceFile { .. })));
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("contract.pdf is already there. Replace it? (y/n)"),
        "{screen}"
    );
    assert!(screen.contains("Y Replace"), "{screen}");
    keys(&mut app, "n");
    assert!(matches!(app.prompt, Some(Prompt::Path(_))));
    assert_eq!(std::fs::read(&saved).expect("kept"), b"mine");
    // Another name.
    for _ in 0.."contract.pdf".len() {
        key(&mut app, KeyCode::Backspace);
    }
    keys(&mut app, "copy.pdf");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        std::fs::read(folder.join("copy.pdf")).expect("saved"),
        b"%PDF signed"
    );
    // y replaces.
    keys(&mut app, "s");
    assert_eq!(
        field_path(&app),
        folder.join("contract.pdf"),
        "the next save starts in the folder saved to"
    );
    key(&mut app, KeyCode::Enter);
    keys(&mut app, "y");
    assert_eq!(std::fs::read(&saved).expect("replaced"), b"%PDF signed");
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn a_hostile_name_is_saved_under_its_safe_name_and_tab_completes_folders() {
    let folder = scratch("tab");
    std::fs::create_dir(folder.join("invoices")).expect("a subfolder");
    let mut app = attachment_index(Some(folder.clone()), 80, 24);
    key(&mut app, KeyCode::Down);
    keys(&mut app, "s");
    let typed = field(&app);
    assert!(
        typed.ends_with(&format!("{MAIN_SEPARATOR}_]0;pwnedinvoicefdp.exe")),
        "no path, controls or bidi in the name: {typed:?}"
    );
    // Type a folder instead, with Tab, and save into it.
    let start = format!("{}{MAIN_SEPARATOR}inv", folder.display());
    if let Some(Prompt::Path(prompt)) = app.prompt.as_mut() {
        prompt.field.input = start;
    }
    key(&mut app, KeyCode::Tab);
    assert_eq!(
        field(&app),
        format!(
            "{}{MAIN_SEPARATOR}invoices{MAIN_SEPARATOR}",
            folder.display()
        )
    );
    key(&mut app, KeyCode::Enter);
    let saved = folder.join("invoices").join("_]0;pwnedinvoicefdp.exe");
    assert_eq!(std::fs::read(&saved).expect("saved"), b"MZ");
    assert_printable(&draw(&mut app, 80, 24));
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn save_all_numbers_taken_names_and_says_what_failed() {
    let folder = scratch("all");
    std::fs::write(folder.join("contract.pdf"), b"mine").expect("written");
    let mut app = attachment_index(Some(folder.clone()), 80, 24);
    keys(&mut app, "a");
    assert_eq!(field_path(&app), folder);
    let screen = text_of(&draw(&mut app, 160, 48));
    assert!(
        screen.contains("Save every attachment into the folder: "),
        "{screen}"
    );
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        std::fs::read(folder.join("contract.pdf")).expect("kept"),
        b"mine",
        "never replaced"
    );
    assert_eq!(
        std::fs::read(folder.join("contract (1).pdf")).expect("numbered"),
        b"%PDF signed"
    );
    let said = status(&app).to_owned();
    assert!(said.starts_with("Saved 2 of 3 to "), "{said}");
    assert!(said.contains("couldn't save notes.txt"), "{said}");
    // Nothing of the one that failed, and no temporary files.
    let mut names = std::fs::read_dir(&folder)
        .expect("listed")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .into_string()
                .expect("UTF-8")
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(
        !names
            .iter()
            .any(|name| name.contains("notes") || name.ends_with(".part"))
    );

    // A folder that isn't there.
    keys(&mut app, "a");
    if let Some(Prompt::Path(prompt)) = app.prompt.as_mut() {
        prompt.field.input = folder.join("nowhere").display().to_string();
    }
    key(&mut app, KeyCode::Enter);
    assert!(
        status(&app).starts_with("There is no folder "),
        "{}",
        status(&app)
    );
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn the_attachment_index_goes_back_to_message_text() {
    let mut app = attachment_index(None, 80, 24);
    keys(&mut app, "n");
    assert_eq!(app.attachments.as_ref().map(|list| list.cursor), Some(1));
    keys(&mut app, "<");
    assert_eq!(app.screen, Screen::Message);
    assert!(app.attachments.is_none());
}
