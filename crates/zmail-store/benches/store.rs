//! Store performance benchmarks for snapshot merges, thread paging,
//! search, and the write-back queue.
//!
//! Run with `cargo bench -p zmail-store`; the harness is deliberately not
//! part of CI so the quality job stays fast.

use divan::Bencher;
use zmail_core::{
    AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, PendingOpKind, ProtocolKind,
    ThreadSnapshot,
};
use zmail_store::LocalStore;

/// Builds a snapshot with one mailbox holding `thread_count` one-message
/// conversations with distinct remote ids.
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
                sender: "Bench Sender".to_owned(),
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

fn main() {
    divan::main();
}
