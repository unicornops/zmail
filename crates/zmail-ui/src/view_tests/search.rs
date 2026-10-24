//! Live search (UI plan Phase 7 item 3): the debounce, stale results,
//! when the servers are asked, and the keys and button that enter and
//! leave the search.

use std::time::Duration;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use super::{
    Setup, click, focus, focused_pane, keys, open_workspace, search_input, search_is_focused,
    selected_thread, text,
};
use crate::pane::Pane;
use crate::search::{SEARCH_DEBOUNCE, SERVER_SEARCH_SETTLE, SearchLevel};
use crate::{ListMode, WorkspaceView};

fn list_mode(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> ListMode {
    cx.read_entity(view, |view, _| view.list_mode)
}

fn generation(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> u64 {
    cx.read_entity(view, |view, _| view.search_context.generation)
}

fn servers_asked(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> bool {
    cx.read_entity(view, |view, _| view.search_context.servers_asked)
}

/// The shown results' subjects and the query they are for.
fn results(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> (String, Vec<String>) {
    cx.read_entity(view, |view, _| {
        (
            view.search_results_query.clone(),
            view.search_results
                .iter()
                .map(|hit| hit.subject.clone())
                .collect(),
        )
    })
}

/// Lets `duration` of (test) time pass.
fn wait(duration: Duration, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(duration);
    cx.run_until_parked();
}

/// Types `query` into the empty search field one character at a time,
/// each keystroke `gap` after the one before.
fn type_slowly(query: &str, gap: Duration, cx: &mut VisualTestContext) {
    for ch in query.chars() {
        cx.simulate_input(&ch.to_string());
        wait(gap, cx);
    }
}

#[gpui::test]
fn typing_searches_once_the_debounce_passes(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let search = search_input(&view, cx);
    focus(&search, cx);
    let before = generation(&view, cx);

    // Typing faster than the debounce runs nothing yet.
    type_slowly("report 1", Duration::from_millis(50), cx);
    assert_eq!(list_mode(&view, cx), ListMode::Mail);
    assert_eq!(generation(&view, cx), before, "no search ran mid-word");

    // A pause runs one search for the whole text, with no Enter.
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(list_mode(&view, cx), ListMode::Search);
    assert_eq!(generation(&view, cx), before + 1);
    assert_eq!(
        results(&view, cx),
        ("report 1".to_owned(), vec!["Quarterly report 1".to_owned()])
    );
    assert!(search_is_focused(&view, cx), "focus stays in the field");

    // Typing on refines the results live.
    cx.simulate_input("x");
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(results(&view, cx), ("report 1x".to_owned(), Vec::new()));

    // Deleting everything goes back to the mailbox at once.
    keys("backspace backspace backspace backspace backspace", cx);
    keys("backspace backspace backspace backspace", cx);
    assert_eq!(text(&search, cx), "");
    assert_eq!(list_mode(&view, cx), ListMode::Mail);
    wait(SERVER_SEARCH_SETTLE, cx);
    assert_eq!(list_mode(&view, cx), ListMode::Mail, "nothing ran late");
}

#[gpui::test]
fn stale_results_never_replace_newer_ones(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let search = search_input(&view, cx);

    // Two searches start before either read finishes; only the second
    // one's results show, whichever finishes last.
    view.update(cx, |view, cx| {
        view.search_query
            .update(cx, |input, cx| input.set_text("report 1", cx));
        view.submit_search(cx);
        view.search_query
            .update(cx, |input, cx| input.set_text("report 2", cx));
        view.submit_search(cx);
    });
    cx.run_until_parked();
    assert_eq!(
        results(&view, cx),
        ("report 2".to_owned(), vec!["Quarterly report 2".to_owned()])
    );

    // Enter on text the shown results match, while a read for other text
    // is still running, searches the text again rather than keeping the
    // running read.
    view.update(cx, |view, cx| {
        view.search_query
            .update(cx, |input, cx| input.set_text("report 0", cx));
        view.submit_search(cx);
        view.search_query
            .update(cx, |input, cx| input.set_text("report 2", cx));
        view.submit_search(cx);
    });
    cx.run_until_parked();
    assert_eq!(
        results(&view, cx),
        ("report 2".to_owned(), vec!["Quarterly report 2".to_owned()])
    );

    // A read for an earlier generation that arrives late is dropped.
    let current = generation(&view, cx);
    view.update(cx, |view, cx| {
        let account_id = view.selected_account_id.expect("an account");
        let stale = view
            .store
            .search_threads(account_id, "report 0", 10)
            .expect("the search runs");
        view.show_search_results(current - 1, "report 0".to_owned(), Ok(stale), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        results(&view, cx),
        ("report 2".to_owned(), vec!["Quarterly report 2".to_owned()])
    );
    assert_eq!(text(&search, cx), "report 2");
}

#[gpui::test]
fn the_servers_are_asked_once_typing_settles_or_on_enter(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let search = search_input(&view, cx);
    focus(&search, cx);

    // The local search runs after the debounce; the servers wait for
    // typing to settle.
    cx.simulate_input("numbers");
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(list_mode(&view, cx), ListMode::Search);
    assert!(!servers_asked(&view, cx));
    wait(SERVER_SEARCH_SETTLE - SEARCH_DEBOUNCE * 2, cx);
    assert!(!servers_asked(&view, cx));

    // Another keystroke starts the wait again.
    cx.simulate_input(" a");
    wait(SERVER_SEARCH_SETTLE - SEARCH_DEBOUNCE, cx);
    assert!(!servers_asked(&view, cx));
    wait(SEARCH_DEBOUNCE, cx);
    assert!(servers_asked(&view, cx), "typing settled");

    // Enter searches and asks straight away, without waiting for the
    // debounce.
    let before = generation(&view, cx);
    cx.simulate_input("t");
    keys("enter", cx);
    assert_eq!(generation(&view, cx), before + 1);
    assert!(servers_asked(&view, cx));
    assert_eq!(results(&view, cx).0, "numbers at");
    // Enter again on the same search does not ask twice.
    keys("enter", cx);
    assert_eq!(generation(&view, cx), before + 1);
}

#[gpui::test]
fn refining_the_query_keeps_the_scope(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Accounts);
    let search = search_input(&view, cx);
    focus(&search, cx);
    cx.simulate_input("numbers");
    wait(SEARCH_DEBOUNCE, cx);
    keys("secondary-alt-3", cx);
    let level =
        |cx: &mut VisualTestContext| cx.read_entity(&view, |view, _| view.search_context.level);
    assert_eq!(level(cx), SearchLevel::AllAccounts);
    cx.simulate_input(" attached");
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(results(&view, cx).0, "numbers attached");
    assert_eq!(level(cx), SearchLevel::AllAccounts);
}

#[gpui::test]
fn escape_clears_the_field_and_leaves_search(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    keys("secondary-f", cx);
    cx.simulate_input("quarterly");
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(list_mode(&view, cx), ListMode::Search);

    keys("escape", cx);
    assert_eq!(list_mode(&view, cx), ListMode::Mail);
    assert_eq!(text(&search_input(&view, cx), cx), "");
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // Escape before the debounce ran cancels the pending search.
    keys("secondary-f", cx);
    cx.simulate_input("quarterly");
    keys("escape", cx);
    wait(SERVER_SEARCH_SETTLE, cx);
    assert_eq!(list_mode(&view, cx), ListMode::Mail);
}

#[gpui::test]
fn the_clear_button_empties_the_field_and_keeps_focus(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    assert!(
        cx.debug_bounds("text-input-clear").is_none(),
        "an empty field has nothing to clear"
    );
    keys("secondary-f", cx);
    cx.simulate_input("quarterly");
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(list_mode(&view, cx), ListMode::Search);

    click("text-input-clear", cx);
    assert_eq!(text(&search_input(&view, cx), cx), "");
    assert_eq!(list_mode(&view, cx), ListMode::Mail);
    assert!(search_is_focused(&view, cx), "ready to type a new search");
    assert!(cx.debug_bounds("text-input-clear").is_none());
}

#[gpui::test]
fn down_moves_from_the_field_into_the_results(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    keys("secondary-f", cx);
    // Straight after typing: Down does not wait for the debounce.
    cx.simulate_input("report 2");
    keys("down", cx);
    assert_eq!(list_mode(&view, cx), ListMode::Search);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[2]));
    assert!(servers_asked(&view, cx), "Down ends the typing");
    assert_eq!(text(&search_input(&view, cx), cx), "report 2");

    // Escape from the results leaves the search too.
    keys("escape", cx);
    assert_eq!(list_mode(&view, cx), ListMode::Mail);
    assert_eq!(text(&search_input(&view, cx), cx), "");
}

#[gpui::test]
fn slash_and_secondary_f_focus_the_search_field(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    for pane in [Pane::MessageList, Pane::Sidebar, Pane::Detail] {
        view.update_in(cx, |view, window, cx| view.focus_pane(pane, window, cx));
        cx.run_until_parked();
        keys("/", cx);
        assert!(search_is_focused(&view, cx), "/ from {pane:?}");
    }
    // In the field the key types.
    keys("/", cx);
    assert_eq!(text(&search_input(&view, cx), cx), "/");

    // Secondary+F selects what is there, so typing replaces it.
    keys("escape secondary-f", cx);
    cx.simulate_input("quarterly");
    keys("secondary-f", cx);
    cx.simulate_input("report 0");
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(
        results(&view, cx),
        ("report 0".to_owned(), vec!["Quarterly report 0".to_owned()])
    );
}

#[gpui::test]
fn showing_a_mailbox_ends_the_search(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    keys("secondary-f", cx);
    cx.simulate_input("quarterly");
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(list_mode(&view, cx), ListMode::Search);
    let archive = harness.archive_id.expect("an archive");
    view.update(cx, |view, cx| view.select_mailbox(archive, cx));
    cx.run_until_parked();
    assert_eq!(list_mode(&view, cx), ListMode::Mail);
    assert_eq!(text(&search_input(&view, cx), cx), "");
}

/// An Inbox of `count` threads that all match "report", newest first.
fn seed_reports(count: usize) -> impl FnOnce(&zmail_store::LocalStore) {
    move |store| {
        let account_id = store.list_accounts().expect("accounts load")[0].id;
        let threads = (0..count)
            .map(|index| {
                let mut thread = super::thread_snapshot(0);
                let at = format!("2026-03-01T{:02}:{:02}:00Z", index / 60, index % 60);
                thread.remote_thread_id = format!("report-{index}");
                thread.subject = format!("Weekly report {index}");
                thread.latest_message_at = at.clone();
                for message in &mut thread.messages {
                    message.remote_id = format!("report-message-{index}");
                    message.subject = thread.subject.clone();
                    message.received_at = at.clone();
                }
                thread
            })
            .collect();
        store
            .replace_account_snapshot(
                account_id,
                &zmail_core::AccountSyncSnapshot {
                    checkpoint: "test".to_owned(),
                    detail: "test".to_owned(),
                    identities: None,
                    mailboxes: vec![zmail_core::MailboxSnapshot {
                        name: "Inbox".to_owned(),
                        role: Some("inbox".to_owned()),
                        threads,
                        ..Default::default()
                    }],
                },
            )
            .expect("the mail is stored");
    }
}

#[gpui::test]
fn results_page_in_as_the_list_scrolls(cx: &mut TestAppContext) {
    let (view, cx, _harness) = super::open_workspace_with(
        cx,
        Setup::Account,
        |_| crate::InitialView::Inbox,
        seed_reports(250),
    );
    keys("secondary-f", cx);
    cx.simulate_input("report");
    keys("enter", cx);
    let count =
        |cx: &mut VisualTestContext| cx.read_entity(&view, |view, _| view.search_results.len());
    assert_eq!(count(cx), 100, "one page to start with");

    // Moving to the last loaded result scrolls it into view, which reads
    // the next page; the end of the results stops asking.
    keys("down", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    for expected in [200, 250, 250] {
        keys("end", cx);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert_eq!(count(cx), expected);
    }
    assert!(!cx.read_entity(&view, |view, _| view.search_context.has_more));
    // Newest first, all the way down.
    let (_, subjects) = results(&view, cx);
    assert_eq!(
        subjects.first().map(String::as_str),
        Some("Weekly report 249")
    );
    assert_eq!(subjects.last().map(String::as_str), Some("Weekly report 0"));
}

#[gpui::test]
fn filter_chips_edit_the_query_text(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let search = search_input(&view, cx);
    keys("secondary-f", cx);
    cx.simulate_input("report");
    keys("enter", cx);
    assert_eq!(results(&view, cx).1.len(), 3);
    let filters = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, _| {
            crate::search::SearchFilters::of(&view.search_results_query)
        })
    };

    // Unread and Has attachment toggle their clause, and search at once.
    click("search-filter-unread", cx);
    assert_eq!(text(&search, cx), "report is:unread");
    assert!(filters(cx).unread);
    assert_eq!(results(&view, cx).1.len(), 3);
    click("search-filter-attachment", cx);
    assert_eq!(text(&search, cx), "report is:unread has:attachment");
    assert!(results(&view, cx).1.is_empty(), "nothing has an attachment");
    click("search-filter-unread", cx);
    click("search-filter-attachment", cx);
    assert_eq!(text(&search, cx), "report");
    assert_eq!(results(&view, cx).1.len(), 3);

    // Typing the operator lights the chip: the text is the state.
    keys("secondary-f", cx);
    cx.simulate_input("is:unread report");
    wait(SEARCH_DEBOUNCE, cx);
    assert!(filters(cx).unread);
    click("search-filter-unread", cx);
    assert_eq!(text(&search, cx), "report");

    // From hands the field over to type the sender.
    click("search-filter-from", cx);
    assert_eq!(text(&search, cx), "report from:");
    assert!(search_is_focused(&view, cx));
    cx.simulate_input("kai");
    wait(SEARCH_DEBOUNCE, cx);
    assert_eq!(filters(cx).from.as_deref(), Some("kai"));
    assert_eq!(results(&view, cx).1.len(), 3);
    click("search-filter-from", cx);
    assert_eq!(text(&search, cx), "report");

    // The date chip's menu sets a range, and Any time removes it.
    click("search-filter-date", cx);
    keys("down enter", cx);
    assert_eq!(text(&search, cx), "report after:7d");
    assert_eq!(filters(cx).date_label(), "Last 7 days");
    click("search-filter-date", cx);
    keys("up enter", cx);
    assert_eq!(text(&search, cx), "report");
    assert_eq!(results(&view, cx).1.len(), 3);
}

#[gpui::test]
fn a_body_match_shows_an_excerpt_in_place_of_the_snippet(cx: &mut TestAppContext) {
    let (view, cx, _harness) = super::open_workspace_with(
        cx,
        Setup::Account,
        |_| crate::InitialView::Inbox,
        |store| {
            seed_reports(2)(store);
            // The first report's body says more than its stored snippet.
            let thread = store
                .search_threads(
                    store.list_accounts().expect("accounts")[0].id,
                    "\"report 0\"",
                    1,
                )
                .expect("the search runs")[0]
                .thread_id;
            let message = store
                .thread_view(thread)
                .expect("the thread loads")
                .expect("the thread exists")
                .messages[0]
                .id;
            store
                .hydrate_message_body(
                    message,
                    &zmail_core::HydratedBody {
                        body_text: Some(format!(
                            "{}Then a kittiwake landed\non the sill.",
                            "Numbers for the week, as usual. ".repeat(4)
                        )),
                        sanitized_html: None,
                        raw_html: None,
                        has_remote_content: false,
                        body_preview: "Numbers attached".to_owned(),
                        attachments: Vec::new(),
                    },
                )
                .expect("the body is stored");
        },
    );
    let previews = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, _| {
            view.search_results
                .iter()
                .map(|hit| hit.preview.clone())
                .collect::<Vec<_>>()
        })
    };
    keys("secondary-f", cx);
    cx.simulate_input("kittiwake");
    keys("enter", cx);
    assert_eq!(
        previews(cx),
        vec!["\u{2026}week, as usual. Then a kittiwake landed on the sill.".to_owned()]
    );

    // A snippet that already shows the match stays as stored.
    keys("secondary-f", cx);
    cx.simulate_input("numbers");
    keys("enter", cx);
    assert_eq!(previews(cx), vec!["Numbers attached".to_owned(); 2]);
}
