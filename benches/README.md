# Store benchmark harness

`cargo bench -p zmail-store` runs the divan-based benchmarks in
`crates/zmail-store/benches/store.rs`. They are deliberately not part of
CI so the quality job stays fast; run them manually around store or
sync changes.

## What is measured

- `snapshot_first_apply` — applying a snapshot to a fresh store (the
  full insert path, plus the FTS index rebuild).
- `snapshot_no_change_remerge` — re-applying the identical snapshot
  (the steady-state cost of every sync with no server changes).
- `list_threads_paging` — one 50-thread page read.
- `full_text_search` — one FTS5 query.
- `pending_op_queue` — listing and counting write-back queue rows.

## Baseline (2026-10-07, 12-core Linux dev container, release profile)

| Benchmark | 100 threads | 1,000 threads | 5,000 threads |
|---|---|---|---|
| snapshot_first_apply (median) | 36.8 ms | 256 ms | 1.16 s |
| snapshot_no_change_remerge (median) | 29.4 ms | 266 ms | 1.38 s |
| list_threads_paging (median) | 640 µs | 643 µs | 653 µs |
| full_text_search (median) | 1.01 ms | 3.31 ms | 12.6 ms |
| pending_op_queue (median) | 1.64 ms | 1.62 ms | — |

## Findings

- Thread paging is flat with mailbox size; the index does its job.
- Search scales linearly with the indexed corpus, as expected for FTS5.
- **A no-change re-merge costs the same as a first apply**: the merge
  rewrites every thread's messages and rebuilds the search index even
  when the snapshot is identical. Skipping unchanged threads (compare
  a content hash before rewriting) is the obvious follow-up and would
  make steady-state syncs near-free.
