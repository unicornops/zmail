//! The user's keymap file: per-screen rebinding, the user's keys on the
//! command bar, the Main Menu, Help and the palette, and a broken file
//! reported and ignored.

use std::path::PathBuf;

use crossterm::event::KeyCode;

use super::{
    app, app_with, assert_snapshot, control, draw, environment, key, keys, open_work_inbox,
    text_of,
    warnings::{open, store_with_suspicious_mail},
};
use crate::{
    app::{App, Prompt, Screen},
    keymap::UserKeymap,
};

/// Where the tests' keymap is said to be: a fixed path, for snapshots.
const PATH: &str = "/home/ada/.local/share/zmail/tui-keymap.json";

const KEYMAP: &str = r#"[
    { "context": "MessageIndex", "bindings": { "x": "Delete", "d": null } },
    { "context": "MainMenu", "bindings": { "w": "Compose" } },
    { "bindings": { "ctrl-p": "Palette" } }
]"#;

fn keymap(source: &str) -> UserKeymap {
    UserKeymap {
        path: Some(PathBuf::from(PATH)),
        ..UserKeymap::parse(source)
    }
}

fn status(app: &App) -> &str {
    app.status.as_deref().unwrap_or_default()
}

#[test]
fn the_users_keys_rebind_a_screen_and_show_on_its_bar() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = app();
        app.set_keymap(keymap(KEYMAP));
        let buffer = open_work_inbox(&mut app, width, height);
        let screen = text_of(&buffer);
        assert!(screen.contains("X Delete"), "{screen}");
        assert!(!screen.contains("D Delete"), "{screen}");
        assert_snapshot(&format!("keymap-index-{width}x{height}"), &buffer);
    }
    let mut app = app();
    app.set_keymap(keymap(KEYMAP));
    open_work_inbox(&mut app, 80, 24);
    let before = app.index.entries.len();
    // D does nothing now…
    keys(&mut app, "d");
    assert_eq!(app.index.entries.len(), before);
    assert_eq!(status(&app), "");
    // …and X deletes, in either case.
    keys(&mut app, "X");
    assert_eq!(app.index.entries.len(), before - 1);
    assert_eq!(status(&app), "Conversation moved to Trash. U Undo");
    // A built-in key the file doesn't touch still works.
    keys(&mut app, "u");
    assert_eq!(app.index.entries.len(), before);
    // Elsewhere, D is Zmail's again: Message Text deletes with it.
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Message);
    assert!(
        app.commands()
            .iter()
            .any(|entry| entry.key == "D" && entry.label == "Delete")
    );
}

#[test]
fn the_palette_help_and_main_menu_show_the_users_keys() {
    let mut app = app();
    app.set_keymap(keymap(KEYMAP));
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("W     COMPOSE MESSAGE"), "{screen}");
    keys(&mut app, "w");
    assert_eq!(app.screen, Screen::Compose, "W composes from the Main Menu");
    control(&mut app, 'c');

    let mut app = app_with(super::seeded_store(), environment());
    app.set_keymap(keymap(KEYMAP));
    open_work_inbox(&mut app, 80, 24);
    // Ctrl-P, bound on every screen, opens the palette.
    control(&mut app, 'p');
    let Some(Prompt::Palette(palette)) = &app.prompt else {
        panic!("the palette is open");
    };
    let delete = palette
        .items
        .iter()
        .find(|item| item.entry.label == "Delete")
        .expect("Delete is listed");
    assert_eq!(delete.entry.key, "X");
    key(&mut app, KeyCode::Esc);

    keys(&mut app, "?");
    for (width, height) in [(80, 24), (160, 48)] {
        let buffer = draw(&mut app, width, height);
        let screen = text_of(&buffer);
        assert!(screen.contains("Your keys"), "{screen}");
        assert!(screen.contains("tui-keymap.json"), "{screen}");
        assert_snapshot(&format!("help-keymap-{width}x{height}"), &buffer);
    }
}

#[test]
fn a_broken_file_is_reported_and_zmails_keys_stay() {
    let mut app = app();
    app.set_keymap(keymap(
        r#"[{ "context": "MessageIndex", "bindings": { "x": "Delete", "q": "Explode" } }]"#,
    ));
    assert!(
        status(&app).starts_with(
            "Your keymap file isn't used: section 1, \"q\": no command is called \"Explode\". \
             Help (?) says where it is"
        ),
        "{}",
        status(&app)
    );
    open_work_inbox(&mut app, 80, 24);
    let before = app.index.entries.len();
    keys(&mut app, "x");
    assert_eq!(
        app.index.entries.len(),
        before,
        "nothing of the file applies"
    );
    keys(&mut app, "d");
    assert_eq!(app.index.entries.len(), before - 1);
    keys(&mut app, "?");
    let screen = text_of(&draw(&mut app, 160, 48));
    assert!(screen.contains("It isn't used: section 1"), "{screen}");
}

#[test]
fn a_command_that_doesnt_work_on_the_screen_says_so() {
    let mut app = app();
    app.set_keymap(keymap(
        r#"[{ "context": "Setup", "bindings": { "x": "Delete", "g": "FolderList" } }]"#,
    ));
    keys(&mut app, "s");
    keys(&mut app, "x");
    assert_eq!(app.screen, Screen::Setup);
    assert_eq!(status(&app), "Delete doesn't work on this screen");
    // A command a built-in key runs there works, though not on the bar.
    keys(&mut app, "g");
    assert_eq!(app.screen, Screen::FolderList);
}

#[test]
fn a_file_on_disk_is_read_and_no_file_means_zmails_keys() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let folder =
        std::env::temp_dir().join(format!("zmail-tui-keymap-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("the folder is made");
    let path = folder.join(crate::KEYMAP_FILE);
    let none = UserKeymap::load(&path);
    assert_eq!(none.error, None);
    assert!(none.bindings.is_empty());
    std::fs::write(&path, KEYMAP).expect("written");
    let loaded = UserKeymap::load(&path);
    assert_eq!(loaded.error, None);
    assert_eq!(loaded.bindings.len(), 4);
    assert_eq!(loaded.path.as_deref(), Some(path.as_path()));
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn looks_safe_follows_its_key_or_points_to_the_palette() {
    let mut app = app_with(store_with_suspicious_mail(), environment());
    app.set_keymap(keymap(
        r#"[{ "context": "MessageText", "bindings": { "x": null, "ctrl-s": "LooksSafe" } }]"#,
    ));
    open(&mut app, 0, 80, 24);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("^S Looks safe: hide this warning"),
        "{screen}"
    );
    assert!(screen.contains("^S LooksSafe"), "{screen}");
    // X does nothing there now.
    keys(&mut app, "x");
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("Possible impersonation"), "{screen}");
    control(&mut app, 's');
    assert_eq!(status(&app), "Warning hidden for this message");

    let mut app = app_with(store_with_suspicious_mail(), environment());
    app.set_keymap(keymap(
        r#"[{ "context": "MessageText", "bindings": { "x": "Archive" } }]"#,
    ));
    open(&mut app, 0, 80, 24);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("Looks safe, in the command palette (:), hides this"),
        "{screen}"
    );
    assert!(!screen.contains("LooksSafe"), "not on the bar: {screen}");
}
