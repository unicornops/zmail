//! Mailboxes with the same name under different parents (#512).
//!
//! RFC 8621 section 2 asks only that a mailbox's name be unique among its
//! siblings, so an account can hold `Clients/Invoices` and
//! `Suppliers/Invoices`, and three levels down `Clients/Acme/Invoices`
//! and `Suppliers/Acme/Invoices`, where the parent `Acme` is no help
//! either. The store matches mailboxes by name, so each of them must
//! reach it under a name of its own, its mail must land in it, and a
//! move into it must name its id to the server.
//!
//! The server replays `mailbox-get.same-names.json` and
//! `email-get.same-names.json` from the `shapes` fixtures: one email in
//! each of the four `Invoices` mailboxes, and one in the inbox.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use zmail_core::{MailboxSnapshot, PendingOp, PendingOpKind, ProtocolKind};
use zmail_store::LocalStore;

use crate::{JmapSync, apply_pending_ops, read_characterisation_tests::Replay};

fn same_names() -> Replay {
    Replay::start_with("shapes", "same-names", "same-names")
}

/// A store with one JMAP account and nothing synced.
fn store() -> (LocalStore, i64) {
    let store = LocalStore::open_ephemeral().expect("the store opens");
    let account_id = store
        .create_demo_account(ProtocolKind::Jmap)
        .expect("the account is created");
    (store, account_id)
}

/// Writes a sync into the store as the sync service does.
fn write(store: &LocalStore, account_id: i64, sync: &JmapSync) {
    store
        .replace_account_snapshot(account_id, &sync.snapshot)
        .expect("the snapshot is written");
}

/// Two full syncs of the replayed account into a new store, the second
/// merging into what the first stored.
async fn synced_twice(replay: &Replay) -> (LocalStore, i64) {
    let (store, account_id) = store();
    let first = replay.sync(None, &[]).await.expect("the first sync runs");
    write(&store, account_id, &first);
    let existing = store
        .account_mailbox_snapshots(account_id)
        .expect("the mirror reads");
    let second = replay
        .sync(None, &existing)
        .await
        .expect("the second sync runs");
    write(&store, account_id, &second);
    (store, account_id)
}

/// Each stored mailbox: its name, parent and the emails in it.
fn stored(store: &LocalStore, account_id: i64) -> Vec<(String, Option<String>, Vec<String>)> {
    store
        .account_mailbox_snapshots(account_id)
        .expect("the mirror reads")
        .into_iter()
        .map(|mailbox| {
            (
                mailbox.name.clone(),
                mailbox.parent.clone(),
                emails_in(&mailbox),
            )
        })
        .collect()
}

fn emails_in(mailbox: &MailboxSnapshot) -> Vec<String> {
    let mut ids = mailbox
        .threads
        .iter()
        .flat_map(|thread| &thread.messages)
        .map(|message| message.remote_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

/// The stored mailboxes that hold the email `email_id`.
fn holding<'a>(
    stored: &'a [(String, Option<String>, Vec<String>)],
    email_id: &str,
) -> Vec<&'a (String, Option<String>, Vec<String>)> {
    stored
        .iter()
        .filter(|(_, _, emails)| emails.iter().any(|id| id == email_id))
        .collect()
}

/// Every stored mailbox name is distinct: the store tells mailboxes
/// apart by name only.
fn assert_names_distinct(stored: &[(String, Option<String>, Vec<String>)]) {
    let mut counts = BTreeMap::<&str, usize>::new();
    for (name, _, _) in stored {
        *counts.entry(name.as_str()).or_default() += 1;
    }
    let repeated = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .collect::<Vec<_>>();
    assert!(
        repeated.is_empty(),
        "mailboxes stored under one name: {repeated:?}\nstored: {stored:#?}"
    );
}

/// `Clients/Invoices` and `Suppliers/Invoices` are two mailboxes in the
/// store, each with its own mail.
#[tokio::test]
async fn same_named_siblings_of_different_parents_are_stored_apart() {
    let replay = same_names();
    let (store, account_id) = synced_twice(&replay).await;
    let stored = stored(&store, account_id);

    assert_names_distinct(&stored);
    assert_eq!(
        stored.len(),
        9,
        "every server mailbox is stored: {stored:#?}"
    );
    for (email, name, parent) in [
        ("e2", "Clients/Invoices", "Clients"),
        ("e3", "Suppliers/Invoices", "Suppliers"),
    ] {
        assert_eq!(
            holding(&stored, email),
            [&(
                name.to_owned(),
                Some(parent.to_owned()),
                vec![email.to_owned()]
            )],
            "{email} is alone in {name}\nstored: {stored:#?}"
        );
    }
}

/// Three levels down the parent is ambiguous by name as well:
/// `Clients/Acme/Invoices` and `Suppliers/Acme/Invoices` are stored apart,
/// each under its own `Acme`.
#[tokio::test]
async fn same_named_mailboxes_under_same_named_parents_are_stored_apart() {
    let replay = same_names();
    let (store, account_id) = synced_twice(&replay).await;
    let stored = stored(&store, account_id);

    assert_names_distinct(&stored);
    for (email, name, parent) in [
        ("e4", "Clients/Acme/Invoices", "Clients/Acme"),
        ("e5", "Suppliers/Acme/Invoices", "Suppliers/Acme"),
    ] {
        assert_eq!(
            holding(&stored, email),
            [&(
                name.to_owned(),
                Some(parent.to_owned()),
                vec![email.to_owned()]
            )],
            "{email} is alone in {name}\nstored: {stored:#?}"
        );
    }
    for (name, parent) in [("Clients/Acme", "Clients"), ("Suppliers/Acme", "Suppliers")] {
        assert!(
            stored.iter().any(|(stored_name, stored_parent, emails)| {
                stored_name == name && stored_parent.as_deref() == Some(parent) && emails.is_empty()
            }),
            "{name} is stored under {parent}\nstored: {stored:#?}"
        );
    }
}

/// The name the sync gave the mailbox that holds `email_id`, as a queued
/// move names it.
fn mailbox_of(sync: &JmapSync, email_id: &str) -> String {
    let names = sync
        .snapshot
        .mailboxes
        .iter()
        .filter(|mailbox| emails_in(mailbox).iter().any(|id| id == email_id))
        .map(|mailbox| mailbox.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 1, "{email_id} is in one mailbox: {names:?}");
    names[0].clone()
}

fn move_op(id: i64, email_id: &str, source: &str, target: &str) -> PendingOp {
    PendingOp {
        id,
        account_id: 1,
        kind: PendingOpKind::Move,
        remote_id: email_id.to_owned(),
        source_mailbox: source.to_owned(),
        target_mailbox: Some(target.to_owned()),
        attempts: 0,
        last_error: None,
    }
}

/// The `Email/set` updates the server was sent, in order.
fn updates_sent(replay: &Replay) -> Vec<Value> {
    replay.with_state(|state| {
        state
            .calls
            .iter()
            .filter(|call| call[0] == "Email/set")
            .map(|call| call[1]["update"].clone())
            .collect()
    })
}

/// A move between two same-named mailboxes names both by their ids: the
/// email leaves the one it is in and joins the one the user chose.
#[tokio::test]
async fn a_move_names_the_mailboxes_the_user_chose() {
    let replay = same_names();
    let sync = replay.sync(None, &[]).await.expect("the sync runs");
    let target = replay.target(None);

    let ops = [
        // Clients/Invoices to Suppliers/Invoices.
        move_op(1, "e2", &mailbox_of(&sync, "e2"), &mailbox_of(&sync, "e3")),
        // Suppliers/Acme/Invoices to Clients/Acme/Invoices.
        move_op(2, "e5", &mailbox_of(&sync, "e5"), &mailbox_of(&sync, "e4")),
    ];
    let report = apply_pending_ops(&target, "pw", &ops)
        .await
        .expect("the moves run");
    assert_eq!(report.applied, [1, 2], "{report:?}");
    assert_eq!(
        updates_sent(&replay),
        [
            json!({"e2": {
                "mailboxIds/mb-c-invoices": null,
                "mailboxIds/mb-s-invoices": true
            }}),
            json!({"e5": {
                "mailboxIds/mb-s-acme-invoices": null,
                "mailboxIds/mb-c-acme-invoices": true
            }}),
        ]
    );
}
