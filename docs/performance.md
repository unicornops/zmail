# Performance budgets

UI plan Phase 8 item 5 (#122) and #137 set numeric budgets for the
interactions that should feel immediate, measured against a mailbox of
100,000 conversations. This page lists the budgets, how each is measured,
and the latest numbers.

## Budgets

| Interaction | Budget |
|---|---|
| Scrolling the thread list, 100k threads | 8.3 ms per frame (120 Hz) |
| Archive, flag, mark read or unread | 100 ms from the action to the visible change |
| Opening a folder | 100 ms |
| Opening a conversation (the next one, from the list) | 100 ms |
| A keystroke in the search field | 100 ms |
| Opening the reader on a heavy conversation | 100 ms |

Which budgets become hard CI gates, and with what tolerance, is still open
(see #137).

## How it is measured

### UI: `performance_budgets`

```sh
cargo test --release -p zmail-ui --lib performance_budgets -- --ignored --nocapture
```

`crates/zmail-ui/src/view_tests/perf.rs` opens the real `WorkspaceView` in a
GPUI test window, the same harness as the view tests, over an on-disk
ephemeral store seeded with:

- an Inbox of 100,000 one-message conversations from
  `SyntheticMailboxSpec` (every body stored), plus the three
  `reader_stress_threads` from `zmail-test-support`: 100 unread messages
  of about 2 KB each, one message whose only body is 1 MB of HTML, and one
  message with 60 attachments;
- an Archive of 20,000 conversations and an empty Trash.

`ZMAIL_PERF_THREADS` changes the Inbox's size. The window is 1440 × 920,
the app's default, in the three-pane layout.

The test window uses the platform's real text system (cosmic-text on
Linux, Core Text on macOS, DirectWrite on Windows) instead of the test
platform's no-op one, so text is shaped and glyphs are rasterised as in
the app. Each sample is the wall time of one step on the main thread: the
input event or action, its handlers and store reads, then one window frame
(render, layout, prepaint and paint into the scene), until the executor is
idle. Background tasks (the search read) run on the same thread under the
test dispatcher, so their time is included.

That is the CPU side of a frame. The test platform has no GPU, so a sample
leaves out drawing the scene, presenting it, and the platform's event
delivery: it is a lower bound on the time to a visible change. "Frame
time" below means this CPU time, not a measured frame rate.

- **List scroll:** 120 scroll-wheel events of 64 px over the thread list
  (one second of a fast flick at 120 Hz) near the top, the middle and the
  end of the Inbox; and 100 frames that each jump far down the list, as
  dragging the scrollbar does.
- **Actions:** Select Next (opens the next conversation), Toggle Read,
  Toggle Flag and Archive, dispatched as the key bindings do, 30 times
  each; opening the Archive and the Inbox from the sidebar's handler, 30
  times each; and typing three queries a character at a time. A keystroke
  shows in the search field in its own frame; the results follow after
  the deliberate 150 ms `SEARCH_DEBOUNCE` pause, so the "results" row
  times the work after the pause, not the pause.
- **Reader open:** each stress conversation opened 10 times, from another
  conversation each time so it loads and lays out from scratch.

### Store: `cargo bench -p zmail-store`

The divan benchmarks in `crates/zmail-store/benches/store.rs` (see
[`benches/README.md`](../benches/README.md)) include the store side of the
budgets over the same 100k Inbox: a list page at three depths
(`large_mailbox_page`), the counts every refresh after a change reads
(`large_mailbox_refresh_counts`), and loading each stress conversation
(`thread_view_stress`).

```sh
cargo bench -p zmail-store --bench store -- large_mailbox thread_view_stress
```

## Baseline (2026-10-20)

AMD Ryzen 5 3600 (6 cores, 12 threads), Linux dev container, release
profile, Rust 1.98.1, `main` at 6b2ced9 (0.14.0). The container is shared:
other builds and test runs were going on at the same time (load average
about 9), so treat differences under about 30% as noise. An earlier run
of the same code gave medians 10–40% lower for the actions and the same
picture everywhere else.

### UI

| Measurement | n | Median | p95 | Max | Budget | Over budget |
|---|---|---|---|---|---|---|
| Scroll frame, flick (top) | 120 | 4.81 ms | 15.34 ms | 18.03 ms | 8.33 ms | 19 |
| Scroll frame, flick (middle) | 120 | 903 ms | 1,216 ms | 1,314 ms | 8.33 ms | 120 |
| Scroll frame, flick (end) | 120 | 1,798 ms | 1,894 ms | 2,276 ms | 8.33 ms | 120 |
| Scroll frame, scrollbar jump | 100 | 1,099 ms | 2,752 ms | 3,425 ms | 8.33 ms | 100 |
| Open thread (Select Next) | 30 | 11.36 ms | 15.58 ms | 34.48 ms | 100 ms | 0 |
| Mark read / unread | 30 | 162.63 ms | 236.68 ms | 302.41 ms | 100 ms | 30 |
| Flag / unflag | 30 | 135.17 ms | 232.42 ms | 234.36 ms | 100 ms | 29 |
| Archive | 30 | 106.76 ms | 126.80 ms | 186.87 ms | 100 ms | 30 |
| Open folder (20k Archive) | 30 | 17.10 ms | 24.91 ms | 57.30 ms | 100 ms | 0 |
| Open folder (100k Inbox) | 30 | 26.80 ms | 36.82 ms | 51.19 ms | 100 ms | 0 |
| Search keystroke, shows in the field | 32 | 1.42 ms | 2.39 ms | 13.48 ms | 100 ms | 0 |
| Search keystroke, results after the debounce | 32 | 214.75 ms | 338.11 ms | 388.09 ms | 100 ms | 31 |
| Open reader, 100 unread messages | 10 | 7.97 ms | 12.09 ms | 12.09 ms | 100 ms | 0 |
| Open reader, 1 MB HTML body | 10 | 447.90 ms | 665.48 ms | 665.48 ms | 100 ms | 10 |
| Open reader, 60 attachments | 10 | 12.97 ms | 14.08 ms | 14.08 ms | 100 ms | 0 |

Seeding the store took 14.1 s.

### Store

| Benchmark | Median |
|---|---|
| `large_mailbox_page` at row 0 | 1.14 ms |
| `large_mailbox_page` at row 50,000 | 40.58 ms |
| `large_mailbox_page` at row 99,850 | 85.09 ms |
| `large_mailbox_refresh_counts` | 25.31 ms |
| `thread_view_stress`, long conversation | 1.25 ms |
| `thread_view_stress`, 1 MB HTML body | 17.38 ms |
| `thread_view_stress`, 60 attachments | 0.76 ms |

### What missed

- **Scrolling away from the top of a large mailbox.** Frames took about a
  second in the middle of the Inbox and almost two at its end. A page of
  rows was read with `LIMIT … OFFSET` joined to every skipped row (40 ms
  at row 50,000), and every scroll frame read four pages: the list
  renders row 0 to measure the row height, which evicted the cached page
  on screen, and the frame renders twice (the row under the pointer
  changes).
- **Archive, flag and mark read** took 100–160 ms: the store refresh after
  each change counted the Flagged virtual mailbox by scanning every thread
  (45–65 ms), and read store-wide totals nothing showed (8 ms).
- **Search results** took about 200 ms of work, on top of the 150 ms
  debounce: a folder-scoped search listed the folder's 100k threads as an
  `IN` list, then read and sorted every match before the first page.
- **Opening a 1 MB HTML body** takes about 450 ms, of which loading the
  thread (and converting the HTML to text) is 17 ms.

## After the fixes (2026-10-20)

The same runs with the fixes in #122's second PR:

- The list reads a page's threads from `threads` alone, so skipping to a
  deep page walks the mailbox's order index (a page at row 99,850: 85 ms
  to 8 ms), and the list keeps row 0 in its own cache, so a scroll frame
  reads the store only when it reaches a new page.
- A partial index over the flagged threads (migration
  `0021_flagged_threads_index`) covers the Flagged count, and the refresh
  no longer reads the store totals.
- A folder-scoped search filters on the thread's mailbox directly, so it
  walks the folder's newest-first index and stops at the page.

This run had a higher load on the shared container (load average about
12) than the baseline. An earlier, quieter run of the same fixes gave
scroll medians of 4.4 ms, 32–40 ms for read, flag and archive, and 13 ms
per scrollbar jump.

| Measurement | n | Median | p95 | Max | Budget | Over budget |
|---|---|---|---|---|---|---|
| Scroll frame, flick (top) | 120 | 5.83 ms | 8.08 ms | 52.75 ms | 8.33 ms | 5 |
| Scroll frame, flick (middle) | 120 | 6.37 ms | 8.78 ms | 13.93 ms | 8.33 ms | 8 |
| Scroll frame, flick (end) | 120 | 6.49 ms | 7.68 ms | 18.26 ms | 8.33 ms | 2 |
| Scroll frame, scrollbar jump | 100 | 18.27 ms | 22.61 ms | 27.07 ms | 8.33 ms | 100 |
| Open thread (Select Next) | 30 | 10.01 ms | 11.63 ms | 33.58 ms | 100 ms | 0 |
| Mark read / unread | 30 | 54.25 ms | 67.46 ms | 84.62 ms | 100 ms | 0 |
| Flag / unflag | 30 | 47.56 ms | 55.89 ms | 61.70 ms | 100 ms | 0 |
| Archive | 30 | 60.93 ms | 84.55 ms | 94.68 ms | 100 ms | 0 |
| Open folder (20k Archive) | 30 | 19.33 ms | 28.37 ms | 31.72 ms | 100 ms | 0 |
| Open folder (100k Inbox) | 30 | 27.87 ms | 48.71 ms | 69.45 ms | 100 ms | 0 |
| Search keystroke, shows in the field | 32 | 1.21 ms | 1.92 ms | 11.53 ms | 100 ms | 0 |
| Search keystroke, results after the debounce | 32 | 30.09 ms | 86.10 ms | 108.12 ms | 100 ms | 1 |
| Open reader, 100 unread messages | 10 | 6.22 ms | 6.80 ms | 6.80 ms | 100 ms | 0 |
| Open reader, 1 MB HTML body | 10 | 435.21 ms | 599.18 ms | 599.18 ms | 100 ms | 10 |
| Open reader, 60 attachments | 10 | 12.77 ms | 13.44 ms | 13.44 ms | 100 ms | 0 |

| Store benchmark | Median |
|---|---|
| `large_mailbox_page` at row 0 | 1.30 ms |
| `large_mailbox_page` at row 50,000 | 4.85 ms |
| `large_mailbox_page` at row 99,850 | 7.93 ms |
| `large_mailbox_refresh_counts` | 8.73 ms |

### What still misses

- **Dragging the scrollbar** across a 100k mailbox: each frame lands on a
  new page, and finding a page still walks the index up to its row (up to
  8 ms at the end), so these frames take 13–18 ms. Scrolling with the
  wheel or keys stays within the frame budget at any depth, apart from a
  frame now and then that reads the next page.
- **Opening a 1 MB HTML body** (about 450 ms). The HTML becomes one
  537 KB paragraph of text, which the reader lays out in full in the
  first frame: the conversation list only skips messages that are off
  screen, not paragraphs. Fixing it means laying out long bodies in
  pieces (or showing the start of a very long body first), which is a
  reader change.
- **Search**: a short prefix that expands to many index terms
  (`budget r`) can still take about 100 ms.
