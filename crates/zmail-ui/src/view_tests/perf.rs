//! Performance budget runs (UI plan Phase 8 item 5, issues #122 and #137)
//! over a seeded mailbox of 100k conversations.
//!
//! The runs are ignored by default: they seed a large store and only mean
//! something in a release build.
//!
//! ```sh
//! cargo test --release -p zmail-ui --lib performance_budgets -- --ignored --nocapture
//! ```
//!
//! `ZMAIL_PERF_THREADS` overrides the Inbox's size (default 100,000).
//! `docs/performance.md` has the budgets and the recorded baseline.
//!
//! # What is measured
//!
//! The real `WorkspaceView` runs in a GPUI test window, as in the other
//! view tests, but with the platform's real text system (cosmic-text on
//! Linux, Core Text on macOS, DirectWrite on Windows) in place of the test
//! platform's no-op one, so text is shaped and glyphs are rasterised as in
//! the app. Every sample is the wall time of one step on the main thread:
//! the event or action, its handlers and store reads, then one window frame
//! (render, layout, prepaint and paint into the scene), until the executor
//! is idle. That is the CPU side of a frame. It leaves out what the test
//! platform has no part in: the GPU drawing the scene, presenting it, and
//! the platform's event delivery. A sample is therefore a lower bound on
//! the time to a visible change, not a frame rate.
//!
//! - **List scroll** (budget 8.3 ms, a 120 Hz frame): one scroll-wheel
//!   event of 64 px over the thread list (about 7,700 px/s at 120 Hz),
//!   flicking near the top, the middle and the end of the Inbox; and a far
//!   jump per frame, as dragging the scrollbar does.
//! - **Actions** (budget 100 ms): Archive, Toggle Flag, Toggle Read and
//!   Select Next (which opens the next conversation) dispatched as the key
//!   bindings do, opening a folder from the sidebar, and a keystroke in the
//!   search field. The keystroke shows in the field in its own frame; the
//!   results follow after the deliberate `SEARCH_DEBOUNCE` pause, so the
//!   results row times only the work after the pause.
//! - **Reader open** (budget 100 ms): opening a conversation of 100 unread
//!   messages (all expanded), one with a 1 MB HTML body, and one with 60
//!   attachments.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AppContext as _, Entity, Modifiers, Pixels, Point, ScrollDelta, ScrollStrategy,
    ScrollWheelEvent, TestAppContext, TestDispatcher, TouchPhase, VisualTestContext, point, px,
    size,
};
use zmail_core::{AccountSyncSnapshot, MailboxSnapshot};
use zmail_store::LocalStore;
use zmail_test_support::{
    STRESS_ATTACHMENTS_SUBJECT, STRESS_HTML_SUBJECT, STRESS_LONG_SUBJECT, SyntheticMailboxSpec,
    reader_stress_threads,
};

use super::{Setup, open_workspace_with};
use crate::WorkspaceView;
use crate::actions::{Archive, FocusSearch, SelectNext, ToggleFlag, ToggleRead};
use crate::pane::Pane;
use crate::search::SEARCH_DEBOUNCE;
use crate::styles::{MESSAGE_LIST_WIDTH, SIDEBAR_WIDTH, WINDOW_HEIGHT, WINDOW_WIDTH};

/// A 120 Hz frame.
const FRAME_BUDGET: Duration = Duration::from_micros(8_333);
/// From an action to the visible change.
const ACTION_BUDGET: Duration = Duration::from_millis(100);

/// Timings of one measurement.
struct Samples {
    name: String,
    budget: Duration,
    times: Vec<Duration>,
}

impl Samples {
    fn new(name: impl Into<String>, budget: Duration) -> Self {
        Self {
            name: name.into(),
            budget,
            times: Vec::new(),
        }
    }

    /// Times `step` and records it.
    fn time<T>(&mut self, step: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let value = step();
        self.times.push(started.elapsed());
        value
    }

    fn sorted(&self) -> Vec<Duration> {
        let mut times = self.times.clone();
        times.sort();
        times
    }

    /// The `percent` percentile (nearest rank).
    fn percentile(&self, percent: usize) -> Duration {
        let times = self.sorted();
        let rank = (times.len() * percent).div_ceil(100).max(1);
        times[rank - 1]
    }

    fn max(&self) -> Duration {
        self.sorted().last().copied().unwrap_or_default()
    }

    fn over_budget(&self) -> usize {
        self.times
            .iter()
            .filter(|time| **time > self.budget)
            .count()
    }

    fn row(&self) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            self.name,
            self.times.len(),
            ms(self.percentile(50)),
            ms(self.percentile(95)),
            ms(self.max()),
            ms(self.budget),
            self.over_budget(),
        )
    }
}

fn ms(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1_000.)
}

fn print_table(title: &str, samples: &[Samples]) {
    println!("\n{title}\n");
    println!("| measurement | n | median | p95 | max | budget | over budget |");
    println!("|---|---|---|---|---|---|---|");
    for sample in samples {
        println!("{}", sample.row());
    }
}

/// A test context whose text system is the platform's own, so text is
/// really shaped and rasterised.
fn context_with_platform_text() -> TestAppContext {
    let text_system = gpui_platform::current_platform(true).text_system();
    let cx = TestAppContext::build_with_text_system(TestDispatcher::new(0), None, text_system);
    cx.update(crate::fonts::register_ui_font);
    cx
}

/// The Inbox (`thread_count` synthetic one-message conversations plus the
/// reader conversations, newest), a 20k-thread Archive and an empty Trash.
fn perf_snapshot(thread_count: usize) -> AccountSyncSnapshot {
    let synthetic = |threads_per_mailbox, seed| {
        let spec = SyntheticMailboxSpec {
            mailbox_count: 1,
            threads_per_mailbox,
            messages_per_thread: 1,
            average_body_bytes: 600,
        };
        // Every body stored, so opening a thread never asks the server.
        spec.snapshot(seed, 1)
            .mailboxes
            .into_iter()
            .next()
            .expect("one mailbox")
    };
    let mut inbox = synthetic(thread_count, 7);
    inbox.name = "INBOX".to_owned();
    inbox.threads.extend(reader_stress_threads());
    let mut archive = synthetic(20_000, 11);
    archive.name = "Archive".to_owned();
    archive.role = Some("archive".to_owned());
    for thread in &mut archive.threads {
        thread.remote_thread_id = format!("archive-{}", thread.remote_thread_id);
        for message in &mut thread.messages {
            message.remote_id = format!("archive-{}", message.remote_id);
        }
    }
    AccountSyncSnapshot {
        checkpoint: "perf".to_owned(),
        detail: "perf".to_owned(),
        identities: None,
        mailboxes: vec![
            inbox,
            archive,
            MailboxSnapshot {
                name: "Trash".to_owned(),
                role: Some("trash".to_owned()),
                ..Default::default()
            },
        ],
    }
}

/// The seeded mailboxes and conversations the runs use.
struct Seeded {
    inbox: i64,
    archive: i64,
    long: i64,
    html: i64,
    attachments: i64,
}

fn seeded_ids(store: &LocalStore) -> Seeded {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    let role = |role: &str| {
        store
            .mailbox_id_by_role(account_id, role)
            .expect("the mailbox lookup works")
            .expect("the mailbox exists")
    };
    let inbox = role("inbox");
    let newest = store.list_threads(inbox, 0..3).expect("the threads load");
    let id_of = |subject: &str| {
        newest
            .iter()
            .find(|thread| thread.subject == subject)
            .map(|thread| thread.id)
            .expect("the reader conversations are the newest")
    };
    Seeded {
        inbox,
        archive: role("archive"),
        long: id_of(STRESS_LONG_SUBJECT),
        html: id_of(STRESS_HTML_SUBJECT),
        attachments: id_of(STRESS_ATTACHMENTS_SUBJECT),
    }
}

/// How far the list is scrolled, in rows.
fn list_top(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> f32 {
    cx.read_entity(view, |view, cx| {
        let list = view.message_list.read(cx);
        let rows = list
            .state()
            .mailbox
            .as_ref()
            .map_or(1, |mailbox| mailbox.total_threads.max(1));
        let scroll = list.thread_scroll().0.borrow();
        let row_height = scroll
            .last_item_size
            .map_or(px(1.), |size| size.contents.height / rows as f32);
        -scroll.base_handle.offset().y / row_height
    })
}

/// Scrolls the list so `row` is at the top, and draws.
fn jump_list_to(view: &Entity<WorkspaceView>, row: usize, cx: &mut VisualTestContext) {
    let list = cx.read_entity(view, |view, _| view.message_list.clone());
    list.update(cx, |list, cx| {
        list.thread_scroll()
            .scroll_to_item_strict(row, ScrollStrategy::Top);
        cx.notify();
    });
    cx.run_until_parked();
}

/// A point over the thread list in the three-pane layout.
fn over_list() -> Point<Pixels> {
    point(
        px(SIDEBAR_WIDTH + MESSAGE_LIST_WIDTH / 2.),
        px(WINDOW_HEIGHT / 2.),
    )
}

fn scroll_wheel(dy: f32, cx: &mut VisualTestContext) {
    cx.simulate_event(ScrollWheelEvent {
        position: over_list(),
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
}

/// (a) Scroll frames over the thread list.
fn measure_list_scroll(
    view: &Entity<WorkspaceView>,
    total: usize,
    cx: &mut VisualTestContext,
) -> Vec<Samples> {
    /// One second at 120 Hz.
    const FRAMES: usize = 120;
    let mut results = Vec::new();
    for (label, start) in [
        ("top", 0),
        ("middle", total / 2),
        ("end", total.saturating_sub(FRAMES * 2)),
    ] {
        jump_list_to(view, start, cx);
        // Scrolling goes to the hovered element.
        cx.simulate_mouse_move(over_list(), None, Modifiers::none());
        // Warm the glyph and line caches the way a running app has them.
        for _ in 0..20 {
            scroll_wheel(-64., cx);
        }
        let before = list_top(view, cx);
        let mut samples = Samples::new(format!("scroll frame, flick ({label})"), FRAME_BUDGET);
        for _ in 0..FRAMES {
            samples.time(|| scroll_wheel(-64., cx));
        }
        let after = list_top(view, cx);
        assert!(
            after > before,
            "the list scrolled ({label}: row {before} to {after})"
        );
        println!("flick ({label}): rows {before:.0} to {after:.0}");
        results.push(samples);
    }

    // Dragging the scrollbar: every frame lands far from the last, so the
    // cached page of rows never covers it.
    let mut samples = Samples::new("scroll frame, scrollbar jump", FRAME_BUDGET);
    for step in 0..100 {
        let row = (step * 7_919) % total.max(1);
        samples.time(|| jump_list_to(view, row, cx));
    }
    results.push(samples);
    results
}

fn select_inbox_row(
    view: &Entity<WorkspaceView>,
    seeded: &Seeded,
    row: usize,
    cx: &mut VisualTestContext,
) {
    let store = cx.read_entity(view, |view, _| view.store.clone());
    let thread = store
        .list_threads(seeded.inbox, row..row + 1)
        .expect("the threads load")[0]
        .id;
    let inbox = seeded.inbox;
    view.update(cx, |view, cx| view.select_thread(thread, inbox, cx));
    cx.run_until_parked();
}

fn focus_list(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::MessageList, window, cx)
    });
    cx.run_until_parked();
}

/// Dispatches `action` and waits for its frame.
fn act(action: impl gpui::Action, cx: &mut VisualTestContext) {
    cx.dispatch_action(action);
    cx.run_until_parked();
}

/// (b) Action to visible change.
fn measure_actions(
    view: &Entity<WorkspaceView>,
    seeded: &Seeded,
    cx: &mut VisualTestContext,
) -> Vec<Samples> {
    const RUNS: usize = 30;
    let mut results = Vec::new();
    view.update(cx, |view, cx| view.select_mailbox(seeded.inbox, cx));
    cx.run_until_parked();
    select_inbox_row(view, seeded, 10, cx);
    focus_list(view, cx);

    let mut next = Samples::new("open thread (Select Next)", ACTION_BUDGET);
    for _ in 0..RUNS {
        next.time(|| act(SelectNext, cx));
    }
    let mut read = Samples::new("mark read / unread (Toggle Read)", ACTION_BUDGET);
    for _ in 0..RUNS {
        read.time(|| act(ToggleRead, cx));
    }
    let mut flag = Samples::new("flag / unflag (Toggle Flag)", ACTION_BUDGET);
    for _ in 0..RUNS {
        flag.time(|| act(ToggleFlag, cx));
    }
    let mut archive = Samples::new("archive (Archive)", ACTION_BUDGET);
    for _ in 0..RUNS {
        let before = cx.read_entity(view, |view, _| view.selected_thread_id);
        archive.time(|| act(Archive, cx));
        let after = cx.read_entity(view, |view, _| view.selected_thread_id);
        assert_ne!(before, after, "archiving moved on to the next conversation");
    }
    results.extend([next, read, flag, archive]);

    let mut open_archive = Samples::new("open folder (20k-thread Archive)", ACTION_BUDGET);
    let mut open_inbox = Samples::new("open folder (100k-thread Inbox)", ACTION_BUDGET);
    for _ in 0..RUNS {
        let (archive_id, inbox_id) = (seeded.archive, seeded.inbox);
        open_archive.time(|| {
            view.update(cx, |view, cx| view.select_mailbox(archive_id, cx));
            cx.run_until_parked();
        });
        open_inbox.time(|| {
            view.update(cx, |view, cx| view.select_mailbox(inbox_id, cx));
            cx.run_until_parked();
        });
    }
    results.extend([open_archive, open_inbox]);

    let mut echo = Samples::new("search keystroke (shows in the field)", ACTION_BUDGET);
    let mut search = Samples::new(
        "search keystroke (results, after the debounce)",
        ACTION_BUDGET,
    );
    for query in ["budget review", "kittiwake", "from:alice"] {
        act(FocusSearch, cx);
        for character in query.chars() {
            let keystroke = if character == ' ' {
                "space".to_owned()
            } else {
                character.to_string()
            };
            echo.time(|| {
                cx.simulate_keystrokes(&keystroke);
                cx.run_until_parked();
            });
            search.time(|| {
                cx.executor().advance_clock(SEARCH_DEBOUNCE);
                cx.run_until_parked();
            });
        }
        let results = cx.read_entity(view, |view, _| view.search_results.len());
        assert!(results > 0, "{query} found conversations");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }
    results.extend([echo, search]);
    results
}

/// (c) Opening the reader on heavy conversations.
fn measure_reader_open(
    view: &Entity<WorkspaceView>,
    seeded: &Seeded,
    cx: &mut VisualTestContext,
) -> Vec<Samples> {
    const RUNS: usize = 10;
    view.update(cx, |view, cx| view.select_mailbox(seeded.inbox, cx));
    cx.run_until_parked();
    let mut results = Vec::new();
    for (label, thread_id) in [
        ("100 unread messages", seeded.long),
        ("1 MB HTML body", seeded.html),
        ("60 attachments", seeded.attachments),
    ] {
        let mut samples = Samples::new(format!("open reader ({label})"), ACTION_BUDGET);
        for _ in 0..RUNS {
            // Another conversation first, so each open loads and lays out
            // this one from scratch.
            select_inbox_row(view, seeded, 10, cx);
            let inbox = seeded.inbox;
            samples.time(|| {
                view.update(cx, |view, cx| view.select_thread(thread_id, inbox, cx));
                cx.run_until_parked();
            });
            let open = cx.read_entity(view, |view, cx| {
                view.reader
                    .read(cx)
                    .state()
                    .thread
                    .as_ref()
                    .map(|thread| thread.id)
            });
            assert_eq!(open, Some(thread_id), "the reader shows {label}");
        }
        results.push(samples);
    }
    results
}

#[test]
#[ignore = "seeds a 100k-thread store; run with --release (see the module docs)"]
fn performance_budgets() {
    let total = std::env::var("ZMAIL_PERF_THREADS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);
    let mut app = context_with_platform_text();
    let started = Instant::now();
    let snapshot = perf_snapshot(total);
    let mut seed_time = Duration::ZERO;
    let (view, cx, harness) = open_workspace_with(
        &mut app,
        Setup::Account,
        |_| crate::InitialView::Inbox,
        |store| {
            let account_id = store.list_accounts().expect("the accounts load")[0].id;
            store
                .replace_account_snapshot(account_id, &snapshot)
                .expect("the mail is stored");
            seed_time = started.elapsed();
        },
    );
    let store: Arc<LocalStore> = harness.store.clone();
    let seeded = seeded_ids(&store);
    println!(
        "seeded {total} Inbox threads (+20k Archive) in {seed_time:.1?}; window opened after {:.1?}",
        started.elapsed()
    );
    cx.simulate_resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
    cx.run_until_parked();
    view.update(cx, |view, cx| view.select_mailbox(seeded.inbox, cx));
    cx.run_until_parked();

    let scroll = measure_list_scroll(&view, total, cx);
    print_table(
        &format!("List scroll, {total} threads (CPU time per frame)"),
        &scroll,
    );
    let actions = measure_actions(&view, &seeded, cx);
    print_table("Action to visible change (CPU time)", &actions);
    let reader = measure_reader_open(&view, &seeded, cx);
    print_table("Reader open (CPU time)", &reader);

    drop(harness);
}
