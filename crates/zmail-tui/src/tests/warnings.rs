//! Sender-safety warnings in Message Text: the desktop app's words above
//! the message text, filtered, at the desktop app's warning level, and
//! "Looks safe" (`X`) hiding one for its message.

use std::sync::Arc;

use crossterm::event::KeyCode;
use zmail_core::{AccountSyncSnapshot, preferences::SenderWarnings};
use zmail_store::LocalStore;

use super::{
    account, app_with, assert_printable, assert_snapshot, draw, environment, key, keys, mailbox,
    text_mail, text_of, thread,
};
use crate::app::{App, Screen};

/// A store whose Work inbox has a look-alike PayPal message whose
/// Reply-To also goes elsewhere (with an escape sequence in it), a
/// newsletter replying to a related domain, and an ordinary message.
fn store_with_suspicious_mail() -> Arc<LocalStore> {
    let store = Arc::new(LocalStore::open_ephemeral().expect("the store opens"));
    let work = account(&store, "Work", "me@work.example");
    let mut lookalike = thread(
        "paypal",
        "Confirm your payment details",
        false,
        &[text_mail(
            "PayPal Security <security@paypa1.com>",
            10,
            false,
            "Your account is limited. Confirm your details today.",
        )],
    );
    lookalike.messages[0].reply_to =
        Some("Billing \x1b]0;pwned\x07<collect@evil.example>".to_owned());
    let mut related = thread(
        "news",
        "Harbour news",
        false,
        &[text_mail(
            "Harbour News <news@harbour.example>",
            60,
            true,
            "This week on the harbour.",
        )],
    );
    related.messages[0].reply_to = Some("Letters <letters@mail.harbour.example>".to_owned());
    let ordinary = thread(
        "lunch",
        "Lunch?",
        false,
        &[text_mail("Ada <ada@work.example>", 120, true, "Noon?")],
    );
    store
        .replace_account_snapshot(
            work,
            &AccountSyncSnapshot {
                checkpoint: "test:1".to_owned(),
                detail: "Up to date".to_owned(),
                identities: None,
                mailboxes: vec![mailbox(
                    "INBOX",
                    Some("inbox"),
                    vec![lookalike, related, ordinary],
                )],
            },
        )
        .expect("the mail is stored");
    store
}

/// Opens the `row`th conversation of the Work inbox in Message Text.
fn open(app: &mut App, row: usize, width: u16, height: u16) {
    keys(app, "l");
    draw(app, width, height);
    key(app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Index);
    for _ in 0..row {
        key(app, KeyCode::Down);
    }
    draw(app, width, height);
    key(app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Message);
}

fn status(app: &App) -> &str {
    app.status.as_deref().unwrap_or_default()
}

#[test]
fn a_lookalike_sender_is_warned_about_in_the_desktop_apps_words() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = app_with(store_with_suspicious_mail(), environment());
        open(&mut app, 0, width, height);
        let buffer = draw(&mut app, width, height);
        assert_printable(&buffer);
        let screen = text_of(&buffer);
        assert!(
            screen.contains("! Possible impersonation: The sender's domain imitates paypal.com"),
            "{screen}"
        );
        assert!(screen.contains("Looks like: paypal.com"), "{screen}");
        assert!(
            screen.contains("Replies go to (Reply-To): collect@evil.example"),
            "{screen}"
        );
        assert!(!screen.contains("pwned"), "{screen}");
        assert!(
            screen.contains("X Looks safe: hide this warning"),
            "{screen}"
        );
        assert!(
            screen.contains("X LooksSafe"),
            "on the first page: {screen}"
        );
        assert_snapshot(&format!("sender-warning-{width}x{height}"), &buffer);
    }
}

#[test]
fn looks_safe_hides_the_warning_for_that_message_only() {
    let store = store_with_suspicious_mail();
    let mut app = app_with(store.clone(), environment());
    open(&mut app, 0, 80, 24);
    draw(&mut app, 80, 24);
    let message_id = app
        .message
        .as_ref()
        .expect("a message")
        .conversation
        .warnings[0]
        .0;
    keys(&mut app, "x");
    assert_eq!(status(&app), "Warning hidden for this message");
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(!screen.contains("Possible impersonation"), "{screen}");
    assert!(!screen.contains("LooksSafe"), "{screen}");
    let view = store
        .thread_view(app.message.as_ref().expect("open").conversation.thread_id)
        .expect("read")
        .expect("there");
    assert!(
        view.messages
            .iter()
            .any(|message| message.id == message_id && message.sender_warning_dismissed),
        "kept with the message, as the desktop app's Looks safe is"
    );
    // X does nothing more now.
    keys(&mut app, "x");
    assert_eq!(status(&app), "");

    // The related-domain newsletter still has its own.
    keys(&mut app, "<");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("! Replies go to a related address: Replies go to mail.harbour.example"),
        "{screen}"
    );
}

#[test]
fn fewer_warnings_leaves_out_related_addresses_only() {
    let store = store_with_suspicious_mail();
    store
        .update_ui_preferences(|preferences| preferences.sender_warnings = SenderWarnings::Fewer)
        .expect("saved");
    let mut app = app_with(store, environment());
    open(&mut app, 1, 80, 24);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(!screen.contains("related address"), "{screen}");
    assert!(
        !app.commands()
            .iter()
            .any(|entry| entry.label == "LooksSafe")
    );
    keys(&mut app, "<");
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Enter);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("Possible impersonation"), "{screen}");
}

#[test]
fn an_ordinary_sender_gets_no_warning_and_x_does_nothing() {
    let mut app = app_with(store_with_suspicious_mail(), environment());
    open(&mut app, 2, 80, 24);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(!screen.contains("! "), "{screen}");
    keys(&mut app, "x");
    assert_eq!(app.screen, Screen::Message);
    assert_eq!(status(&app), "");
}

#[test]
fn the_preview_pane_shows_the_warning_without_the_key() {
    let mut app = app_with(store_with_suspicious_mail(), environment());
    keys(&mut app, "l");
    draw(&mut app, 160, 48);
    key(&mut app, KeyCode::Enter);
    let screen = text_of(&draw(&mut app, 160, 48));
    assert!(screen.contains("! Possible impersonation"), "{screen}");
    assert!(!screen.contains("Looks safe: hide"), "{screen}");
}
