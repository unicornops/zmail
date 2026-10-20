//! Store performance benchmarks for snapshot merges, thread paging,
//! search, address autocomplete, and the write-back queue; and the store
//! side of the UI budgets (#122) over a 100k-thread mailbox.
//!
//! Run with `cargo bench -p zmail-store`; the harness is deliberately not
//! part of CI so the quality job stays fast. `docs/performance.md` has the
//! budgets.

use std::sync::OnceLock;

use divan::Bencher;
use zmail_core::{
    AccountSyncSnapshot, MailboxSelection, MailboxSnapshot, MessageSnapshot, PendingOpKind,
    ProtocolKind, ThreadSnapshot, VirtualMailbox,
};
use zmail_store::LocalStore;
use zmail_test_support::{
    STRESS_ATTACHMENTS_SUBJECT, STRESS_HTML_SUBJECT, STRESS_LONG_SUBJECT, reader_stress_threads,
};

/// Builds a snapshot with one mailbox holding `thread_count` one-message
/// conversations with distinct remote ids, from 500 different senders.
fn synthetic_snapshot(thread_count: usize) -> AccountSyncSnapshot {
    let threads = (0..thread_count)
        .map(|index| ThreadSnapshot {
            remote_thread_id: format!("bench-thread-{index}"),
            subject: format!("Benchmark subject {index}"),
            participants: "Bench Sender -> recipient@example.com".to_owned(),
            preview: format!("Benchmark body {index}"),
            is_flagged: false,
            has_attachments: false,
            unread_count: 1,
            latest_message_at: format!("2026-01-01T00:{:02}:{:02}Z", index / 60 % 60, index % 60),
            messages: vec![MessageSnapshot {
                remote_id: format!("bench-message-{index}"),
                identity_email: "bench@example.com".to_owned(),
                sender: format!("Bench Sender {0} <sender{0}@example.com>", index % 500),
                reply_to: None,
                recipients: "recipient@example.com".to_owned(),
                subject: format!("Benchmark subject {index}"),
                body_preview: format!("Benchmark body {index}"),
                body_text: Some(format!("Benchmark body {index}")),
                sanitized_html: None,
                has_remote_content: false,
                is_read: false,
                is_flagged: false,
                received_at: format!("2026-01-01T00:{:02}:{:02}Z", index / 60 % 60, index % 60),
                attachments: Vec::new(),
            }],
        })
        .collect();

    AccountSyncSnapshot {
        checkpoint: "bench".to_owned(),
        detail: "benchmark snapshot".to_owned(),
        identities: None,
        mailboxes: vec![MailboxSnapshot {
            name: "Inbox".to_owned(),
            role: Some("inbox".to_owned()),
            threads,
            ..Default::default()
        }],
    }
}

/// Opens an ephemeral store seeded with one account and `thread_count`
/// merged messages.
fn seeded_store(thread_count: usize) -> (LocalStore, i64) {
    let store = LocalStore::open_ephemeral().expect("store should open");
    let account_id = store
        .create_demo_account(ProtocolKind::Imap)
        .expect("account should be created");
    store
        .replace_account_snapshot(account_id, &synthetic_snapshot(thread_count))
        .expect("snapshot should apply");
    (store, account_id)
}

#[divan::bench(args = [100, 1_000, 5_000])]
fn snapshot_first_apply(bencher: Bencher, thread_count: usize) {
    let snapshot = synthetic_snapshot(thread_count);
    bencher.bench(|| {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");
    });
}

#[divan::bench(args = [100, 1_000, 5_000])]
fn snapshot_no_change_remerge(bencher: Bencher, thread_count: usize) {
    let (store, account_id) = seeded_store(thread_count);
    let snapshot = synthetic_snapshot(thread_count);
    bencher.bench(|| {
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should re-merge");
    });
}

/// A sync that brings one new message, which also rebuilds the account's
/// address history from the mirror.
#[divan::bench(args = [1_000, 5_000, 20_000])]
fn snapshot_one_new_message(bencher: Bencher, thread_count: usize) {
    let (store, account_id) = seeded_store(thread_count);
    let mut with_new = synthetic_snapshot(thread_count + 1);
    let without_new = synthetic_snapshot(thread_count);
    let mut grow = true;
    bencher.bench_local(|| {
        // Alternate, so every iteration changes the mirror.
        let snapshot = if grow {
            &mut with_new
        } else {
            &mut without_new.clone()
        };
        store
            .replace_account_snapshot(account_id, snapshot)
            .expect("snapshot should merge");
        grow = !grow;
    });
}

#[divan::bench(args = [1_000, 5_000, 20_000])]
fn address_autocomplete(bencher: Bencher, thread_count: usize) {
    let (store, _) = seeded_store(thread_count);
    bencher.bench(|| {
        store
            .address_suggestions("bench sender 4", 8)
            .expect("suggestions should load");
    });
}

#[divan::bench(args = [100, 1_000, 5_000])]
fn list_threads_paging(bencher: Bencher, thread_count: usize) {
    let (store, account_id) = seeded_store(thread_count);
    let mailbox_id = store
        .list_mailboxes(account_id)
        .expect("mailboxes should load")[0]
        .id;
    bencher.bench(|| {
        store
            .list_threads(mailbox_id, 0..50)
            .expect("threads should load");
    });
}

#[divan::bench(args = [100, 1_000, 5_000])]
fn full_text_search(bencher: Bencher, thread_count: usize) {
    let (store, account_id) = seeded_store(thread_count);
    bencher.bench(|| {
        store
            .search_threads(account_id, "benchmark", 50)
            .expect("search should run");
    });
}

#[divan::bench(args = [100, 1_000])]
fn pending_op_queue(bencher: Bencher, thread_count: usize) {
    let (store, account_id) = seeded_store(thread_count);
    // Queue one write-back operation per message.
    let mailbox_id = store
        .list_mailboxes(account_id)
        .expect("mailboxes should load")[0]
        .id;
    let thread = store
        .list_threads(mailbox_id, 0..1)
        .expect("threads should load")[0]
        .clone();
    store
        .set_thread_read(thread.id, true)
        .expect("ops should queue");
    let _ = PendingOpKind::MarkRead;
    bencher.bench(|| {
        let _ = store.pending_op_count(account_id);
        let _ = store.dead_op_count(account_id);
        for op in store.list_pending_ops(account_id).expect("ops should load") {
            let _ = op.id;
        }
    });
}

/// Threads in the large mailbox: the UI budgets' 100k.
const LARGE_MAILBOX: usize = 100_000;

/// A store whose Inbox holds [`LARGE_MAILBOX`] one-message conversations
/// plus the reader-stress conversations (newest), seeded once and shared
/// by the large-mailbox benchmarks.
struct LargeStore {
    store: LocalStore,
    inbox: i64,
}

fn large_store() -> &'static LargeStore {
    static STORE: OnceLock<LargeStore> = OnceLock::new();
    STORE.get_or_init(|| {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let mut snapshot = synthetic_snapshot(LARGE_MAILBOX);
        snapshot.mailboxes[0]
            .threads
            .extend(reader_stress_threads());
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");
        let inbox = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .id;
        LargeStore { store, inbox }
    })
}

/// The page of rows the thread list reads when it scrolls to `depth`: the
/// visible rows and 50 either side.
#[divan::bench(args = [0, 50_000, 99_850])]
fn large_mailbox_page(bencher: Bencher, depth: usize) {
    let large = large_store();
    bencher.bench(|| {
        large
            .store
            .list_threads(large.inbox, depth..depth + 112)
            .expect("threads should load")
    });
}

/// The counts every store refresh reads after a change (archive, flag,
/// mark read): the store totals, every mailbox with its counts, and the
/// Flagged virtual mailbox's unread count.
#[divan::bench]
fn large_mailbox_refresh_counts(bencher: Bencher) {
    let large = large_store();
    bencher.bench(|| {
        let _ = large.store.stats().expect("stats should load");
        let _ = large
            .store
            .list_all_mailboxes()
            .expect("mailboxes should load");
        large
            .store
            .selection_counts(&MailboxSelection::Virtual(VirtualMailbox::Flagged))
            .expect("counts should load")
    });
}

/// Loading a conversation the reader opens.
#[divan::bench(args = [STRESS_LONG_SUBJECT, STRESS_HTML_SUBJECT, STRESS_ATTACHMENTS_SUBJECT])]
fn thread_view_stress(bencher: Bencher, subject: &str) {
    let large = large_store();
    let thread_id = large
        .store
        .list_threads(large.inbox, 0..3)
        .expect("threads should load")
        .into_iter()
        .find(|thread| thread.subject == subject)
        .expect("the stress conversations list first")
        .id;
    bencher.bench(|| {
        large
            .store
            .thread_view(thread_id)
            .expect("the thread should load")
    });
}

fn main() {
    divan::main();
}
