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
| Scrolling the reader down a long body | 8.3 ms per frame (120 Hz) |

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
- **Reader scroll:** 240 scroll-wheel events of 64 px down the 1 MB HTML
  body, which the reader lays out a chunk of the body per conversation
  list item.

### In CI

The `Performance budgets` job in `.github/workflows/ci.yml` runs
`performance_budgets` on Linux for pull requests that touch `zmail-ui`,
`zmail-store`, `zmail-core`, `zmail-test-support` or the lock file, and
for every push to `main`. It writes the tables to the job summary and
uploads them, with `performance.json`, as the `performance` artefact
(kept 90 days, so `main`'s runs form the trend).

The job reports; it is not the budget. Shared runners vary from run to
run, so a median only fails the job when it is more than five times its
budget (`ZMAIL_PERF_GATE=5`): it catches regressions of the
order-of-magnitude kind the baseline below found. Every measurement is
gated. Which budgets should become hard gates, and at what tolerance, is
still open.

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
- **Opening a 1 MB HTML body** took about 450 ms, of which loading the
  thread (and converting the HTML to text) was 17 ms.

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

## After the reader chunking (2026-10-21)

The reader lays out a paragraph longer than 2 KB in pieces of whole
lines, and an expanded message whose body is longer than about 6 KB
takes one conversation list item per chunk of pieces, so the list lays
out only the part on screen (#249). Opening the 1 MB HTML message went
from about 450 ms to under 40 ms, and the CI job now gates it like every
other measurement.

`main` at dfd386a, the same machine, release profile. The shared
container's load average was about 5 when the run started and about 11
when it finished (other agents were building), so treat differences under
about 30% as noise.

| Measurement | n | Median | p95 | Max | Budget | Over budget |
|---|---|---|---|---|---|---|
| Scroll frame, flick (top) | 120 | 5.41 ms | 8.80 ms | 14.19 ms | 8.33 ms | 11 |
| Scroll frame, flick (middle) | 120 | 4.61 ms | 7.33 ms | 12.94 ms | 8.33 ms | 4 |
| Scroll frame, flick (end) | 120 | 4.96 ms | 7.18 ms | 14.49 ms | 8.33 ms | 3 |
| Scroll frame, scrollbar jump | 100 | 15.54 ms | 21.59 ms | 23.45 ms | 8.33 ms | 100 |
| Open thread (Select Next) | 30 | 6.12 ms | 7.59 ms | 24.42 ms | 100 ms | 0 |
| Mark read / unread | 30 | 33.82 ms | 43.70 ms | 53.24 ms | 100 ms | 0 |
| Flag / unflag | 30 | 32.42 ms | 33.56 ms | 33.70 ms | 100 ms | 0 |
| Archive | 30 | 38.30 ms | 39.73 ms | 40.60 ms | 100 ms | 0 |
| Open folder (20k Archive) | 30 | 22.63 ms | 28.46 ms | 32.73 ms | 100 ms | 0 |
| Open folder (100k Inbox) | 30 | 32.52 ms | 40.86 ms | 44.57 ms | 100 ms | 0 |
| Search keystroke, shows in the field | 32 | 1.18 ms | 1.58 ms | 14.84 ms | 100 ms | 0 |
| Search keystroke, results after the debounce | 32 | 30.80 ms | 93.58 ms | 105.73 ms | 100 ms | 1 |
| Open reader, 100 unread messages | 10 | 7.21 ms | 8.20 ms | 8.20 ms | 100 ms | 0 |
| Open reader, 1 MB HTML body | 10 | 38.60 ms | 41.45 ms | 41.45 ms | 100 ms | 0 |
| Open reader, 60 attachments | 10 | 11.02 ms | 11.58 ms | 11.58 ms | 100 ms | 0 |
| Reader scroll frame, 1 MB HTML body | 240 | 1.23 ms | 2.12 ms | 8.12 ms | 8.33 ms | 0 |

Seeding the store took 15.3 s. With `ZMAIL_PERF_GATE=5`, as in CI, the run
passes.

## What still misses

From the run above (`main` at dfd386a):

- **Dragging the scrollbar** across a 100k mailbox: 15.5 ms a frame
  (median; p95 21.6 ms) against 8.3 ms. Each frame lands on a new page,
  and finding a page still walks the mailbox's order index up to its row
  (about 8 ms at the end). Wheel and keyboard scrolling stay within budget
  at any depth, apart from a frame now and then that reads the next page
  (p95 7–9 ms). Tracked in #242, which proposes seeking pages by a
  position index.
- **A short word typed after a full one** in a 100k folder: `budget r`
  took 107.6 ms after the debounce (the store read alone 102 ms, and
  `budget re` 82 ms), against 100 ms; it is the only search sample over
  budget, and every other keystroke took 25–45 ms. FTS5 expands `r*`
  into every indexed term starting with `r` and merges their doclists
  before the page limit applies. Tracked in #250.

Everything else in the tables is within its budget.

## The encrypted store (2026-10-21)

The store is a SQLCipher database from #139 (ADR 0005): every page is
decrypted and its MAC checked when read, and encrypted when written.
On its own that cost 10–35% on mark read, flag and archive, and 1.5–2×
on the scrollbar jump. The same change made each thread keep its store
connections, with a 16 MiB page cache each, so a call no longer opens a
connection and reads every page cold. The net result, from
`performance_budgets` on the development container (two runs each,
medians):

| Measurement | Budget | Before (plaintext) | Encrypted |
|---|---|---|---|
| Scroll frame, flick (top / middle / end) | 8.33 ms | 4.4 / 4.4 / 4.3 ms | 4.4 / 4.4 / 4.4 ms |
| Scroll frame, scrollbar jump | 8.33 ms | 12.2–12.5 ms | 10.7–11.0 ms |
| Open thread (Select Next) | 100 ms | 6.0–6.1 ms | 4.5–5.0 ms |
| Mark read / unread | 100 ms | 33.1–33.6 ms | 17.4–18.7 ms |
| Flag / unflag | 100 ms | 31.7–31.9 ms | 17.2–17.5 ms |
| Archive | 100 ms | 38.3–38.4 ms | 18.6–18.9 ms |
| Open folder (20k / 100k) | 100 ms | 13.6–13.8 / 20.6–21.3 ms | 7.5 / 14.6 ms |
| Search keystroke, results | 100 ms | 30.3 ms | 24.9–25.0 ms |
| Open reader (100 unread / 1 MB HTML / 60 attachments) | 100 ms | 7.2 / 36.4–36.6 / 11.0–11.4 ms | 4.9–5.0 / 33.3 / 9.5 ms |

ADR 0005 §5 has the measurements without the connection cache and the
store benchmarks. Re-measured with the whole of #139 in place (encrypted
attachment files too, `main` at cc1ab3b against the stack's last PR, two
runs each on a busier container, load 9–20): mark read 17.8–19.3 ms
against 34.2–36.1 ms, archive 18.7–21.1 ms against 39.1–43.1 ms, the
scrollbar jump 11.1–12.8 ms against 12.7–13.2 ms, and every other median
within noise of the table above.
