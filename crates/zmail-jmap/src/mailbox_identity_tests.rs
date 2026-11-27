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
//!
//! Each mailbox is stored under its path (`Clients/Acme/Invoices`). The
//! last tests cover what changes that path afterwards: a mirror stored
//! under the mailboxes' own names before, and a parent renamed on the
//! server.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use zmail_core::{MailDelta, MailboxSnapshot, PendingOp, PendingOpKind, ProtocolKind};
use zmail_store::LocalStore;

use crate::{JmapSync, apply_pending_ops, read_characterisation_tests::Replay};

fn same_names() -> Replay {
    Replay::start_with("shapes", "same-names", "same-names")
}

/// A store with one JMAP account and nothing synced.
pub(crate) fn store() -> (LocalStore, i64) {
    let store = LocalStore::open_ephemeral().expect("the store opens");
    let account_id = store
        .create_demo_account(ProtocolKind::Jmap)
        .expect("the account is created");
    (store, account_id)
}

/// Writes a sync into the store as the sync service does: the renamed
/// mailboxes first, then the snapshot.
pub(crate) fn write(store: &LocalStore, account_id: i64, sync: &JmapSync) {
    if !sync.renamed.is_empty() {
        store
            .apply_mail_delta(
                account_id,
                &MailDelta {
                    renamed: sync.renamed.clone(),
                    ..MailDelta::default()
                },
            )
            .expect("the renames are applied");
    }
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

/// Each stored mailbox's row id, by name.
fn row_ids(store: &LocalStore, account_id: i64) -> BTreeMap<String, i64> {
    store
        .list_mailboxes(account_id)
        .expect("the mailboxes list")
        .into_iter()
        .map(|mailbox| (mailbox.name, mailbox.id))
        .collect()
}

/// An `Email/changes` answer with nothing changed since `state`, so the
/// next sync takes the incremental path.
fn nothing_changed(replay: &Replay, state: &str) {
    replay.with_state(|server| {
        server.changes = Some(json!({
            "accountId": "a1", "oldState": state, "newState": state,
            "hasMoreChanges": false, "created": [], "updated": [], "destroyed": []
        }));
    });
}

/// The own name of a mailbox stored under `path`: what the mirror stored
/// it under before #512.
fn own_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// A mirror stored the way it was before #512: nested mailboxes under
/// their own names, with no delimiter, and a checkpoint that recorded
/// those names.
fn stored_before_paths(sync: &JmapSync) -> (Vec<MailboxSnapshot>, String) {
    let mailboxes = sync
        .snapshot
        .mailboxes
        .iter()
        .cloned()
        .map(|mut mailbox| {
            mailbox.name = own_name(&mailbox.name);
            mailbox.parent = mailbox.parent.as_deref().map(own_name);
            mailbox.delimiter = None;
            mailbox
        })
        .collect();
    let mut checkpoint: Value =
        serde_json::from_str(&sync.snapshot.checkpoint).expect("the checkpoint is JSON");
    for name in checkpoint["mailbox_ids"]
        .as_object_mut()
        .expect("the checkpoint has mailbox ids")
        .values_mut()
    {
        *name = Value::String(own_name(name.as_str().expect("a name")));
    }
    (mailboxes, checkpoint.to_string())
}

/// A mirror from before #512 is moved to paths by the next sync: the
/// nested mailbox keeps its row, its mail and the move queued into it
/// under its old name.
#[tokio::test]
async fn a_mirror_stored_by_own_names_moves_to_paths_in_place() {
    // `shapes` has `Zmail` under `Projects`, and no two names alike.
    let replay = Replay::start_with("shapes", "all", "sync");
    let current = replay.sync(None, &[]).await.expect("the first sync runs");
    let (before, checkpoint) = stored_before_paths(&current);
    let (store, account_id) = store();
    store
        .replace_account_snapshot(
            account_id,
            &zmail_core::AccountSyncSnapshot {
                mailboxes: before,
                ..current.snapshot.clone()
            },
        )
        .expect("the old mirror is written");
    let rows = row_ids(&store, account_id);
    let zmail = rows["Zmail"];
    let kept = store
        .account_mailbox_snapshots(account_id)
        .expect("mirror")
        .iter()
        .find(|mailbox| mailbox.name == "Zmail")
        .map(emails_in)
        .expect("Zmail is stored");
    // A conversation moved into it, queued under the old name.
    let inbox_thread = store
        .list_threads(rows["Inbox"], 0..1)
        .expect("the inbox lists")[0]
        .id;
    store
        .move_thread(inbox_thread, zmail)
        .expect("the conversation moves");
    let queued = store.list_pending_ops(account_id).expect("the queue lists");
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].target_mailbox.as_deref(), Some("Zmail"));

    nothing_changed(&replay, "em-41");
    let existing = store.account_mailbox_snapshots(account_id).expect("mirror");
    let sync = replay
        .sync(Some(checkpoint), &existing)
        .await
        .expect("the next sync runs");
    assert_eq!(
        sync.renamed,
        [zmail_core::MailboxRename {
            from: "Zmail".to_owned(),
            to: "Projects/Zmail".to_owned(),
            parent: Some("Projects".to_owned()),
        }]
    );
    write(&store, account_id, &sync);

    let rows_after = row_ids(&store, account_id);
    assert_eq!(
        rows_after.get("Projects/Zmail"),
        Some(&zmail),
        "{rows_after:?}"
    );
    assert!(!rows_after.contains_key("Zmail"), "{rows_after:?}");
    let mirror = store.account_mailbox_snapshots(account_id).expect("mirror");
    let moved = mirror
        .iter()
        .find(|mailbox| mailbox.name == "Projects/Zmail")
        .expect("Projects/Zmail is stored");
    assert_eq!(moved.parent.as_deref(), Some("Projects"));
    assert_eq!(moved.delimiter.as_deref(), Some("/"));
    let emails = emails_in(moved);
    assert!(
        kept.iter().all(|id| emails.contains(id)) && emails.len() > kept.len(),
        "its mail and the moved conversation stay: {kept:?} then {emails:?}"
    );
    let queued = store.list_pending_ops(account_id).expect("the queue lists");
    assert_eq!(queued[0].target_mailbox.as_deref(), Some("Projects/Zmail"));
    // Every top-level mailbox kept its row as well.
    for (name, id) in rows.iter().filter(|(name, _)| name.as_str() != "Zmail") {
        assert_eq!(rows_after.get(name), Some(id), "{name}");
    }
}

/// A move queued before #512 names a nested mailbox by its own name: it
/// still reaches that mailbox when the name is the only one of its kind,
/// and fails, rather than pick one, when several mailboxes share it.
#[tokio::test]
async fn a_move_queued_under_an_own_name_reaches_its_mailbox_when_unambiguous() {
    let replay = Replay::start_with("shapes", "all", "sync");
    let ops = [move_op(1, "e1", "Inbox", "Zmail")];
    let report = apply_pending_ops(&replay.target(None), "pw", &ops)
        .await
        .expect("the move runs");
    assert_eq!(report.applied, [1], "{report:?}");
    assert_eq!(
        updates_sent(&replay),
        [json!({"e1": {"mailboxIds/mb-inbox": null, "mailboxIds/mb-zmail": true}})]
    );

    let replay = same_names();
    let ops = [move_op(2, "e1", "Inbox", "Invoices")];
    let report = apply_pending_ops(&replay.target(None), "pw", &ops)
        .await
        .expect("the move runs");
    assert_eq!(
        report.failed,
        [(
            2,
            "the JMAP account has no mailbox named Invoices".to_owned()
        )]
    );
    assert!(updates_sent(&replay).is_empty());
}

/// A parent renamed on the server renames every mailbox under it, each
/// in place: same rows, same mail.
#[tokio::test]
async fn a_parent_renamed_on_the_server_renames_its_children_in_place() {
    let replay = same_names();
    let (store, account_id) = store();
    let first = replay.sync(None, &[]).await.expect("the first sync runs");
    write(&store, account_id, &first);
    let rows = row_ids(&store, account_id);
    let before = stored(&store, account_id);

    replay.with_state(|server| {
        server.mailboxes["state"] = json!("mb-13");
        for mailbox in server.mailboxes["list"].as_array_mut().expect("a list") {
            if mailbox["id"] == "mb-clients" {
                mailbox["name"] = json!("Customers");
            }
        }
    });
    nothing_changed(&replay, "em-7");
    let existing = store.account_mailbox_snapshots(account_id).expect("mirror");
    let second = replay
        .sync(Some(first.snapshot.checkpoint.clone()), &existing)
        .await
        .expect("the second sync runs");
    assert_eq!(
        second
            .renamed
            .iter()
            .map(|rename| (rename.from.as_str(), rename.to.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Clients", "Customers"),
            ("Clients/Invoices", "Customers/Invoices"),
            ("Clients/Acme", "Customers/Acme"),
            ("Clients/Acme/Invoices", "Customers/Acme/Invoices"),
        ]
    );
    write(&store, account_id, &second);

    let renamed = |name: &str| name.replacen("Clients", "Customers", 1);
    let rows_after = row_ids(&store, account_id);
    let expected_rows = rows
        .iter()
        .map(|(name, id)| (renamed(name), *id))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(rows_after, expected_rows);
    let expected = before
        .into_iter()
        .map(|(name, parent, emails)| (renamed(&name), parent.as_deref().map(renamed), emails))
        .collect::<Vec<_>>();
    assert_eq!(stored(&store, account_id), expected);
}
