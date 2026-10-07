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

After unchanged-thread skipping landed, measured medians:

| Benchmark | 100 threads | 1,000 threads | 5,000 threads |
|---|---|---|---|
| snapshot_first_apply (median) | 38.4 ms | 261 ms | 1.21 s |
| snapshot_no_change_remerge (median) | 25.4 ms | 24.2 ms | **120 ms** |
| list_threads_paging (median) | 642 µs | 641 µs | 644 µs |
| full_text_search (median) | 1.02 ms | 3.4 ms | 12.2 ms |
| pending_op_queue (median) | 1.62 ms | 1.63 ms | — |

For comparison, before skipping (the original 2026-10-07 baseline):

| Benchmark | 100 threads | 1,000 threads | 5,000 threads |
|---|---|---|---|
| snapshot_no_change_remerge (median) | 29.4 ms | 266 ms | 1.38 s |

## Findings

- Thread paging is flat with mailbox size; the index does its job.
- Search scales linearly with the indexed corpus, as expected for FTS5.
- **Unchanged-thread skipping (issue #60) cut the no-change re-merge by
  11.5x at 5,000 threads** (1.38 s to 120 ms) by comparing merged
  values against stored rows and skipping identical rewrites plus the
  search-index rebuild. The remaining ~120 ms is the per-thread
  comparison queries; batching them (one query per mailbox instead of
  per thread) is the next increment if steady-state syncs need to go
  lower.
