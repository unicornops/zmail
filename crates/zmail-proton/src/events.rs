// Applying events follows proton-bridge
// `internal/services/imapservice/service_message_events.go` (create,
// update, update-flags and delete) and
// `internal/services/userevents/subscription.go` (a refresh resyncs), and
// `EventPeriod`/`EventJitter` from `internal/user/user.go`, at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust against Zmail's
// mailbox snapshots. Label, address and user changes resync the account
// instead of being applied one by one. Modified on 2026-10-23: message
// events become a delta of just the changed messages, written without
// reading or rewriting the rest of the mirror.

//! Incremental sync from the account's event stream.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::Duration,
};

use anyhow::Result;
use rand::Rng;
use zmail_core::MailDelta;
use zmail_proton_api::{Client, Event, EventAction, Label, MessageEvent, MessageMetadata};

use crate::{
    ProtonCheckpoint, address_identities,
    sync::{self, PlacedMessage},
};

/// How often Bridge polls the event stream (`EventPeriod`).
pub const EVENT_PERIOD: Duration = Duration::from_secs(20);
/// The random extra wait on each poll (`EventJitter`).
pub const EVENT_JITTER: Duration = Duration::from_secs(20);

/// The wait before the next event poll: the period plus a random jitter,
/// like Bridge's ticker.
pub fn event_poll_delay() -> Duration {
    let jitter_ms = u64::try_from(EVENT_JITTER.as_millis()).unwrap_or(u64::MAX);
    EVENT_PERIOD + Duration::from_millis(rand::thread_rng().gen_range(0..=jitter_ms))
}

/// Whether anything Zmail mirrors changed after the stored `checkpoint`
/// (one event poll; the sync then fetches and applies the events). A
/// checkpoint that cannot be read needs a sync to replace it.
pub async fn has_new_events(session: &crate::ProtonSession, checkpoint: &str) -> Result<bool> {
    let Some(checkpoint) = ProtonCheckpoint::parse(checkpoint) else {
        return Ok(true);
    };
    let (events, more) = session.client().get_events(&checkpoint.event_id).await?;
    Ok(more || events.iter().any(Event::has_changes))
}

/// The events after a checkpoint, read.
#[derive(Debug)]
pub(crate) struct AppliedEvents {
    /// The last event read: the next checkpoint.
    pub(crate) event_id: String,
    /// The changed messages, or `None` when no event changed one.
    pub(crate) delta: Option<MailDelta>,
    /// Messages the events created, newest state last: the candidates for
    /// new-mail notifications.
    pub(crate) created: Vec<String>,
    /// The number of message changes read.
    pub(crate) changes: usize,
}

/// Reads the events after `event_id` and turns their message changes into
/// a delta of just those messages (Bridge applies each event to its IMAP
/// state the same way, message by message). Returns `None` when the
/// account needs a first sync instead.
///
/// While older mail is still being fetched the mirror holds only part of
/// the history; an event for a message not listed yet places it now, and
/// the backfill listing it later finds it already there.
pub(crate) async fn apply_events(client: &Client, event_id: &str) -> Result<Option<AppliedEvents>> {
    let mut events = Vec::new();
    let mut from = event_id.to_owned();
    loop {
        let (batch, more) = client.get_events(&from).await?;
        if let Some(last) = batch.last() {
            from = last.event_id.clone();
        }
        events.extend(batch);
        if !more {
            break;
        }
    }
    if events.iter().any(needs_resync) {
        return Ok(None);
    }

    let changes: Vec<_> = events.iter().flat_map(Event::message_events).collect();
    if changes.is_empty() {
        return Ok(Some(AppliedEvents {
            event_id: from,
            delta: None,
            created: Vec::new(),
            changes: 0,
        }));
    }

    let labels = client.get_labels(&sync::LABEL_TYPES).await?;
    let addresses = client.get_addresses().await?;
    let (identity_for_address, fallback_identity) = address_identities(&addresses, "");
    let (delta, created) =
        message_delta(&labels, &changes, &identity_for_address, &fallback_identity);
    Ok(Some(AppliedEvents {
        event_id: from,
        delta: Some(delta),
        created,
        changes: changes.len(),
    }))
}

/// The delta for a batch of message events: each changed message placed by
/// its latest labels (none, once deleted), and the messages created.
fn message_delta(
    labels: &[Label],
    changes: &[&MessageEvent],
    identity_for_address: &HashMap<String, String>,
    fallback_identity: &str,
) -> (MailDelta, Vec<String>) {
    // The last change to a message wins.
    let mut latest: BTreeMap<&str, Option<&MessageMetadata>> = BTreeMap::new();
    let mut created: BTreeSet<&str> = BTreeSet::new();
    for change in changes {
        match (change.action, &change.message) {
            (EventAction::Delete, _) => {
                latest.insert(&change.id, None);
                created.remove(change.id.as_str());
            }
            (EventAction::Create | EventAction::Update | EventAction::UpdateFlags, Some(meta)) => {
                latest.insert(&meta.id, Some(meta));
                if change.action == EventAction::Create {
                    created.insert(&meta.id);
                }
            }
            // An update without metadata, or an unknown action: nothing to
            // apply here.
            _ => {}
        }
    }
    let placed = latest.values().flatten().map(|meta| {
        let identity = identity_for_address
            .get(&meta.address_id)
            .map(String::as_str)
            .unwrap_or(fallback_identity);
        PlacedMessage::from_metadata(meta, identity)
    });
    let delta = MailDelta {
        mailboxes: sync::place_messages(labels, placed),
        message_ids: latest.keys().map(|id| (*id).to_owned()).collect(),
    };
    (delta, created.into_iter().map(str::to_owned).collect())
}

/// Whether an event needs a full sync rather than message updates.
fn needs_resync(event: &Event) -> bool {
    event.needs_mail_refresh()
        || event.user.is_some()
        || !event.label_events().is_empty()
        || !event.address_events().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(json: serde_json::Value) -> Event {
        serde_json::from_value(json).expect("event")
    }

    #[test]
    fn turns_message_events_into_a_delta_of_those_messages() {
        use crate::mailbox::tests::label;

        let labels = [
            label("0", "Inbox", "Inbox", 4),
            label("5", "All Mail", "All Mail", 4),
            label("10", "Starred", "Starred", 4),
            label("l", "Family", "Family", 1),
        ];
        let meta = |id: &str, conversation: &str, labels: &[&str], unread: bool| {
            serde_json::json!({
                "ID": id, "AddressID": "a1", "ConversationID": conversation,
                "LabelIDs": labels, "Subject": format!("S {id}"),
                "Sender": {"Name": "", "Address": "x@example.com"}, "Flags": 1,
                "Time": 1_700_000_000, "Unread": i64::from(unread),
            })
        };
        let identities = HashMap::from([("a1".to_owned(), "me@proton.local".to_owned())]);
        let events: Vec<MessageEvent> = serde_json::from_value(serde_json::json!([
            // m1 read and starred.
            {"ID": "m1", "Action": 3, "Message": meta("m1", "c1", &["0", "5", "10"], false)},
            // m2 loses its label.
            {"ID": "m2", "Action": 2, "Message": meta("m2", "c2", &["0", "5"], false)},
            // m3 deleted; m4 arrives in conversation c1.
            {"ID": "m3", "Action": 0},
            {"ID": "m4", "Action": 1, "Message": meta("m4", "c1", &["0", "5"], true)},
            // m5 arrives, then is moved out of the inbox, then deleted.
            {"ID": "m5", "Action": 1, "Message": meta("m5", "c5", &["0", "5"], true)},
            {"ID": "m5", "Action": 2, "Message": meta("m5", "c5", &["5"], true)},
            {"ID": "m5", "Action": 0},
            // An update without metadata changes nothing.
            {"ID": "m6", "Action": 2},
        ]))
        .expect("events");
        let changes: Vec<&MessageEvent> = events.iter().collect();
        let (delta, created) = message_delta(&labels, &changes, &identities, "me@proton.local");

        assert_eq!(delta.message_ids, ["m1", "m2", "m3", "m4", "m5"]);
        assert_eq!(created, ["m4"], "m5 was deleted again");
        let places = |name: &str| {
            let mut places: Vec<(String, String)> = delta
                .mailboxes
                .iter()
                .find(|mailbox| mailbox.name == name)
                .map(|mailbox| {
                    mailbox
                        .threads
                        .iter()
                        .flat_map(|thread| {
                            thread.messages.iter().map(|message| {
                                (thread.remote_thread_id.clone(), message.remote_id.clone())
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            places.sort();
            places
        };
        let pair = |thread: &str, message: &str| (thread.to_owned(), message.to_owned());
        assert_eq!(
            places("INBOX"),
            [pair("c1", "m1"), pair("c1", "m4"), pair("c2", "m2")]
        );
        assert_eq!(places("All Mail"), places("INBOX"));
        assert!(
            places("Labels/Family").is_empty(),
            "m2 is placed nowhere else, so it leaves Family"
        );
        let inbox = &delta.mailboxes[0];
        let message = |id: &str| {
            inbox
                .threads
                .iter()
                .flat_map(|thread| thread.messages.iter())
                .find(|message| message.remote_id == id)
                .expect("message")
        };
        assert!(message("m1").is_read && message("m1").is_flagged);
        assert!(!message("m4").is_read);
        assert_eq!(message("m4").identity_email, "me@proton.local");
    }

    #[test]
    fn polls_like_bridge() {
        for _ in 0..100 {
            let delay = event_poll_delay();
            assert!(delay >= Duration::from_secs(20) && delay <= Duration::from_secs(40));
        }
    }

    #[test]
    fn resyncs_on_refresh_and_structural_changes() {
        assert!(needs_resync(&event(
            serde_json::json!({"EventID": "e", "Refresh": 1})
        )));
        assert!(needs_resync(&event(
            serde_json::json!({"EventID": "e", "Labels": [{"ID": "l", "Action": 1}]})
        )));
        assert!(needs_resync(&event(
            serde_json::json!({"EventID": "e", "Addresses": [{"ID": "a", "Action": 0}]})
        )));
        assert!(!needs_resync(&event(
            serde_json::json!({"EventID": "e", "Messages": [{"ID": "m", "Action": 0}]})
        )));
    }
}
