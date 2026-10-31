//! The terminal UI on ratatui's `TestBackend`: a snapshot of every screen
//! at 80x24 and 160x48 (in `src/snapshots`), and the keys, mouse, copy,
//! paste, links, sync and new-mail behaviour, over an ephemeral store
//! seeded with synthetic mail and a fixed clock.
//!
//! `ZMAIL_UPDATE_SNAPSHOTS=1 cargo test -p zmail-tui` rewrites the
//! snapshots; review the diff.

use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
};

use chrono::{DateTime, FixedOffset};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};
use zmail_core::{
    AccountSyncSnapshot, AuthKind, CachePolicySummary, MailboxSnapshot, MessageAttachment,
    MessageSnapshot, NewAccount, NewIdentity, ProtocolKind, ThreadSnapshot,
    preferences::TerminalPreferences, time::HourCycle,
};
use zmail_notify::{NewMailArrival, NewMailBatch};
use zmail_store::LocalStore;

use crate::{
    app::{App, AppOptions, Effect, Environment, Screen},
    text, view,
};

/// Thursday 15 October 2026, 12:00 UTC.
const NOW: &str = "2026-10-15T12:00:00+00:00";

fn now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(NOW).expect("a valid time")
}

/// `minutes` before [`NOW`], as the store keeps times.
fn ago(minutes: i64) -> String {
    (now() - chrono::Duration::minutes(minutes)).to_rfc3339()
}

struct Mail {
    sender: &'static str,
    minutes_ago: i64,
    read: bool,
    text: Option<&'static str>,
    html: Option<&'static str>,
    attachments: &'static [&'static str],
}

const fn text_mail(sender: &'static str, minutes_ago: i64, read: bool, text: &'static str) -> Mail {
    Mail {
        sender,
        minutes_ago,
        read,
        text: Some(text),
        html: None,
        attachments: &[],
    }
}

fn thread(id: &str, subject: &str, flagged: bool, mails: &[Mail]) -> ThreadSnapshot {
    let messages: Vec<MessageSnapshot> = mails
        .iter()
        .enumerate()
        .map(|(index, mail)| MessageSnapshot {
            remote_id: format!("{id}-{index}"),
            identity_email: "me@work.example".to_owned(),
            sender: mail.sender.to_owned(),
            reply_to: None,
            recipients: "Me <me@work.example>".to_owned(),
            subject: subject.to_owned(),
            body_preview: mail
                .text
                .unwrap_or("Preview of the HTML message")
                .chars()
                .take(80)
                .collect(),
            body_text: mail.text.map(str::to_owned),
            sanitized_html: mail
                .html
                .map(|html| zmail_security::sanitize_html_for_display(html).html),
            raw_html: mail.html.map(str::to_owned),
            has_remote_content: false,
            is_read: mail.read,
            is_flagged: flagged,
            received_at: ago(mail.minutes_ago),
            attachments: mail
                .attachments
                .iter()
                .map(|name| MessageAttachment {
                    filename: (*name).to_owned(),
                    content_type: "application/pdf".to_owned(),
                    size_bytes: 248_312,
                    storage_path: format!("fixture/{name}"),
                    content_id: None,
                    inline_only: false,
                })
                .collect(),
        })
        .collect();
    let latest = messages
        .iter()
        .max_by(|left, right| left.received_at.cmp(&right.received_at))
        .expect("threads have messages");
    ThreadSnapshot {
        remote_thread_id: id.to_owned(),
        subject: subject.to_owned(),
        participants: latest.sender.clone(),
        preview: latest.body_preview.clone(),
        is_flagged: flagged,
        has_attachments: mails.iter().any(|mail| !mail.attachments.is_empty()),
        unread_count: mails.iter().filter(|mail| !mail.read).count(),
        latest_message_at: latest.received_at.clone(),
        messages,
    }
}

fn account(store: &LocalStore, name: &str, email: &str) -> i64 {
    store
        .insert_account(&NewAccount {
            display_name: name.to_owned(),
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            endpoint: "imaps://mail.example.com:993".to_owned(),
            username: email.to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: None,
            identities: vec![NewIdentity {
                display_name: name.to_owned(),
                email_address: email.to_owned(),
                signature: String::new(),
                is_primary: true,
            }],
        })
        .expect("the account is stored")
}

fn mailbox(name: &str, role: Option<&str>, threads: Vec<ThreadSnapshot>) -> MailboxSnapshot {
    MailboxSnapshot {
        name: name.to_owned(),
        role: role.map(str::to_owned),
        threads,
        ..MailboxSnapshot::default()
    }
}

const NEWSLETTER: &str = "<p>Hello from the <b>harbour</b> newsletter.</p>\
    <p>Read <a href=\"https://news.example/issue/42\">the full issue</a> or \
    <a href=\"https://evil.example/login\">www.bank.example</a>.</p>";

/// Two accounts: Work, with a conversation, an HTML newsletter, mail with
/// wide characters and hostile mail full of escape sequences; and Home.
fn seeded_store() -> Arc<LocalStore> {
    let store = Arc::new(LocalStore::open_ephemeral().expect("the store opens"));
    let work = account(&store, "Work", "me@work.example");
    let home = account(&store, "Home", "me@home.example");
    store
        .replace_account_snapshot(
            work,
            &AccountSyncSnapshot {
                checkpoint: "test:1".to_owned(),
                detail: "Up to date".to_owned(),
                identities: None,
                mailboxes: vec![
                    mailbox(
                        "INBOX",
                        Some("inbox"),
                        vec![
                            thread(
                                "launch",
                                "Launch plan for the terminal UI",
                                true,
                                &[
                                    text_mail(
                                        "Ada Lovelace <ada@work.example>",
                                        3 * 60,
                                        true,
                                        "Here is the plan. The terminal UI reads mail first, \
                                         then triage, compose and accounts.\n\nAda",
                                    ),
                                    text_mail(
                                        "Grace Hopper <grace@work.example>",
                                        25,
                                        false,
                                        "Looks good. The notes are at https://wiki.work.example/tui \
                                         if anyone wants them.\n\nGrace",
                                    ),
                                ],
                            ),
                            thread(
                                "newsletter",
                                "Harbour newsletter, issue 42",
                                false,
                                &[Mail {
                                    sender: "Harbour News <news@harbour.example>",
                                    minutes_ago: 26 * 60,
                                    read: false,
                                    text: None,
                                    html: Some(NEWSLETTER),
                                    attachments: &[],
                                }],
                            ),
                            thread(
                                "tokyo",
                                "東京オフィスの会議 📅 next week",
                                false,
                                &[Mail {
                                    sender: "山田 太郎 <yamada@work.example>",
                                    minutes_ago: 3 * 24 * 60,
                                    read: true,
                                    text: Some("会議は火曜日です。\nThe agenda is attached."),
                                    html: None,
                                    attachments: &["agenda.pdf"],
                                }],
                            ),
                            thread(
                                "hostile",
                                "Invoice\x1b[2J\x1b]0;pwned\x07 \u{202E}fdp.exe",
                                false,
                                &[text_mail(
                                    "Billing\x1b[31m <billing@evil.example>",
                                    20 * 24 * 60,
                                    true,
                                    "Pay now\x1b]52;c;Y3VybCBldmls\x07!\r\n\x1b[1A\x1b[2KYour \
                                     account\tis fine.\x1bP+q\x1b\\ Bye\u{9b}31m",
                                )],
                            ),
                        ],
                    ),
                    mailbox(
                        "Archive",
                        Some("archive"),
                        vec![thread(
                            "old",
                            "Old receipt",
                            false,
                            &[text_mail("Shop <shop@example.com>", 400 * 24 * 60, true, "Thanks.")],
                        )],
                    ),
                    MailboxSnapshot {
                        name: "Projects/Website".to_owned(),
                        delimiter: Some("/".to_owned()),
                        parent: Some("Projects".to_owned()),
                        ..MailboxSnapshot::default()
                    },
                ],
            },
        )
        .expect("the Work mail is stored");
    store
        .replace_account_snapshot(
            home,
            &AccountSyncSnapshot {
                checkpoint: "test:1".to_owned(),
                detail: "Up to date".to_owned(),
                identities: None,
                mailboxes: vec![mailbox(
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
                            attachments: &["booking.pdf"],
                        }],
                    )],
                )],
            },
        )
        .expect("the Home mail is stored");
    store
}

fn environment() -> Environment {
    Environment {
        color: true,
        unicode: true,
        over_ssh: false,
        macos: false,
    }
}

fn app_with(store: Arc<LocalStore>, env: Environment) -> App {
    App::new(AppOptions {
        store,
        sync: None,
        runtime: None,
        env,
        clock: Arc::new(now),
        hour_cycle: HourCycle::H24,
        version: "0.0.0-test".to_owned(),
        arrivals: None,
    })
}

fn app() -> App {
    app_with(seeded_store(), environment())
}

/// Draws `app` on a `width` x `height` test terminal.
fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    app.handle_event(Event::Resize(width, height));
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| view::draw(app, frame))
        .expect("the frame draws");
    terminal.backend().buffer().clone()
}

/// The buffer's text, one line per row, wide characters once.
fn text_of(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            skip = text::width(symbol).saturating_sub(1);
            line.push_str(symbol);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Compares the screen with `src/snapshots/<name>.txt`.
fn assert_snapshot(name: &str, buffer: &Buffer) {
    let actual = text_of(buffer);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("ZMAIL_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        std::fs::write(&path, &actual).expect("the snapshot is written");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "no snapshot {}; run with ZMAIL_UPDATE_SNAPSHOTS=1 and review it\n{actual}",
            path.display()
        )
    });
    // Git may check text files out with CRLF on Windows.
    let expected = expected.replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "screen {name} differs from its snapshot:\n{actual}"
    );
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn keys(app: &mut App, typed: &str) {
    for ch in typed.chars() {
        key(app, KeyCode::Char(ch));
    }
}

fn control(app: &mut App, ch: char) {
    app.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char(ch),
        KeyModifiers::CONTROL,
    )));
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    app.handle_event(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }));
}

fn click(app: &mut App, column: u16, row: u16) {
    mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
    mouse(app, MouseEventKind::Up(MouseButton::Left), column, row);
}

/// The screen row whose text contains `needle`.
fn row_of(buffer: &Buffer, needle: &str) -> u16 {
    text_of(buffer)
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{}", text_of(buffer))) as u16
}

/// Opens the Work INBOX from the Folder List.
fn open_work_inbox(app: &mut App, width: u16, height: u16) -> Buffer {
    keys(app, "l");
    let buffer = draw(app, width, height);
    let row = row_of(&buffer, "INBOX");
    click(app, 6, row);
    draw(app, width, height)
}

/// Every cell is printable: no control character, ESC or C1 reached the
/// screen.
fn assert_printable(buffer: &Buffer) {
    for cell in buffer.content() {
        for ch in cell.symbol().chars() {
            assert!(
                !ch.is_control()
                    && !matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'),
                "U+{:04X} reached the screen:\n{}",
                ch as u32,
                text_of(buffer)
            );
        }
    }
}

#[test]
fn every_screen_matches_its_snapshot_narrow_and_wide() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = app();
        let size = format!("{width}x{height}");
        assert_snapshot(&format!("main-menu-{size}"), &draw(&mut app, width, height));
        keys(&mut app, "l");
        assert_snapshot(
            &format!("folder-list-{size}"),
            &draw(&mut app, width, height),
        );
        let buffer = open_work_inbox(&mut app, width, height);
        assert_eq!(app.screen, Screen::Index);
        assert_snapshot(&format!("message-index-{size}"), &buffer);
        // The newsletter: HTML, with links.
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::Message);
        assert_snapshot(
            &format!("message-text-{size}"),
            &draw(&mut app, width, height),
        );
        keys(&mut app, "<");
        key(&mut app, KeyCode::Up);
        keys(&mut app, "/");
        assert_snapshot(
            &format!("message-index-ungrouped-{size}"),
            &draw(&mut app, width, height),
        );
        keys(&mut app, "/m");
        keys(&mut app, "s");
        assert_snapshot(&format!("setup-{size}"), &draw(&mut app, width, height));
        keys(&mut app, "?");
        assert_snapshot(&format!("help-{size}"), &draw(&mut app, width, height));
    }
}

#[test]
fn hostile_mail_never_reaches_the_terminal_unfiltered() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = app();
        let buffer = open_work_inbox(&mut app, width, height);
        assert_printable(&buffer);
        assert!(text_of(&buffer).contains("Invoice"));
        assert!(!text_of(&buffer).contains("pwned"));
        let row = row_of(&buffer, "Invoice");
        click(&mut app, 30, row);
        if app.screen != Screen::Message {
            // With the preview pane, the first click selects.
            click(&mut app, 30, row);
        }
        let buffer = draw(&mut app, width, height);
        assert_printable(&buffer);
        let screen = text_of(&buffer);
        assert!(screen.contains("Pay now!"), "{screen}");
        // The tab goes to the next 8-column stop.
        assert!(screen.contains("Your account    is fine. Bye"), "{screen}");
        assert!(
            !screen.contains("52;c") && !screen.contains("Y3VybC"),
            "{screen}"
        );
        assert!(screen.contains("invoicefdp.exe") || screen.contains("Invoice fdp.exe"));
        assert_snapshot(&format!("hostile-message-{width}x{height}"), &buffer);
    }
}

#[test]
fn alpine_keys_move_through_the_screens() {
    let mut app = app();
    draw(&mut app, 80, 24);
    assert_eq!(app.screen, Screen::MainMenu);
    // Enter on the Main Menu runs the highlighted item: Message Index.
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, Screen::Index);
    // The default folder is the Unified Inbox: three accounts' inboxes.
    assert_eq!(app.index.entries.len(), 5);
    keys(&mut app, "<");
    assert_eq!(app.screen, Screen::FolderList);
    keys(&mut app, "<");
    assert_eq!(app.screen, Screen::MainMenu);
    keys(&mut app, "i>");
    assert_eq!(app.screen, Screen::Message);
    let first = app
        .message
        .as_ref()
        .map(|state| state.conversation.thread_id);
    keys(&mut app, "n");
    let second = app
        .message
        .as_ref()
        .map(|state| state.conversation.thread_id);
    assert_ne!(first, second, "N opens the next message");
    keys(&mut app, "p");
    assert_eq!(
        app.message
            .as_ref()
            .map(|state| state.conversation.thread_id),
        first
    );
    keys(&mut app, "M");
    assert_eq!(app.screen, Screen::MainMenu);
    // Q asks first.
    keys(&mut app, "q");
    assert!(!app.should_quit());
    assert!(
        text_of(&draw(&mut app, 80, 24)).contains("Really quit Zmail?"),
        "the quit question shows"
    );
    keys(&mut app, "n");
    assert!(!app.should_quit());
    keys(&mut app, "Qy");
    assert!(app.should_quit());
}

#[test]
fn other_cmds_pages_the_command_bar() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("O OTHER CMDS"), "{screen}");
    assert!(
        screen.contains("WhereIs") && !screen.contains("Quit"),
        "{screen}"
    );
    keys(&mut app, "o");
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(
        screen.contains("Quit") && !screen.contains("WhereIs"),
        "{screen}"
    );
    // A click on a command runs it.
    let buffer = draw(&mut app, 80, 24);
    let row = row_of(&buffer, "Quit");
    let column = text_of(&buffer)
        .lines()
        .nth(row as usize)
        .and_then(|line| line.find("Quit"))
        .expect("Quit is on the bar") as u16;
    click(&mut app, column, row);
    let buffer = draw(&mut app, 80, 24);
    assert!(text_of(&buffer).contains("Really quit Zmail?"));
    // The question's answers are on the bar, and clickable.
    let row = row_of(&buffer, "N No");
    let column = text_of(&buffer)
        .lines()
        .nth(row as usize)
        .and_then(|line| line.find("N No"))
        .expect("No is on the bar") as u16;
    click(&mut app, column, row);
    assert!(app.prompt.is_none() && !app.should_quit());
    keys(&mut app, "q");
    let buffer = draw(&mut app, 80, 24);
    let row = row_of(&buffer, "Y Yes");
    click(&mut app, 0, row);
    assert!(app.should_quit());
}

#[test]
fn threads_group_and_ungroup_and_the_choice_is_saved() {
    let store = seeded_store();
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);
    assert_eq!(app.index.entries.len(), 4, "four conversations");
    keys(&mut app, "/");
    assert_eq!(app.index.entries.len(), 5, "five messages");
    assert!(
        !store
            .ui_preferences()
            .expect("preferences")
            .terminal
            .group_threads
    );
    // The newest message first, each a row of its own.
    assert_eq!(app.index.entries[0].sender, "Grace Hopper");
    assert_eq!(app.index.entries[1].sender, "Ada Lovelace");
    keys(&mut app, "/");
    assert_eq!(app.index.entries.len(), 4);
}

#[test]
fn opening_a_single_message_starts_at_it() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, "/");
    // Ada's message is the older of the conversation's two.
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    let screen = text_of(&draw(&mut app, 80, 24));
    let first_body_line = screen.lines().nth(1).unwrap_or_default();
    assert!(first_body_line.contains("Message 1 of 2"), "{screen}");
    // Grace's, the newer, is the conversation's end: it scrolls as far as
    // the text goes.
    keys(&mut app, "<");
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Enter);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(!screen.contains("Message 1 of 2"), "{screen}");
    assert!(screen.contains("From: Grace Hopper"), "{screen}");
}

#[test]
fn where_is_searches_as_you_type_and_esc_restores_the_folder() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, "w");
    keys(&mut app, "harbour");
    assert_eq!(app.index.entries.len(), 1);
    assert_eq!(app.index.entries[0].subject, "Harbour newsletter, issue 42");
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("WhereIs"), "{screen}");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.index.entries.len(), 4);
    // Enter keeps the results; < goes back to the whole folder.
    keys(&mut app, "wfrom:ada");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.index.entries.len(), 1);
    assert!(text_of(&draw(&mut app, 80, 24)).contains("Search \"from:ada\""));
    keys(&mut app, "<");
    assert_eq!(app.index.entries.len(), 4);
    assert_eq!(app.screen, Screen::Index);
}

#[test]
fn pastes_go_into_text_fields_and_never_run_commands() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    // Outside a text field, "q" pasted is not the Quit command.
    app.handle_event(Event::Paste("q\nm".to_owned()));
    assert!(app.prompt.is_none());
    assert_eq!(app.screen, Screen::Index);
    keys(&mut app, "w");
    app.handle_event(Event::Paste("harbour\x1b[2J\nnews".to_owned()));
    let Some(crate::app::Prompt::Text { input, .. }) = &app.prompt else {
        panic!("WhereIs is open");
    };
    assert_eq!(input, "harbour news");
}

#[test]
fn a_drag_selects_in_one_pane_and_copies_the_drawn_text_on_release() {
    let mut app = app();
    let buffer = open_work_inbox(&mut app, 160, 48);
    // With the preview pane, the Index is on the left and the preview on
    // the right. Drag across Ada's text in the preview.
    let row = row_of(&buffer, "Here is the plan");
    let line = text_of(&buffer)
        .lines()
        .nth(row as usize)
        .unwrap_or_default()
        .to_owned();
    let start = text::width(&line[..line.find("Here").expect("text")]) as u16;
    app.take_effects();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        start,
        row,
    );
    // Past the right edge of the screen: the selection stays in the
    // preview pane.
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        159,
        row + 1,
    );
    let buffer = draw(&mut app, 160, 48);
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        159,
        row + 1,
    );
    let effects = app.take_effects();
    let Some(Effect::Copy(copied)) = effects.first() else {
        panic!("a copy on release, got {effects:?}");
    };
    // The wrapped line rejoined, and no padding or index text.
    assert!(
        copied.starts_with("Here is the plan. The terminal UI reads mail first,"),
        "{copied:?}"
    );
    assert!(!copied.contains("  "), "{copied:?}");
    assert!(!copied.contains("Launch plan"), "{copied:?}");
    // The selection is drawn in reverse video in the preview only.
    let reversed = |x: u16, y: u16| {
        buffer[(x, y)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    };
    assert!(reversed(start, row));
    assert!(!reversed(2, row + 5));
    assert!(text_of(&draw(&mut app, 160, 48)).contains("Copied"));
}

#[test]
fn shift_drag_is_left_to_the_terminal() {
    let mut app = app();
    let buffer = open_work_inbox(&mut app, 160, 48);
    let row = row_of(&buffer, "Here is the plan");
    app.take_effects();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.handle_event(Event::Mouse(MouseEvent {
            kind,
            column: 90,
            row,
            modifiers: KeyModifiers::SHIFT,
        }));
    }
    assert!(app.selection.is_none());
    assert!(app.take_effects().is_empty());
}

#[test]
fn double_and_triple_clicks_select_a_word_and_a_line() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    key(&mut app, KeyCode::Enter);
    let buffer = draw(&mut app, 80, 24);
    let row = row_of(&buffer, "Here is the plan");
    let line = text_of(&buffer)
        .lines()
        .nth(row as usize)
        .unwrap_or_default()
        .to_owned();
    let column = line.find("plan").expect("the word") as u16 + 1;
    app.take_effects();
    click(&mut app, column, row);
    click(&mut app, column, row);
    assert_eq!(app.take_effects(), vec![Effect::Copy("plan.".to_owned())]);
    click(&mut app, column, row);
    let effects = app.take_effects();
    let Some(Effect::Copy(copied)) = effects.first() else {
        panic!("a copy, got {effects:?}");
    };
    assert_eq!(
        copied,
        "Here is the plan. The terminal UI reads mail first, then triage, compose and \
         accounts."
    );
}

#[test]
fn ctrl_space_marks_and_the_arrow_keys_select_for_a_keyboard_copy() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    key(&mut app, KeyCode::Enter);
    draw(&mut app, 80, 24);
    // The mark goes at the top line; move to the Date header's text.
    control(&mut app, ' ');
    for _ in 0..3 {
        key(&mut app, KeyCode::Down);
    }
    for _ in 0..5 {
        key(&mut app, KeyCode::Right);
    }
    draw(&mut app, 80, 24);
    app.take_effects();
    keys(&mut app, "y");
    let effects = app.take_effects();
    let Some(Effect::Copy(copied)) = effects.first() else {
        panic!("a copy, got {effects:?}");
    };
    assert!(copied.starts_with("── Message 1 of 2"), "{copied:?}");
    assert!(copied.ends_with("\nTo: Me"), "{copied:?}");
    // Ctrl-C copies too, once something is selected.
    control(&mut app, ' ');
    key(&mut app, KeyCode::Right);
    control(&mut app, 'c');
    assert_eq!(app.take_effects().len(), 1);
}

#[test]
fn links_open_only_after_the_real_address_is_shown() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("the full issue[1]"), "{screen}");
    assert!(screen.contains("www.bank.example[2]"), "{screen}");
    // Tab twice: the bank-looking link, which goes elsewhere.
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    let screen = text_of(&draw(&mut app, 200, 24));
    assert!(
        screen.contains("Open https://evil.example/login?"),
        "{screen}"
    );
    assert!(screen.contains("shows www.bank.example"), "{screen}");
    app.take_effects();
    keys(&mut app, "n");
    assert!(app.take_effects().is_empty(), "No opens nothing");
    // A click on a reference asks the same question.
    let buffer = draw(&mut app, 80, 24);
    let row = row_of(&buffer, "the full issue[1]");
    let line = text_of(&buffer)
        .lines()
        .nth(row as usize)
        .unwrap_or_default()
        .to_owned();
    let column = line.find("[1]").expect("the reference") as u16 + 1;
    click(&mut app, column, row);
    keys(&mut app, "y");
    assert_eq!(
        app.take_effects(),
        vec![Effect::Open("https://news.example/issue/42".to_owned())]
    );
}

#[test]
fn the_wheel_scrolls_and_a_click_opens() {
    let mut app = app();
    let buffer = open_work_inbox(&mut app, 80, 24);
    mouse(&mut app, MouseEventKind::ScrollDown, 10, 3);
    assert_eq!(app.index.cursor, 3);
    let row = row_of(&buffer, "Harbour newsletter");
    click(&mut app, 30, row);
    assert_eq!(app.screen, Screen::Message);
    assert_eq!(
        app.message
            .as_ref()
            .map(|state| state.conversation.subject.as_str()),
        Some("Harbour newsletter, issue 42")
    );
}

#[test]
fn no_color_draws_in_monochrome_and_ascii_without_box_drawing() {
    let store = seeded_store();
    store
        .update_ui_preferences(|preferences| preferences.terminal.ascii = true)
        .expect("saved");
    let mut app = app_with(
        store,
        Environment {
            color: false,
            ..environment()
        },
    );
    let buffer = open_work_inbox(&mut app, 160, 48);
    key(&mut app, KeyCode::Enter);
    let message = draw(&mut app, 160, 48);
    for buffer in [&buffer, &message] {
        for cell in buffer.content() {
            assert_eq!(cell.fg, Color::Reset, "{:?}", cell);
            assert_eq!(cell.bg, Color::Reset, "{:?}", cell);
            assert!(
                !cell.symbol().contains(['│', '─', '…']),
                "box drawing: {}",
                text_of(buffer)
            );
        }
    }
    assert!(text_of(&message).contains("-- Message 1 of 2 ---"));
}

#[test]
fn new_mail_shows_in_the_status_line_and_rings_the_bell() {
    let (sender, arrivals) = mpsc::channel();
    let mut app = App::new(AppOptions {
        store: seeded_store(),
        sync: None,
        runtime: None,
        env: environment(),
        clock: Arc::new(now),
        hour_cycle: HourCycle::H24,
        version: "0.0.0-test".to_owned(),
        arrivals: Some(arrivals),
    });
    let batch = NewMailBatch {
        account_id: 1,
        account: "Work\x1b]0;x\x07".to_owned(),
        arrivals: vec![NewMailArrival {
            subject: "Lunch?\x1b[2J".to_owned(),
            sender: "Ada".to_owned(),
            mailbox: "INBOX".to_owned(),
            remote_thread_id: "lunch".to_owned(),
        }],
    };
    sender.send(batch.clone()).expect("sent");
    app.tick();
    assert_eq!(app.take_effects(), vec![Effect::Bell]);
    let buffer = draw(&mut app, 80, 24);
    assert_printable(&buffer);
    assert!(text_of(&buffer).contains("[1 new message in Work: Lunch?]"));
    // Without the bell.
    keys(&mut app, "s");
    draw(&mut app, 80, 24);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert!(!app.preferences().bell);
    sender.send(batch).expect("sent");
    app.tick();
    assert!(app.take_effects().is_empty());
}

#[test]
fn the_status_line_says_when_another_zmail_syncs() {
    let store = seeded_store();
    let held = store.try_sync_lock().expect("the lock file opens");
    assert!(held.is_some());
    let runtime = zmail_sync::BackgroundRuntime::start(1).expect("a runtime");
    let sync = zmail_sync::SyncService::with_sync_lock(
        runtime.handle().clone(),
        store.clone(),
        zmail_security::InMemorySecretStore::shared(),
    )
    .expect("the service starts");
    assert!(!sync.runs_sync());
    let mut app = App::new(AppOptions {
        store,
        sync: Some(sync),
        runtime: Some(runtime.handle().clone()),
        env: environment(),
        clock: Arc::new(now),
        hour_cycle: HourCycle::H24,
        version: "0.0.0-test".to_owned(),
        arrivals: None,
    });
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains(zmail_sync::SYNCS_ELSEWHERE), "{screen}");
}

#[test]
fn the_store_is_read_again() {
    let store = seeded_store();
    let mut app = app_with(store.clone(), environment());
    open_work_inbox(&mut app, 80, 24);
    assert_eq!(app.index.entries.len(), 4);
    let work = store.list_accounts().expect("accounts")[0].id;
    let mut snapshot = AccountSyncSnapshot {
        checkpoint: "test:2".to_owned(),
        detail: "Up to date".to_owned(),
        identities: None,
        mailboxes: vec![mailbox(
            "INBOX",
            Some("inbox"),
            vec![thread(
                "new",
                "A new message",
                false,
                &[text_mail("Bob <bob@work.example>", 1, false, "Hi")],
            )],
        )],
    };
    snapshot.mailboxes[0].threads[0].unread_count = 1;
    store
        .replace_account_snapshot(work, &snapshot)
        .expect("stored");
    app.refresh();
    assert!(
        app.index
            .entries
            .iter()
            .any(|entry| entry.subject == "A new message")
    );
}

#[test]
fn setup_changes_are_kept_in_the_store_and_mouse_capture_follows() {
    let store = seeded_store();
    let mut app = app_with(store.clone(), environment());
    keys(&mut app, "s");
    draw(&mut app, 80, 24);
    app.take_effects();
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.take_effects(), vec![Effect::MouseCapture(false)]);
    // The copy command, with a paste in its field.
    for _ in 0..6 {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Enter);
    app.handle_event(Event::Paste("wl-copy --primary".to_owned()));
    key(&mut app, KeyCode::Enter);
    let saved = store.ui_preferences().expect("preferences").terminal;
    assert_eq!(
        saved,
        TerminalPreferences {
            mouse: false,
            copy_command: Some("wl-copy --primary".to_owned()),
            ..TerminalPreferences::default()
        }
    );
    assert!(
        text_of(&draw(&mut app, 80, 24)).contains("Copies go to the command wl-copy --primary")
    );
}

#[test]
fn with_no_accounts_the_screens_say_how_to_add_one() {
    let mut app = app_with(
        Arc::new(LocalStore::open_ephemeral().expect("the store opens")),
        environment(),
    );
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("No accounts yet"), "{screen}");
    keys(&mut app, "l");
    assert!(text_of(&draw(&mut app, 80, 24)).contains("No accounts yet"));
    keys(&mut app, "i");
    assert_eq!(app.screen, Screen::FolderList);
}
