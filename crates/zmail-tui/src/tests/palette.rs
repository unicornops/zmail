//! The command palette (`:`): the screen's commands from its command bar,
//! fuzzy filtered, run with Enter or a click.

use crossterm::event::{Event, KeyCode};

use super::{app, assert_snapshot, click, draw, key, keys, open_work_inbox, row_of, text_of};
use crate::app::{Cmd, Prompt, Screen};

#[test]
fn the_palette_matches_its_snapshots_narrow_and_wide() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = app();
        open_work_inbox(&mut app, width, height);
        keys(&mut app, ":");
        assert!(matches!(app.prompt, Some(Prompt::Palette(_))));
        assert_snapshot(
            &format!("palette-{width}x{height}"),
            &draw(&mut app, width, height),
        );
        keys(&mut app, "arch");
        assert_snapshot(
            &format!("palette-filtered-{width}x{height}"),
            &draw(&mut app, width, height),
        );
    }
}

#[test]
fn it_lists_every_command_of_the_bar_with_its_key() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    let bar: Vec<(String, Cmd)> = app
        .commands()
        .into_iter()
        .filter(|entry| entry.cmd != Cmd::Palette)
        .map(|entry| (entry.key.into_owned(), entry.cmd))
        .collect();
    keys(&mut app, ":");
    let Some(Prompt::Palette(palette)) = &app.prompt else {
        panic!("the palette is open");
    };
    let listed: Vec<(String, Cmd)> = palette
        .shown
        .iter()
        .map(|(index, _)| {
            let entry = &palette.items[*index].entry;
            (entry.key.clone().into_owned(), entry.cmd)
        })
        .collect();
    assert_eq!(listed, bar);
    // Every page of the bar, not just the first.
    assert!(listed.contains(&("V".to_owned(), Cmd::Preview)));
}

#[test]
fn enter_runs_the_command_under_the_cursor() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, ":fldr");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.prompt, None);
    assert_eq!(app.screen, Screen::FolderList);

    // Down moves through the matches; Archive runs on the conversation.
    keys(&mut app, "i");
    draw(&mut app, 80, 24);
    let before = app.index.entries.len();
    keys(&mut app, ":archive the");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.index.entries.len(), before - 1, "{:?}", app.status);
    assert!(
        app.status.as_deref().unwrap_or_default().contains("Undo"),
        "{:?}",
        app.status
    );
}

#[test]
fn esc_or_backspace_on_an_empty_query_closes_it_and_a_paste_filters() {
    let mut app = app();
    open_work_inbox(&mut app, 80, 24);
    keys(&mut app, ":");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.prompt, None);
    keys(&mut app, ":w");
    key(&mut app, KeyCode::Backspace);
    assert!(matches!(app.prompt, Some(Prompt::Palette(_))));
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.prompt, None);
    keys(&mut app, ":");
    app.handle_event(Event::Paste("where\x1b[2J".to_owned()));
    let Some(Prompt::Palette(palette)) = &app.prompt else {
        panic!("the palette is open");
    };
    assert_eq!(palette.query, "where");
    assert_eq!(palette.current(), Some(Cmd::WhereIs));
    let screen = text_of(&draw(&mut app, 80, 24));
    assert!(screen.contains("Nothing matches") || screen.contains("WhereIs"));
}

#[test]
fn a_click_runs_a_command() {
    let mut app = app();
    keys(&mut app, ":");
    let buffer = draw(&mut app, 80, 24);
    let row = row_of(&buffer, "Setup");
    click(&mut app, 10, row);
    assert_eq!(app.prompt, None);
    assert_eq!(app.screen, Screen::Setup);
}

#[test]
fn compose_takes_a_colon_as_text() {
    let mut app = app();
    keys(&mut app, "c");
    assert_eq!(app.screen, Screen::Compose);
    keys(&mut app, ":");
    assert!(!matches!(app.prompt, Some(Prompt::Palette(_))));
}
