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

## Writing a stretch of older Proton mail (2026-11-16, #478)

A Proton account's older mail arrives in stretches of 1,024 messages
(8 pages of 128, metadata only), each written with
`LocalStore::merge_account_snapshot`, or with
`LocalStore::apply_mail_delta` while the history is listed again after a
refresh request (ADR 0004). #478 reported the download slowing down as
the mirror grew. This is what one stretch costs to write, and where the
time goes.

### How it is measured

```sh
ZMAIL_BACKFILL_SIZES=10000,100000,1000000 cargo bench -p zmail-store --bench backfill
```

`crates/zmail-store/benches/backfill.rs` grows a synthetic account in an
encrypted store on disk to each size in turn and writes the next
stretches into it, timing each phase of the write
(`zmail_store::merge_profile`). The account is shaped like a Proton
mailbox (`zmail_test_support::proton_mirror_stretch`): every message is
in All Mail and in one of Inbox, Archive, Sent, Trash or Spam, one in
four also carries one of six labels, so a million messages are 2.25
million message rows in 2.03 million threads; a fifth of the messages
are in conversations of two to six, some of which straddle two
stretches; mail comes from 4,000 contacts (a few of them most of the
time) and 20,000 bulk senders, and 12% of it is sent mail to one to
three contacts. Messages are metadata only, as the backfill lists them:
no bodies. Without `ZMAIL_BACKFILL_SIZES` it runs 10,000 and 100,000,
which takes about 20 seconds; the million takes about three minutes
(over six before the fix).

AMD Ryzen 5 3600 (6 cores, 12 threads), Linux dev container, release
profile, Rust 1.98.1. All times are milliseconds for one stretch of
1,024 messages; the merge is the median of three stretches, the deltas
one run each. The container was quiet for the "after" run (load
average under 2) and shared with a compile for part of the "before"
run; treat differences under about 20% as noise (the commit alone
varied between 60 and 130 ms).

### Before (`main` at ff1de94, 0.34.2, with the timing added)

`merge_account_snapshot`, 1,024 messages new to the mirror:

| Mirror | match stored threads | compare and write | mailbox counters | address history | search index | commit | other | total |
|---|---|---|---|---|---|---|---|---|
| 10,000 | 10.2 | 78.5 | 5.2 | 47.2 | 10.9 | 71.3 | 1.1 | 224 |
| 100,000 | 73.3 | 76.8 | 125 | 610 | 11.5 | 81.4 | 7.7 | 985 |
| 1,000,000 | 840 | 79.2 | 1,500 | 6,438 | 11.3 | 72.2 | 175 | 9,115 |

`apply_mail_delta`, the same kind of stretch as a relist writes it:

| Mirror | 1,024 messages new to the mirror | 1,024 messages already held, unchanged |
|---|---|---|
| 10,000 | 273 | 196 |
| 100,000 | 976 | 268 |
| 1,000,000 | 9,277 | 1,208 |

Three steps read the whole mirror on every stretch, and each grew in
step with it:

- **The address history rebuild**, #478's main suspect: 62% of the write
  at 100,000 messages and 71% at a million. It grouped every message of
  the account by its address headers.
- **The mailbox counters**: 13% and 16%. Each changed mailbox's threads
  were counted twice, once for the total and once for unread, and a
  stretch changes the largest mailboxes (All Mail, Archive, Inbox).
- **Matching the stored threads**: 7% and 9%, which #478 did not list.
  The merge loaded the id of every thread in each mailbox it touched to
  find the few it needed. Most of "other" at a million was dropping that
  map. It is also all of the growth of the unchanged relist, which
  skips the other two.

Writing the messages, the search index and the commit did not grow.
The write of a stretch took about 0.13 s plus 9 s per million messages
mirrored, so a backfill was quadratic: about 2.2 hours of writing in all
for 1.32 million messages.

### After

`merge_account_snapshot`, 1,024 messages new to the mirror:

| Mirror | match stored threads | compare and write | mailbox counters | address history | search index | commit | other | total |
|---|---|---|---|---|---|---|---|---|
| 10,000 | 3.1 | 77.0 | 0.06 | 4.6 | 10.6 | 74.9 | 0.7 | 171 |
| 100,000 | 3.4 | 78.4 | 0.07 | 6.5 | 10.7 | 82.9 | 0.7 | 183 |
| 1,000,000 | 3.4 | 78.2 | 0.06 | 5.6 | 10.7 | 109 | 0.7 | 208 |

`apply_mail_delta`:

| Mirror | 1,024 messages new to the mirror | 1,024 messages already held, unchanged |
|---|---|---|
| 10,000 | 190 | 193 |
| 100,000 | 187 | 192 |
| 1,000,000 | 246 | 195 |

A stretch now costs about 0.2 s to write whatever the mirror holds
(9.1 s to 0.2 s at a million messages), about four and a half minutes in
all for 1.32 million. The only phase that still moved between 10,000
and a million is the commit (75 to 109 ms, within its run-to-run
spread). The store's other calls of one backfill sync (status,
progress, checkpoints, the mailbox list) take 1.5 ms together at every
size.

What changed (ADR 0004 has the reasoning): stored threads are found one
by one through the index; counters move by what the write added; and
the address history adds the addresses of the added message rows
instead of rebuilding, which gives the same rows because its counts are
sums and its last use and display name are maxima. A write that removes
or changes mail still counts and rebuilds.

### In CI

`merge::incremental_tests` in `zmail-store` runs with the ordinary
tests, so on every pull request:

- `writing_a_stretch_costs_the_same_in_a_small_and_a_large_mirror`
  counts the SQLite virtual machine steps of writing a stretch into
  mirrors of 2,048 and 32,768 messages, for the merge and for both
  kinds of delta, and fails if the larger takes more than a quarter
  more. It counts work, not time, so it does not depend on the runner:
  the same build gives the same counts (821,965 and 817,408 steps for
  the merge). Counting and rebuilding on every stretch, as before, takes
  1.37 million and 4.24 million steps, and the test checks that too, so
  the measure is known to see the regression it guards against.
- `any_sequence_of_writes_matches_counting_and_rebuilding_every_time`
  applies random sequences of stretches, event batches (new mail,
  moves, deletions, read changes, a message back under a new id, a
  changed sender, removed mailboxes, changed addresses), full resyncs
  and local deletes, moves and read changes to two stores, one of which
  counts and rebuilds after every write as before, and compares the
  counters and every address history row after each step.

### What this does not show

- **The download rate of a real account.** #478's figures (57 to 66
  seconds for each stretch between 65,000 and 265,000 messages) are far
  more than the write cost measured here: about 0.7 s at 65,000 and
  2.5 s at 265,000 before the fix, on this machine and this synthetic
  data. So at those sizes most of each stretch was spent elsewhere,
  which leaves the requests (a stretch is the event poll, the addresses,
  the labels and 8 metadata pages, one after another) and their retries;
  nothing here measures them (the next section does). The write would have reached about 12 s
  a stretch by 1.32 million messages. Real mail has longer headers than
  the synthetic account and some stored bodies, so its rebuild was
  probably slower than measured here, but not by the factor of 20 the
  observed times would need.
- **Full snapshots.** `replace_account_snapshot`, which IMAP and JMAP
  syncs and a Proton first sync use, still rebuilds the address history
  and counts the mailboxes when mail changed; #478 does not change it.
  Writing a mirror of about 104,000 messages again with one new message
  took 8.6 s here, of which the rebuild was 0.63 s and comparing every
  stored thread with the snapshot 7.7 s (one set of queries per thread,
  as `benches/README.md` notes).
- **A write that removes mail** (a message moved or deleted on another
  device arrives as an event) still rebuilds the address history, about
  6.5 s at a million messages, and counts the threads of each mailbox
  the message left, as before.


## Fetching a stretch of older Proton mail (2026-11-28, #478)

The write still does not grow. Two runs of the bench on `main` at
e62cca4 (0.39.0), same machine, shared with other work (load average 4
to 9), milliseconds for one stretch:

| Mirror | merge, run 1 | merge, run 2 | delta of new mail, run 1 | run 2 | unchanged relist, run 1 | run 2 |
|---|---|---|---|---|---|---|
| 10,000 | 173 | 170 | 185 | 185 | 192 | 193 |
| 100,000 | 233 | 181 | 468 | 190 | 201 | 194 |
| 1,000,000 | 208 | 209 | 250 | 247 | 195 | 196 |

The two outliers at 100,000 are the commit (101 and 335 ms against 83
and 61 ms in the other run), which is noise on a busy machine; every
other phase matches the "After" table above.

The write is about 0.2 s of a stretch. #478 observed 57 to 66 s
for each stretch, so nearly all of it is the requests, and nothing timed
them. Now each stretch keeps what it cost in the account's checkpoint,
and the terminal UI shows it under the account's sync details (Setup →
Accounts) while older mail arrives:

```text
Last stretch : 1,016 messages in 16.6 s, writing 0.2 s; about 3,331 a minute
Its requests : 13 in 16.1 s: events 0.3 s, addresses and labels 12.5 s, 8 pages 3.3 s; 1 rate limited, 12.0 s waiting to retry
```

- **Last stretch** is the whole sync that fetched and wrote it, from the
  event poll to the stored checkpoint. The rate divides its messages by
  that time plus the time since the stretch before ended (the 1.5 s
  pause and anything else the sync service did in between), so it is
  the download rate as it runs. It is left out after a gap of more than
  ten minutes (Zmail was closed) and for the first stretch.
- **Its requests** splits the requests into the event poll the sync
  starts with, the addresses and the three label types the stretch
  places its messages with, and the metadata pages. A request retried
  after rate limiting or a dropped connection counts once; a page Proton
  answers as stale and that is asked for again counts again, so more
  than 8 pages means stale answers. "Rate limited" counts 429 and 503
  answers; the wait is the time spent before retries (`Retry-After`, or
  10 s plus up to 10 s of jitter without one, at most a minute each).

The counts come from `zmail_proton_api::tally`, which counts the
requests of the future it runs in a task-local, so a message opened or
another account syncing on the same connection at the same time is not
in them. `a_backfill_stretch_records_its_requests_and_rate_limiting`
in `zmail-sync` runs a stretch against the mock Proton server, with one
429, and checks what is stored.

### What to expect

A stretch with the default paging is 13 requests (one event poll, the
addresses, three label types, eight pages of 128) for 1,016 new
messages (each page repeats the cursor message), then a write of about
0.2 s and the 1.5 s pause, one after another. So a stretch takes about
1.7 s plus its requests, and 1,000,000 messages are 985 stretches:

| Time per request | Rate limited answers per stretch | Stretch | Messages a minute | 1,000,000 messages |
|---|---|---|---|---|
| 0.25 s | none | 5.0 s | about 12,300 | 1.4 hours |
| 0.5 s | none | 8.2 s | about 7,400 | 2.2 hours |
| 1 s | none | 14.7 s | about 4,100 | 4.0 hours |
| 0.5 s | 1, waiting 15 s on average | 23.7 s | about 2,600 | 6.5 hours |
| 0.5 s | 3 | 54.7 s | about 1,100 | 15 hours |

The times per request are assumptions, not measurements: nothing here
reaches Proton. The observed 57 to 66 s a stretch fit either three or
four rate-limited answers a stretch, or 4.3 to 4.9 s for each request
with none; the sync details now say which. The live figure for a large
account is still to be recorded here.

What is left to decide once it is (#478): not reading the addresses and
labels again for every stretch (4 of the 13 requests; label and address
changes arrive as events), more pages per stretch, or a shorter pause.
Bridge's own metadata stage lists sequential pages of 128 too, so
listing in parallel is out. None of these is changed yet.
