//! What it costs to write one stretch of a Proton account's older mail
//! into mirrors of different sizes (#478), phase by phase.
//!
//! ```sh
//! cargo bench -p zmail-store --bench backfill
//! ZMAIL_BACKFILL_SIZES=10000,100000,1000000 cargo bench -p zmail-store --bench backfill
//! ```
//!
//! The mirror is a synthetic account shaped like a Proton mailbox
//! (`zmail_test_support::proton_mirror_stretch`) in an encrypted store on
//! disk. It is grown to each size in turn; at each size three stretches
//! of 1,024 messages are merged as the backfill merges them, then one is
//! written as a relist delta that changes nothing, and one as a delta of
//! mail the mirror does not hold. The store's other calls of one backfill
//! sync (status, progress, checkpoints) are timed together. Up to
//! 100,000 messages the whole mirror plus one new message is also written
//! as a full snapshot, the way IMAP and JMAP syncs write. The tables are
//! Markdown, for `docs/performance.md`. Not part of CI: the scaling guard
//! that is lives in the store's tests (`merge::incremental_tests`).

use std::time::{Duration, Instant};

use zmail_core::{ProtocolKind, SyncPhase};
use zmail_store::{
    LocalStore,
    merge_profile::{MergePhase, MergeProfile, profile_merge},
};
use zmail_test_support::{PROTON_STRETCH_MESSAGES, proton_mirror_relist, proton_mirror_stretch};

/// The demo Proton account's primary identity.
const OWN_ADDRESS: &str = "private1@proton.zmail.dev";

/// Messages merged at a time while growing the mirror between sizes.
const SEED_CHUNK: usize = 20 * PROTON_STRETCH_MESSAGES;

/// Stretches merged at each size; the table shows their median.
const SAMPLES: usize = 3;

/// The largest mirror also written as one full snapshot, which has to be
/// built in memory.
const FULL_SNAPSHOT_LIMIT: usize = 100_000;

/// The store calls of one backfill sync besides the write of its stretch
/// (`zmail-sync`'s `run_single_sync` and `sync_proton_mail`).
fn sync_bookkeeping(store: &LocalStore, account_id: i64) {
    let checked = "bookkeeping should succeed";
    store
        .begin_sync_attempt(account_id, SyncPhase::Connecting, "Connecting")
        .expect(checked);
    store.sync_target(account_id).expect(checked);
    store.list_pending_ops(account_id).expect(checked);
    store
        .begin_sync_attempt(account_id, SyncPhase::Discovering, "Discovering")
        .expect(checked);
    store.list_mailboxes(account_id).expect(checked);
    store
        .record_sync_checkpoint(account_id, "checkpoint")
        .expect(checked);
    for page in 0..8 {
        store
            .record_sync_progress(account_id, format!("Fetched page {page}"))
            .expect(checked);
    }
    store
        .begin_sync_attempt(account_id, SyncPhase::Syncing, "Writing")
        .expect(checked);
    store
        .record_sync_checkpoint(account_id, "checkpoint")
        .expect(checked);
    store
        .record_sync_success(account_id, "Fetched", "checkpoint")
        .expect(checked);
    store.account_disk_budget_bytes(account_id).expect(checked);
}

struct Row {
    label: String,
    wall: Duration,
    profile: MergeProfile,
}

fn median(mut samples: Vec<(Duration, MergeProfile)>) -> (Duration, MergeProfile) {
    samples.sort_by_key(|(wall, _)| *wall);
    samples[samples.len() / 2]
}

fn millis(duration: Duration) -> String {
    let ms = duration.as_secs_f64() * 1_000.0;
    if ms >= 100.0 {
        format!("{ms:.0}")
    } else if ms >= 1.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.2}")
    }
}

fn print_table(title: &str, rows: &[Row]) {
    println!("\n### {title}\n");
    print!("| Mirror |");
    for phase in MergePhase::ALL {
        print!(" {} |", phase.name());
    }
    println!(" other | total (ms) |");
    println!("|---|{}", "---|".repeat(MergePhase::ALL.len() + 2));
    for row in rows {
        print!("| {} |", row.label);
        for phase in MergePhase::ALL {
            print!(" {} |", millis(row.profile.phase(phase)));
        }
        println!(
            " {} | {} |",
            millis(row.wall.saturating_sub(row.profile.total())),
            millis(row.wall)
        );
    }
}

fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn main() {
    // `cargo bench` passes `--bench`; `cargo test --benches` runs this
    // without it, and then there is nothing to check.
    if !std::env::args().any(|arg| arg == "--bench") {
        return;
    }
    let sizes: Vec<usize> = std::env::var("ZMAIL_BACKFILL_SIZES")
        .unwrap_or_else(|_| "10000,100000".to_owned())
        .split(',')
        .filter_map(|size| size.trim().parse().ok())
        .collect();

    let store = LocalStore::open_ephemeral().expect("store should open");
    let account_id = store
        .create_demo_account(ProtocolKind::Proton)
        .expect("account should be created");

    let mut merged = Vec::new();
    let mut relisted_same = Vec::new();
    let mut relisted_new = Vec::new();
    let mut replaced = Vec::new();
    let mut bookkeeping = Vec::new();
    let mut held = 0usize;
    for size in sizes {
        let seeding = Instant::now();
        while held < size {
            let end = size.min(held + SEED_CHUNK);
            store
                .merge_account_snapshot(account_id, &proton_mirror_stretch(OWN_ADDRESS, held..end))
                .expect("the mirror should grow");
            held = end;
        }
        let stats = store.stats().expect("stats");
        eprintln!(
            "mirror of {} messages ({} message rows, {} threads) ready after {:.1} s",
            thousands(held),
            thousands(stats.message_count),
            thousands(stats.thread_count),
            seeding.elapsed().as_secs_f64()
        );
        let label = thousands(size);

        let mut samples = Vec::new();
        for _ in 0..SAMPLES {
            let stretch = proton_mirror_stretch(OWN_ADDRESS, held..held + PROTON_STRETCH_MESSAGES);
            let started = Instant::now();
            let (result, profile) =
                profile_merge(|| store.merge_account_snapshot(account_id, &stretch));
            samples.push((started.elapsed(), profile));
            result.expect("the stretch should merge");
            held += PROTON_STRETCH_MESSAGES;
        }
        let (wall, profile) = median(samples);
        merged.push(Row {
            label: label.clone(),
            wall,
            profile,
        });

        // A relist of mail the mirror holds unchanged, as after a refresh
        // request.
        let delta = proton_mirror_relist(OWN_ADDRESS, held - PROTON_STRETCH_MESSAGES..held);
        let started = Instant::now();
        let (result, profile) = profile_merge(|| store.apply_mail_delta(account_id, &delta));
        let wall = started.elapsed();
        assert!(result.expect("the delta should apply").is_empty());
        relisted_same.push(Row {
            label: label.clone(),
            wall,
            profile,
        });

        // A delta of mail the mirror does not hold: a relist that has
        // passed where the backfill stopped.
        let delta = proton_mirror_relist(OWN_ADDRESS, held..held + PROTON_STRETCH_MESSAGES);
        let started = Instant::now();
        let (result, profile) = profile_merge(|| store.apply_mail_delta(account_id, &delta));
        let wall = started.elapsed();
        assert_eq!(
            result.expect("the delta should apply").len(),
            PROTON_STRETCH_MESSAGES
        );
        held += PROTON_STRETCH_MESSAGES;
        relisted_new.push(Row {
            label: label.clone(),
            wall,
            profile,
        });

        let started = Instant::now();
        sync_bookkeeping(&store, account_id);
        bookkeeping.push((label.clone(), started.elapsed()));

        if held <= FULL_SNAPSHOT_LIMIT + 5 * PROTON_STRETCH_MESSAGES {
            let snapshot = proton_mirror_stretch(OWN_ADDRESS, 0..held + 1);
            let started = Instant::now();
            let (result, profile) =
                profile_merge(|| store.replace_account_snapshot(account_id, &snapshot));
            let wall = started.elapsed();
            result.expect("the snapshot should apply");
            held += 1;
            replaced.push(Row {
                label,
                wall,
                profile,
            });
        }
    }

    print_table(
        "`merge_account_snapshot`, one stretch of 1,024 new messages (median of 3)",
        &merged,
    );
    print_table(
        "`apply_mail_delta`, 1,024 messages the mirror already holds, unchanged",
        &relisted_same,
    );
    print_table(
        "`apply_mail_delta`, 1,024 messages new to the mirror",
        &relisted_new,
    );
    print_table(
        "`replace_account_snapshot`, the whole mirror and one new message",
        &replaced,
    );
    println!("\n### The other store calls of one backfill sync\n");
    println!("| Mirror | total (ms) |\n|---|---|");
    for (label, wall) in bookkeeping {
        println!("| {label} | {} |", millis(wall));
    }
}
