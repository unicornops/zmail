//! When a full sync may return the stored mirror as it is (#513).
//!
//! When the incremental path cannot answer (the server cannot calculate
//! changes, or the checkpoint has no `Email` state), the full sync reads
//! the `Mailbox` state, the `Email` state and the `queryState` of its
//! `Email/query`, and returns the stored mirror untouched when nothing
//! has changed since the checkpoint.
//!
//! RFC 8620 section 5.5 ties the `queryState` to the results of the
//! query: the ids and their order. A change to an email's keywords
//! (read, flagged) on another device leaves those as they were, so the
//! server may keep the `queryState`. Only the `Email` state (section
//! 5.1: it changes whenever any email changes) says whether the emails
//! did.
//!
//! The server replays the `shapes` fixtures, into a real store.

use serde_json::json;
use zmail_core::{MailboxSnapshot, MessageSnapshot};
use zmail_store::LocalStore;

use crate::{
    mailbox_identity_tests::{store, write},
    read_characterisation_tests::Replay,
};

/// The stored message `email_id`, from the first mailbox that holds it.
fn stored_message(mirror: &[MailboxSnapshot], email_id: &str) -> MessageSnapshot {
    mirror
        .iter()
        .flat_map(|mailbox| &mailbox.threads)
        .flat_map(|thread| &thread.messages)
        .find(|message| message.remote_id == email_id)
        .unwrap_or_else(|| panic!("{email_id} is stored"))
        .clone()
}

fn mirror(store: &LocalStore, account_id: i64) -> Vec<MailboxSnapshot> {
    store
        .account_mailbox_snapshots(account_id)
        .expect("the mirror reads")
}

/// A recorded account synced once into a new store, with the checkpoint
/// the sync left.
async fn synced_once(replay: &Replay) -> (LocalStore, i64, String) {
    let (store, account_id) = store();
    let first = replay.sync(None, &[]).await.expect("the first sync runs");
    write(&store, account_id, &first);
    (store, account_id, first.snapshot.checkpoint)
}

/// The `queryState` a checkpoint recorded.
fn query_state(checkpoint: &str) -> String {
    let checkpoint: serde_json::Value =
        serde_json::from_str(checkpoint).expect("the checkpoint is JSON");
    checkpoint["email_query_state"]
        .as_str()
        .expect("the full sync recorded a queryState")
        .to_owned()
}

/// Emails read and flagged on another device: the `Email` state moves
/// on, the `queryState` stays (the query's results are the same ids),
/// and the server cannot say what changed. The next sync stores the new
/// keywords.
#[tokio::test]
async fn a_keyword_change_under_the_same_query_state_reaches_the_store() {
    let replay = Replay::start_with("shapes", "all", "sync");
    let (store, account_id, checkpoint) = synced_once(&replay).await;
    let before = mirror(&store, account_id);
    assert!(!stored_message(&before, "e2").is_read);
    assert!(!stored_message(&before, "e1").is_flagged);

    replay.with_state(|server| {
        server.full["state"] = json!("em-42");
        for email in server.full["list"].as_array_mut().expect("a list") {
            match email["id"].as_str() {
                Some("e2") => email["keywords"] = json!({"$seen": true}),
                Some("e1") => email["keywords"] = json!({"$seen": true, "$flagged": true}),
                _ => {}
            }
        }
        // `Email/changes` is answered `cannotCalculateChanges`.
        server.changes = None;
    });

    let sync = replay
        .sync(Some(checkpoint.clone()), &before)
        .await
        .expect("the second sync runs");
    assert_eq!(
        query_state(&sync.snapshot.checkpoint),
        query_state(&checkpoint),
        "the server kept the queryState"
    );
    assert_ne!(
        sync.snapshot.detail, "JMAP mailbox state unchanged for Recorded",
        "the Email state changed, so the mirror is not returned as it was"
    );
    write(&store, account_id, &sync);

    let after = mirror(&store, account_id);
    assert!(
        stored_message(&after, "e2").is_read,
        "e2, read on another device, is stored read"
    );
    assert!(
        stored_message(&after, "e1").is_flagged,
        "e1, flagged on another device, is stored flagged"
    );
    let checkpoint: serde_json::Value =
        serde_json::from_str(&sync.snapshot.checkpoint).expect("the checkpoint is JSON");
    assert_eq!(checkpoint["email_state"], "em-42");
}

/// With the `Mailbox` state, the `Email` state and the `queryState` all
/// as the checkpoint recorded them, the stored mirror is returned as it
/// is and no email is fetched.
#[tokio::test]
async fn nothing_changed_returns_the_stored_mirror_without_fetching() {
    let replay = Replay::start_with("shapes", "all", "sync");
    let (store, account_id, checkpoint) = synced_once(&replay).await;
    let before = mirror(&store, account_id);
    replay.with_state(|server| server.calls.clear());

    let sync = replay
        .sync(Some(checkpoint), &before)
        .await
        .expect("the second sync runs");
    assert_eq!(
        sync.snapshot.detail,
        "JMAP mailbox state unchanged for Recorded"
    );
    assert_eq!(sync.snapshot.mailboxes, before);
    let fetched = replay.with_state(|server| {
        server
            .calls
            .iter()
            .filter(|call| {
                call[0] == "Email/get"
                    && call[1]["ids"].as_array().is_some_and(|ids| !ids.is_empty())
            })
            .count()
    });
    assert_eq!(fetched, 0, "no email is fetched");
}

/// The mailbox side of the same shortcut: a mailbox renamed on the
/// server moves the `Mailbox` state on (RFC 8620 section 5.1), with the
/// `Email` state and the `queryState` as they were. The mirror is not
/// returned as it was.
#[tokio::test]
async fn a_mailbox_change_under_the_same_email_states_is_not_skipped() {
    let replay = Replay::start_with("shapes", "all", "sync");
    let (store, account_id, checkpoint) = synced_once(&replay).await;
    let before = mirror(&store, account_id);

    replay.with_state(|server| {
        server.mailboxes["state"] = json!("mb-8");
        for mailbox in server.mailboxes["list"].as_array_mut().expect("a list") {
            if mailbox["id"] == "mb-zmail" {
                mailbox["name"] = json!("Zmail 2");
            }
        }
    });

    let sync = replay
        .sync(Some(checkpoint), &before)
        .await
        .expect("the second sync runs");
    assert_ne!(
        sync.snapshot.detail,
        "JMAP mailbox state unchanged for Recorded"
    );
    assert!(
        sync.snapshot
            .mailboxes
            .iter()
            .any(|mailbox| mailbox.name == "Projects/Zmail 2"),
        "{:#?}",
        sync.snapshot
            .mailboxes
            .iter()
            .map(|mailbox| &mailbox.name)
            .collect::<Vec<_>>()
    );
}
