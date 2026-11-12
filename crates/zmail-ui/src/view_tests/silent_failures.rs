//! Background work that fails says so: a message body that could not be
//! downloaded, and a server search that could not run.

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use super::{Setup, click, focus, open_workspace, search_input, selected_thread};
use crate::WorkspaceView;
use crate::search::SearchScopeState;

fn download_failed_shown(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> bool {
    cx.read_entity(view, |view, cx| {
        view.reader.read(cx).state().download_failed
    })
}

#[gpui::test]
fn a_failed_body_download_says_so_with_retry(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let thread_id = selected_thread(&view, cx).expect("a conversation is open");
    view.update(cx, |view, cx| {
        view.body_download_finished(thread_id, false, cx)
    });
    cx.run_until_parked();
    assert!(download_failed_shown(&view, cx));
    assert!(cx.debug_bounds("reader-download-retry").is_some());

    // Another conversation has no such banner.
    let other = harness.threads[1];
    let mailbox_id = harness.inbox_id.expect("an inbox");
    view.update(cx, |view, cx| view.show_thread(other, mailbox_id, cx));
    cx.run_until_parked();
    assert!(!download_failed_shown(&view, cx));

    // Back on it, Retry tries again (here the bodies are stored, so it
    // has nothing to fetch) and the banner goes.
    view.update(cx, |view, cx| view.show_thread(thread_id, mailbox_id, cx));
    cx.run_until_parked();
    assert!(download_failed_shown(&view, cx));
    click("reader-download-retry", cx);
    assert!(!download_failed_shown(&view, cx));

    // A download that works clears it too.
    view.update(cx, |view, cx| {
        view.body_download_finished(thread_id, false, cx);
        view.body_download_finished(thread_id, true, cx);
    });
    cx.run_until_parked();
    assert!(!download_failed_shown(&view, cx));
}

fn search_scope(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> SearchScopeState {
    cx.read_entity(view, |view, cx| {
        view.message_list.read(cx).state().search_scope.clone()
    })
}

#[gpui::test]
fn a_failed_server_search_says_results_may_be_incomplete(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Account);
    let search = search_input(&view, cx);
    focus(&search, cx);
    cx.simulate_input("invoice");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    // The account's server search fails (offline, say).
    view.update(cx, |view, cx| {
        let generation = view.search_context.generation;
        view.search_context.servers_pending = 1;
        view.server_search_finished(generation, "Work".to_owned(), false, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        search_scope(&view, cx).failed_servers().as_deref(),
        Some("Work\u{2019}s server")
    );

    // A result for an earlier search is ignored, and a new search starts
    // afresh.
    view.update(cx, |view, cx| {
        let earlier = view.search_context.generation.wrapping_sub(1);
        view.server_search_finished(earlier, "Home".to_owned(), false, cx);
    });
    assert_eq!(search_scope(&view, cx).servers_failed, ["Work"]);
    focus(&search, cx);
    cx.simulate_input(" total");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(search_scope(&view, cx).failed_servers(), None);
}

#[test]
fn failed_servers_are_named_in_a_sentence() {
    let scope = |names: &[&str]| SearchScopeState {
        servers_failed: names.iter().map(|name| (*name).to_owned()).collect(),
        ..SearchScopeState::default()
    };
    assert_eq!(scope(&[]).failed_servers(), None);
    assert_eq!(
        scope(&["Work", "Home", "Club"]).failed_servers().as_deref(),
        Some("the servers of Work, Home and Club")
    );
}
