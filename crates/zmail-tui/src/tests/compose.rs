//! Writing mail: Compose, reply and forward, postponed drafts,
//! attachments, send with undo-send (with and without the sync lock, and
//! from a second process), and the Outbox, over the seeded store.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use zmail_core::{
    AccountSyncSnapshot, DraftState, OutboxJobState,
    reauth::{ReauthReason, ReauthRequired},
    time::HourCycle,
};
use zmail_store::LocalStore;

use super::{
    Mail, app, app_with, assert_printable, assert_snapshot, control, draw, environment, key, keys,
    mailbox, now, open_work_inbox, seed, seeded_store, text_of, thread,
    triage::{accounts, app_without_the_sync_lock, fixture_key},
};
use crate::{
    app::{App, AppOptions, Prompt, Screen},
    compose::{Confirm, Field},
};

fn status(app: &App) -> &str {
    app.status.as_deref().unwrap_or_default()
}

fn compose(app: &App) -> &crate::compose::Compose {
    app.compose.as_ref().expect("a message is being written")
}

/// The draft open on the Compose screen, as the store has it.
fn stored_draft(app: &App) -> zmail_core::DraftDetail {
    app.store
        .draft_detail(compose(app).draft_id)
        .expect("the draft loads")
        .expect("the draft exists")
}

/// Sets the undo-send delay, as the GUI's Settings does.
fn undo_delay(store: &LocalStore, seconds: u32) {
    store
        .update_ui_preferences(|preferences| preferences.undo_send_delay_secs = seconds)
        .expect("preferences saved");
}

fn paste(app: &mut App, text: &str) {
    app.handle_event(Event::Paste(text.to_owned()));
}

/// Types into the focused field.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        match ch {
            '\n' => key(app, KeyCode::Enter),
            ch => key(app, KeyCode::Char(ch)),
        }
    }
}

/// Starts a new message from the Main Menu and fills it in.
fn write_message(app: &mut App, to: &str, subject: &str, body: &str) {
    keys(app, "c");
    if app.prompt == Some(Prompt::Postponed) {
        // Not one of those: a new message.
        keys(app, "n");
    }
    assert_eq!(app.screen, Screen::Compose);
    assert_eq!(compose(app).focus, Field::To);
    type_text(app, to);
    key(app, KeyCode::Down); // Cc
    key(app, KeyCode::Down); // Bcc
    key(app, KeyCode::Down); // Attchmnt
    key(app, KeyCode::Down); // Subject
    type_text(app, subject);
    key(app, KeyCode::Enter);
    assert_eq!(compose(app).focus, Field::Body);
    type_text(app, body);
}

/// A scratch folder holding `files` of the given sizes.
fn scratch_files(name: &str, files: &[(&str, usize)]) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "zmail-tui-compose-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("the folder");
    for (file, size) in files {
        std::fs::write(directory.join(file), vec![b'x'; *size]).expect("the file");
    }
    directory
}

/// The Home account gets a conversation sent to several people, for
/// reply all.
fn add_team_thread(store: &LocalStore) {
    let (_, home) = accounts(store);
    let team = thread(
        "team",
        "Team dinner",
        false,
        &[Mail {
            sender: "Ada Lovelace <ada@home.example>",
            minutes_ago: 90,
            read: true,
            text: Some("Shall we book for Friday?\n\nAda"),
            html: None,
            attachments: &[],
        }],
    );
    let mut team = team;
    for message in &mut team.messages {
        message.recipients =
            "Me <me@home.example>, Bo Diddley <bo@home.example>, ada@home.example".to_owned();
    }
    let mut ferry = thread(
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
    );
    ferry.messages[0].recipients = "Me <me@home.example>".to_owned();
    store
        .replace_account_snapshot(
            home,
            &AccountSyncSnapshot {
                checkpoint: "test:2".to_owned(),
                detail: "Up to date".to_owned(),
                identities: None,
                mailboxes: vec![mailbox("INBOX", Some("inbox"), vec![ferry, team])],
            },
        )
        .expect("the Home mail is stored");
}

/// Opens the Home INBOX from the Folder List.
fn open_home_inbox(app: &mut App) {
    keys(app, "l");
    let buffer = draw(app, 80, 24);
    let row = text_of(&buffer)
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("INBOX"))
        .map(|(row, _)| row)
        .last()
        .expect("Home's INBOX") as u16;
    super::click(app, 6, row);
    draw(app, 80, 24);
}

#[test]
fn compose_screens_match_their_snapshots() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = app();
        keys(&mut app, "c");
        assert_eq!(app.screen, Screen::Compose);
        assert_snapshot(
            &format!("compose-empty-{width}x{height}"),
            &draw(&mut app, width, height),
        );
    }
}

#[test]
fn a_reply_quotes_the_message_read_and_goes_to_its_sender() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    key(&mut app, KeyCode::Enter);
    draw(&mut app, 80, 24);
    keys(&mut app, "r");
    assert_eq!(app.screen, Screen::Compose);
    let draft = stored_draft(&app);
    assert_eq!(draft.to, ["Grace Hopper <grace@work.example>"]);
    assert!(draft.cc.is_empty());
    assert_eq!(draft.subject, "Re: Launch plan for the terminal UI");
    assert!(
        draft.body.starts_with(
            "\n\nOn Thu 15 Oct 2026, 11:35, Grace Hopper <grace@work.example> wrote:\n> Looks good."
        ),
        "{:?}",
        draft.body
    );
    assert_eq!(compose(&app).focus, Field::Body);
    assert_eq!(
        compose(&app).body.cursor().0,
        0,
        "the cursor is above the quote"
    );
    assert_snapshot("compose-reply-80x24", &draw(&mut app, 80, 24));

    // ^C on an untouched reply deletes it without asking.
    let draft_id = draft.id;
    control(&mut app, 'c');
    assert_eq!(app.screen, Screen::Message);
    assert_eq!(status(&app), "Message cancelled");
    assert_eq!(app.store.draft_detail(draft_id).expect("loads"), None);
}

#[test]
fn reply_all_asks_inline_and_adds_the_other_recipients() {
    let store = seeded_store();
    add_team_thread(&store);
    let mut app = app_with(store, environment());
    open_home_inbox(&mut app);
    key(&mut app, KeyCode::Down);
    keys(&mut app, "r");
    assert!(matches!(app.prompt, Some(Prompt::ReplyAll { .. })));
    assert_snapshot("reply-all-question-80x24", &draw(&mut app, 80, 24));
    keys(&mut app, "y");
    let draft = stored_draft(&app);
    assert_eq!(draft.to, ["Ada Lovelace <ada@home.example>"]);
    assert_eq!(
        draft.cc,
        ["Bo Diddley <bo@home.example>"],
        "not me, not Ada twice"
    );
    control(&mut app, 'c');

    keys(&mut app, "r");
    keys(&mut app, "n");
    let draft = stored_draft(&app);
    assert_eq!(draft.to, ["Ada Lovelace <ada@home.example>"]);
    assert!(draft.cc.is_empty());
    control(&mut app, 'c');

    // ^C at the question writes nothing.
    keys(&mut app, "r");
    control(&mut app, 'c');
    assert!(app.compose.is_none());
    assert_eq!(status(&app), "Reply cancelled");

    // A message sent only to me asks nothing (the ferry booking).
    key(&mut app, KeyCode::Up);
    keys(&mut app, "r");
    assert!(app.prompt.is_none());
    assert_eq!(app.screen, Screen::Compose);
}

#[test]
fn a_forward_copies_the_message_and_starts_in_to() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    key(&mut app, KeyCode::Down);
    keys(&mut app, "f");
    let draft = stored_draft(&app);
    assert!(draft.to.is_empty());
    assert_eq!(draft.subject, "Fwd: Harbour newsletter, issue 42");
    assert!(
        draft
            .body
            .contains("---------- Forwarded message ----------")
    );
    assert!(
        draft
            .body
            .contains("From: Harbour News <news@harbour.example>")
    );
    assert!(draft.body.contains("Hello from the harbour newsletter."));
    assert_eq!(compose(&app).focus, Field::To);
}

#[test]
fn quoted_hostile_mail_is_filtered_before_it_enters_the_editor() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    key(&mut app, KeyCode::End);
    keys(&mut app, "r");
    let compose = compose(&app);
    for field in [Field::To, Field::Subject, Field::Body] {
        let text = compose.text(field);
        assert!(
            !text.chars().any(|ch| (ch.is_control() && ch != '\n')
                || matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')),
            "{field:?}: {text:?}"
        );
    }
    assert!(compose.text(Field::Body).contains("> Pay now!"));
    let draft = stored_draft(&app);
    assert!(!draft.body.contains('\x1b') && !draft.subject.contains('\x1b'));
    assert_printable(&draw(&mut app, 80, 24));
    assert_printable(&draw(&mut app, 160, 48));
}

#[test]
fn a_postponed_draft_round_trips_through_the_store() {
    let mut app = app();
    write_message(
        &mut app,
        "Ada <ada@example.com>, bo@example.com",
        "Lunch",
        "Friday?\nAt one.",
    );
    control(&mut app, 'o');
    assert_eq!(app.screen, Screen::MainMenu);
    assert_eq!(status(&app), "Message postponed. C continues it");
    let drafts = app.store.list_drafts(None).expect("drafts");
    assert_eq!(drafts.len(), 1);
    let draft = app
        .store
        .draft_detail(drafts[0].id)
        .expect("loads")
        .expect("exists");
    assert_eq!(draft.to, ["Ada <ada@example.com>", "bo@example.com"]);
    assert_eq!(draft.subject, "Lunch");
    assert_eq!(draft.body, "Friday?\nAt one.");
    assert_eq!(draft.state, DraftState::Editing);

    // C asks to continue it.
    keys(&mut app, "c");
    assert_eq!(app.prompt, Some(Prompt::Postponed));
    assert_snapshot("postponed-prompt-80x24", &draw(&mut app, 80, 24));
    keys(&mut app, "y");
    assert_eq!(app.screen, Screen::Compose);
    let compose_state = compose(&app);
    assert_eq!(compose_state.draft_id, draft.id);
    assert_eq!(
        compose_state.text(Field::To),
        "Ada <ada@example.com>, bo@example.com"
    );
    assert_eq!(compose_state.text(Field::Subject), "Lunch");
    assert_eq!(compose_state.text(Field::Body), "Friday?\nAt one.");
    assert!(!compose_state.changed());

    // An edit autosaves once typing pauses.
    type_text(&mut app, "Hi! ");
    assert!(compose(&app).dirty);
    app.tick();
    assert_eq!(stored_draft(&app).body, "Friday?\nAt one.", "not yet");
    app.compose.as_mut().expect("composing").last_edit =
        Instant::now() - zmail_actions::AUTOSAVE_DELAY;
    app.tick();
    assert!(!compose(&app).dirty);
    assert_eq!(stored_draft(&app).body, "Hi! Friday?\nAt one.");

    // ^C on a changed draft asks; n keeps writing.
    control(&mut app, 'c');
    assert_eq!(app.prompt, Some(Prompt::Confirm(Confirm::Cancel)));
    keys(&mut app, "n");
    assert_eq!(app.screen, Screen::Compose);
    // Unchanged since it was continued, ^C leaves it postponed.
    control(&mut app, 'o');
    keys(&mut app, "cy");
    control(&mut app, 'c');
    assert!(app.compose.is_none());
    assert_eq!(app.store.list_drafts(None).expect("drafts").len(), 1);

    // N at the question writes a new message instead.
    keys(&mut app, "cn");
    assert_ne!(compose(&app).draft_id, draft.id);
    type_text(&mut app, "x@example.com");
    control(&mut app, 'c');
    keys(&mut app, "y");
    assert_eq!(status(&app), "Message cancelled");
    assert_eq!(app.store.list_drafts(None).expect("drafts").len(), 1);
}

#[test]
fn postponed_messages_list_continue_and_delete() {
    let mut app = app();
    write_message(&mut app, "a@example.com", "First", "One");
    control(&mut app, 'o');
    write_message(&mut app, "b@example.com", "Second", "Two");
    control(&mut app, 'o');
    keys(&mut app, "cy");
    assert_eq!(app.screen, Screen::Postponed);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("POSTPONED MESSAGES")
            && screen.contains("First")
            && screen.contains("Second"),
        "{screen}"
    );
    // The newest first: delete it.
    keys(&mut app, "d");
    assert!(matches!(
        app.prompt,
        Some(Prompt::Confirm(Confirm::DeleteDraft(_)))
    ));
    keys(&mut app, "y");
    assert_eq!(app.postponed.len(), 1);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Compose);
    assert_eq!(compose(&app).text(Field::Subject), "First");
    // Folder List lists them under Local too.
    control(&mut app, 'o');
    keys(&mut app, "l");
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("Postponed") && screen.contains("Outbox"),
        "{screen}"
    );
}

#[test]
fn attachments_are_added_by_path_or_browser_shown_and_removed() {
    let files = scratch_files(
        "attach",
        &[("report.pdf", 12 * 1024), ("photo.jpg", 3 * 1024 * 1024)],
    );
    let mut app = app();
    keys(&mut app, "c");
    type_text(&mut app, "ada@example.com");
    // ^J, then the path with Tab completion.
    control(&mut app, 'j');
    let typed = format!("{}{}rep", files.display(), std::path::MAIN_SEPARATOR);
    paste(&mut app, &typed);
    key(&mut app, KeyCode::Tab);
    match &app.prompt {
        Some(Prompt::Path(prompt)) => assert!(prompt.field.input.ends_with("report.pdf")),
        other => panic!("the file prompt: {other:?}"),
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(status(&app), "Attached report.pdf (12 KB)");
    // ^T on Attchmnt: the file browser.
    control(&mut app, 't');
    let picker = compose(&app).picker.clone().expect("the browser shows");
    let photo = picker
        .entries
        .iter()
        .position(|entry| entry.label == "photo.jpg")
        .expect("listed");
    for _ in 0..photo {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(status(&app), "Attached photo.jpg (3.0 MB)");
    let stored = stored_draft(&app);
    assert_eq!(stored.attachments.len(), 2);
    // The store keeps an encrypted copy, not the file.
    assert!(
        stored
            .attachments
            .iter()
            .all(|attachment| !Path::new(&attachment.storage_path).starts_with(&files))
    );
    key(&mut app, KeyCode::Down); // Subject
    type_text(&mut app, "Files");
    app.status = None;
    assert_snapshot("compose-attachments-80x24", &draw(&mut app, 80, 24));

    // ^K on the attachment under the cursor removes it.
    key(&mut app, KeyCode::Up);
    assert_eq!(compose(&app).focus, Field::Attachments);
    control(&mut app, 'k');
    assert_eq!(status(&app), "Removed photo.jpg (3.0 MB)");
    assert_eq!(stored_draft(&app).attachments.len(), 1);
    let _ = std::fs::remove_dir_all(files);
}

#[test]
fn attachment_names_are_filtered() {
    let mut app = app();
    keys(&mut app, "c");
    let draft_id = compose(&app).draft_id;
    // A file named with escape sequences (allowed on Unix), attached by
    // another frontend.
    let files = scratch_files("hostile", &[("plain.txt", 10)]);
    let attachment = app
        .store
        .add_draft_attachment(draft_id, files.join("plain.txt"))
        .expect("attached");
    let hostile = zmail_core::DraftAttachment {
        filename: "evil\x1b]0;pwned\x07\u{202E}fdp.exe".to_owned(),
        ..attachment
    };
    let label = crate::compose::attachment_label(&hostile);
    assert!(
        !label.chars().any(char::is_control) && !label.contains('\u{202E}'),
        "{label:?}"
    );
    assert!(
        !label.contains("pwned"),
        "the title sequence went whole: {label:?}"
    );
    assert!(
        label.starts_with("evil") && label.ends_with("(10 B)"),
        "{label:?}"
    );
    let _ = std::fs::remove_dir_all(files);
}

#[test]
fn tab_completes_addresses_from_the_history_and_paste_splits_lists() {
    let mut app = app();
    keys(&mut app, "c");
    type_text(&mut app, "gra");
    key(&mut app, KeyCode::Tab);
    assert_eq!(
        compose(&app).text(Field::To),
        "Grace Hopper <grace@work.example>, "
    );
    // Several match: the first, then Tab cycles.
    type_text(&mut app, "harbour");
    key(&mut app, KeyCode::Tab);
    let first = compose(&app).text(Field::To);
    let completion = compose(&app).completion.clone().expect("matches show");
    assert!(completion.matches.len() > 1, "{completion:?}");
    key(&mut app, KeyCode::Tab);
    assert_ne!(compose(&app).text(Field::To), first);
    assert!(
        compose(&app)
            .text(Field::To)
            .starts_with("Grace Hopper <grace@work.example>, ")
    );

    // A pasted list, one address per line, becomes a comma list.
    key(&mut app, KeyCode::Down); // Cc
    paste(
        &mut app,
        "a@example.com\r\nb@example.com\n\x1b[31mc@example.com\n",
    );
    assert_eq!(
        compose(&app).text(Field::Cc),
        "a@example.com, b@example.com, c@example.com"
    );
    // Into Subject on one line, into the body as it is.
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    paste(&mut app, "One\ntwo");
    assert_eq!(compose(&app).text(Field::Subject), "One two");
    key(&mut app, KeyCode::Down);
    paste(&mut app, "Line one\nline \x1b[2Jtwo");
    assert_eq!(compose(&app).text(Field::Body), "Line one\nline two");
}

#[test]
fn cut_and_uncut_lines_and_where_is() {
    let mut app = app();
    write_message(&mut app, "a@example.com", "Cut", "one\ntwo\nthree");
    // To the top of the body, then cut two lines and put them back below.
    for _ in 0..2 {
        key(&mut app, KeyCode::Up);
    }
    control(&mut app, 'k');
    control(&mut app, 'k');
    assert_eq!(compose(&app).text(Field::Body), "three");
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Enter);
    control(&mut app, 'u');
    assert_eq!(compose(&app).text(Field::Body), "three\none\ntwo\n");
    // ^W finds text in the body.
    control(&mut app, 'w');
    paste(&mut app, "TWO");
    key(&mut app, KeyCode::Enter);
    assert_eq!(compose(&app).body.cursor().0, 2);
    control(&mut app, 'w');
    paste(&mut app, "nowhere");
    key(&mut app, KeyCode::Enter);
    assert_eq!(status(&app), "\"nowhere\" isn't in the text");
}

#[test]
fn sending_checks_the_recipients() {
    let mut app = app();
    write_message(&mut app, "", "No one", "Hello");
    control(&mut app, 'x');
    keys(&mut app, "y");
    assert_eq!(status(&app), "Add a recipient in To, Cc or Bcc first");
    assert_eq!(compose(&app).focus, Field::To);
    type_text(&mut app, "ada@example.com, bob");
    control(&mut app, 'x');
    keys(&mut app, "y");
    assert_eq!(status(&app), "Not an address Zmail can send to: bob");
    assert_eq!(app.screen, Screen::Compose);
    control(&mut app, 'x');
    keys(&mut app, "n");
    assert_eq!(status(&app), "Not sent");
}

#[test]
fn send_queues_after_the_undo_delay_and_u_takes_it_back() {
    let store = seeded_store();
    undo_delay(&store, 10);
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, "c");
    type_text(&mut app, "ada@example.com");
    key(&mut app, KeyCode::Down);
    control(&mut app, 'x');
    assert_eq!(app.prompt, Some(Prompt::Confirm(Confirm::Send)));
    keys(&mut app, "y");
    assert_eq!(app.screen, Screen::Index, "back where compose started");
    assert_eq!(
        status(&app),
        "Sending in 10 seconds (nothing sends mail from here). U Undo"
    );
    let jobs = store.list_outbox_jobs(None).expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, OutboxJobState::Queued);
    let draft_id = jobs[0].draft_id;
    assert_eq!(
        store
            .draft_detail(draft_id)
            .expect("loads")
            .expect("exists")
            .state,
        DraftState::Queued
    );
    // U takes it back and opens it again.
    keys(&mut app, "u");
    assert_eq!(status(&app), "Sending undone");
    assert_eq!(app.screen, Screen::Compose);
    assert_eq!(compose(&app).draft_id, draft_id);
    assert!(store.list_outbox_jobs(None).expect("jobs").is_empty());
    assert_eq!(
        store
            .draft_detail(draft_id)
            .expect("loads")
            .expect("exists")
            .state,
        DraftState::Editing
    );
    // Sent again, the offer ends with the delay.
    control(&mut app, 'x');
    keys(&mut app, "y");
    assert!(app.can_undo_send());
    app.pending_send.as_mut().expect("pending").until = Instant::now();
    app.tick();
    assert!(!app.can_undo_send());
    keys(&mut app, "u");
    assert_eq!(store.list_outbox_jobs(None).expect("jobs").len(), 1);
}

#[test]
fn without_the_sync_lock_send_waits_in_the_outbox_for_the_zmail_that_syncs() {
    let store = seeded_store();
    undo_delay(&store, 10);
    let held = store.try_sync_lock().expect("the lock file opens");
    assert!(held.is_some());
    let (mut app, _runtime) = app_without_the_sync_lock(store.clone());
    write_message(&mut app, "ada@example.com", "Queued", "For the holder");
    control(&mut app, 'x');
    keys(&mut app, "y");
    assert_eq!(
        status(&app),
        "Sending in 10 seconds (the Zmail that syncs sends it). U Undo"
    );
    let jobs = store.list_outbox_jobs(None).expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, OutboxJobState::Queued);
    keys(&mut app, "u");
    assert!(store.list_outbox_jobs(None).expect("jobs").is_empty());
}

#[test]
fn holding_the_sync_lock_it_sends_and_the_outbox_retries_a_failure() {
    let store = seeded_store();
    undo_delay(&store, 0);
    let runtime = zmail_sync::BackgroundRuntime::start(1).expect("a runtime");
    let sync = zmail_sync::SyncService::with_sync_lock(
        runtime.handle().clone(),
        store.clone(),
        zmail_security::InMemorySecretStore::shared(),
    )
    .expect("the service starts");
    assert!(sync.runs_sync(), "this process syncs");
    let mut app = App::new(AppOptions {
        store: store.clone(),
        sync: Some(sync),
        runtime: Some(runtime.handle().clone()),
        env: environment(),
        clock: Arc::new(now),
        hour_cycle: HourCycle::H24,
        version: "0.0.0-test".to_owned(),
        arrivals: None,
    });
    write_message(&mut app, "ada@example.com", "Now", "Straight away");
    control(&mut app, 'x');
    keys(&mut app, "y");
    assert_eq!(status(&app), "Message queued to send");
    // This process's outbox pass tries it at once; with no credentials it
    // fails.
    let deadline = Instant::now() + Duration::from_secs(20);
    let failed = loop {
        let jobs = store.list_outbox_jobs(None).expect("jobs");
        if jobs[0].attempt_count > 0
            && matches!(
                jobs[0].state,
                OutboxJobState::Failed | OutboxJobState::Retrying
            )
        {
            break jobs[0].clone();
        }
        assert!(Instant::now() < deadline, "the pass ran: {jobs:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    keys(&mut app, "mt");
    assert_eq!(app.screen, Screen::Outbox);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("Couldn\u{2019}t send") || screen.contains("Failed"),
        "{screen}"
    );
    keys(&mut app, "r");
    assert_eq!(status(&app), "Queued for another send attempt");
    let retried = store.list_outbox_jobs(None).expect("jobs");
    assert!(
        retried[0].state != failed.state || retried[0].attempt_count > failed.attempt_count,
        "{retried:?}"
    );
    drop(app);
    drop(runtime);
}

#[test]
fn the_outbox_shows_queued_failed_and_refused_messages() {
    let store = seeded_store();
    let (work, _) = accounts(&store);
    let identity = store.list_identities(work).expect("identities")[0].id;
    let queue = |subject: &str, to: &str| {
        let draft_id = store
            .create_draft(&zmail_core::NewDraft {
                account_id: work,
                identity_id: identity,
                to: vec![to.to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject.to_owned(),
                body: "Body".to_owned(),
            })
            .expect("draft");
        store.queue_draft(draft_id).expect("queued")
    };
    let failed = queue("Failed", "bo@example.com");
    let refused = queue("Refused", "cy@example.com");
    // A send attempt claims both.
    while store.claim_due_outbox_job(None).expect("claim").is_some() {}
    let waiting = queue("Waiting", "ada@example.com");
    store
        .record_outbox_failure(failed, "550 mailbox \x1b[31munavailable")
        .expect("failure");
    store
        .record_outbox_failure(
            refused,
            ReauthRequired::new(ReauthReason::SendRefused, "535 auth").to_string(),
        )
        .expect("refusal");
    let mut app = app_with(store.clone(), environment());
    keys(&mut app, "t");
    assert_eq!(app.screen, Screen::Outbox);
    assert_eq!(app.outbox.len(), 3);
    let buffer = draw(&mut app, 80, 24);
    assert_printable(&buffer);
    assert_snapshot("outbox-80x24", &buffer);

    let row = |app: &App, subject: &str| {
        app.outbox
            .iter()
            .position(|row| row.subject == subject)
            .expect("listed")
    };
    // A refused message waits for a sign-in: no retry here.
    app.outbox_cursor = row(&app, "Refused");
    keys(&mut app, "r");
    assert!(
        status(&app).contains("nothing to retry"),
        "{}",
        status(&app)
    );
    // R sends a failed one again.
    app.outbox_cursor = row(&app, "Failed");
    keys(&mut app, "r");
    let job = |id| {
        store
            .list_outbox_jobs(None)
            .expect("jobs")
            .into_iter()
            .find(|job| job.id == id)
    };
    assert_eq!(
        job(failed).expect("still there").state,
        OutboxJobState::Queued
    );
    // D takes back one that hasn't started sending: it is postponed again.
    app.outbox_cursor = row(&app, "Waiting");
    keys(&mut app, "d");
    assert_eq!(job(waiting), None);
    assert!(status(&app).starts_with("Taken back"));
    assert_eq!(app.postponed_drafts().len(), 1);
}

/// Set in the child process that
/// `a_second_process_sends_while_the_first_holds_the_sync_lock` starts:
/// the store it opens.
const CHILD_STORE: &str = "ZMAIL_TUI_SEND_CHILD_STORE";

#[test]
fn a_second_process_sends_while_the_first_holds_the_sync_lock() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("zmail-tui-send-{}-{nanos}", std::process::id()));
    let path = directory.join("store.sqlite3");
    let store = LocalStore::open(&path, fixture_key()).expect("the store opens");
    seed(&store);
    undo_delay(&store, 0);
    // This process stands in for the GUI: it holds the sync lock.
    let held = store
        .try_sync_lock()
        .expect("the lock file opens")
        .expect("the lock is free");

    let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", "tests::compose::send_child", "--nocapture"])
        .env(CHILD_STORE, &path)
        .output()
        .expect("the child runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "child failed: {stdout}");
    assert!(stdout.contains("child: sync lock refused"), "{stdout}");
    assert!(
        stdout.contains("child: [Message queued to send (the Zmail that syncs sends it)]"),
        "{stdout}"
    );

    // The message waits in the outbox of the store this process syncs.
    let jobs = store.list_outbox_jobs(None).expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, OutboxJobState::Queued);
    let draft = store
        .draft_detail(jobs[0].draft_id)
        .expect("loads")
        .expect("exists");
    assert_eq!(draft.to, ["ada@example.com"]);
    assert_eq!(draft.subject, "From the terminal");
    assert_eq!(draft.state, DraftState::Queued);
    drop(held);
    drop(store);
    let _ = std::fs::remove_dir_all(directory);
}

/// The child half of
/// `a_second_process_sends_while_the_first_holds_the_sync_lock`: opens the
/// store, finds the sync lock taken, writes a message and sends it. Does
/// nothing in a normal test run.
#[test]
fn send_child() {
    let Some(path) = std::env::var_os(CHILD_STORE) else {
        return;
    };
    let store = Arc::new(LocalStore::open(&path, fixture_key()).expect("the store opens"));
    let (mut app, _runtime) = app_without_the_sync_lock(store);
    println!("child: sync lock refused");
    write_message(&mut app, "ada@example.com", "From the terminal", "Hello");
    app.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char('x'),
        KeyModifiers::CONTROL,
    )));
    keys(&mut app, "y");
    println!("child: [{}]", status(&app));
}

#[test]
fn from_offers_the_accounts_identities() {
    let store = seeded_store();
    let (work, _) = accounts(&store);
    let work_snapshot = AccountSyncSnapshot {
        checkpoint: "test:3".to_owned(),
        detail: "Up to date".to_owned(),
        identities: Some(vec![
            zmail_core::NewIdentity {
                display_name: "Work".to_owned(),
                email_address: "me@work.example".to_owned(),
                signature: String::new(),
                is_primary: true,
            },
            zmail_core::NewIdentity {
                display_name: "Support\x1b[2J".to_owned(),
                email_address: "support@work.example".to_owned(),
                signature: String::new(),
                is_primary: false,
            },
        ]),
        mailboxes: vec![mailbox("INBOX", Some("inbox"), Vec::new())],
    };
    store
        .replace_account_snapshot(work, &work_snapshot)
        .expect("identities stored");
    let mut app = app_with(store, environment());
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, "c");
    assert_eq!(compose(&app).fields()[0], Field::From);
    key(&mut app, KeyCode::Up);
    assert_eq!(compose(&app).focus, Field::From);
    let before = compose(&app).identity_id;
    key(&mut app, KeyCode::Char(' '));
    assert_ne!(compose(&app).identity_id, before);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("From    : Support <support@work.example>"),
        "{screen}"
    );
    assert_printable(&draw(&mut app, 80, 24));
    // Saved with the draft.
    control(&mut app, 'o');
    let drafts = app.store.list_drafts(None).expect("drafts");
    assert_ne!(drafts[0].identity_id, before);
}

#[test]
fn a_new_message_leaves_room_above_the_signature() {
    let store = seeded_store();
    let (work, _) = accounts(&store);
    store
        .replace_account_snapshot(
            work,
            &AccountSyncSnapshot {
                checkpoint: "test:4".to_owned(),
                detail: "Up to date".to_owned(),
                identities: Some(vec![zmail_core::NewIdentity {
                    display_name: "Work".to_owned(),
                    email_address: "me@work.example".to_owned(),
                    signature: "Ada\nWork".to_owned(),
                    is_primary: true,
                }]),
                mailboxes: vec![mailbox("INBOX", Some("inbox"), Vec::new())],
            },
        )
        .expect("the signature is stored");
    let mut app = app_with(store, environment());
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, "c");
    assert_eq!(compose(&app).text(Field::Body), "\n\nAda\nWork");
    assert!(!compose(&app).changed());
    // Untouched, it is cancelled without asking.
    let draft_id = compose(&app).draft_id;
    control(&mut app, 'c');
    assert_eq!(app.store.draft_detail(draft_id).expect("loads"), None);
}
