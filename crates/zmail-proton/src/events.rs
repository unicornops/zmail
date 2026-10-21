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
// instead of being applied one by one.

//! Incremental sync from the account's event stream.

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use rand::Rng;
use zmail_core::{AccountSyncSnapshot, MailboxSnapshot};
use zmail_proton_api::{Client, Event, EventAction, Label, MessageEvent};

use crate::sync::{self, PlacedMessage};

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

/// Whether anything Zmail mirrors changed after `checkpoint` (one event
/// poll; the sync then fetches and applies the events).
pub async fn has_new_events(session: &crate::ProtonSession, checkpoint: &str) -> Result<bool> {
    let (events, more) = session.client().get_events(checkpoint).await?;
    Ok(more || events.iter().any(Event::has_changes))
}

/// Applies the events after `checkpoint` to the stored mirror. Returns
/// `None` when the account needs a full sync instead.
pub(crate) async fn apply_events(
    client: &Client,
    checkpoint: &str,
    mirror: &[MailboxSnapshot],
) -> Result<Option<AccountSyncSnapshot>> {
    let mut events = Vec::new();
    let mut from = checkpoint.to_owned();
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
        return Ok(Some(AccountSyncSnapshot {
            checkpoint: from,
            detail: "Proton account up to date".to_owned(),
            identities: None,
            mailboxes: mirror.to_vec(),
        }));
    }

    let labels = client.get_labels(&sync::LABEL_TYPES).await?;
    let identity_for_address: HashMap<String, String> = client
        .get_addresses()
        .await?
        .into_iter()
        .map(|address| (address.id, address.email))
        .collect();
    let mailboxes = apply_message_changes(&labels, mirror, &changes, &identity_for_address);
    Ok(Some(AccountSyncSnapshot {
        checkpoint: from,
        detail: format!("Proton account: applied {} message changes", changes.len()),
        identities: None,
        mailboxes,
    }))
}

/// Applies message events to the mirror and rebuilds the mailboxes.
fn apply_message_changes(
    labels: &[Label],
    mirror: &[MailboxSnapshot],
    changes: &[&MessageEvent],
    identity_for_address: &HashMap<String, String>,
) -> Vec<MailboxSnapshot> {
    let mut placed = sync::placed_from_mirror(labels, mirror);
    for change in changes {
        match (change.action, &change.message) {
            (EventAction::Delete, _) => {
                placed.remove(&change.id);
            }
            (EventAction::Create | EventAction::Update | EventAction::UpdateFlags, Some(meta)) => {
                let previous = placed.remove(&meta.id);
                let identity = identity_for_address
                    .get(&meta.address_id)
                    .cloned()
                    .or_else(|| previous.as_ref().map(|p| p.snapshot.identity_email.clone()))
                    .unwrap_or_default();
                let mut updated = PlacedMessage::from_metadata(meta, &identity);
                if let Some(previous) = &previous {
                    updated = updated.keep_body_of(&previous.snapshot);
                }
                placed.insert(meta.id.clone(), updated);
            }
            // An update without metadata, or an unknown action: nothing to
            // apply here.
            _ => {}
        }
    }
    sync::place_messages(labels, placed.into_values())
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
    fn applies_message_changes_to_the_mirror() {
        use crate::mailbox::tests::label;

        let labels = [
            label("0", "Inbox", "Inbox", 4),
            label("5", "All Mail", "All Mail", 4),
            label("10", "Starred", "Starred", 4),
            label("l", "Family", "Family", 1),
        ];
        let meta = |id: &str, labels: &[&str], unread: bool| {
            serde_json::json!({
                "ID": id, "AddressID": "a1", "LabelIDs": labels, "Subject": format!("S {id}"),
                "Sender": {"Name": "", "Address": "x@example.com"}, "Flags": 1,
                "Time": 1_700_000_000, "Unread": i64::from(unread),
            })
        };
        let identities = HashMap::from([("a1".to_owned(), "me@proton.local".to_owned())]);
        let metadata: Vec<zmail_proton_api::MessageMetadata> = [
            meta("m1", &["0", "5"], true),
            meta("m2", &["0", "5", "l"], false),
            meta("m3", &["5"], false),
        ]
        .into_iter()
        .map(|json| serde_json::from_value(json).expect("metadata"))
        .collect();
        let mut mirror = sync::build_mailboxes(&labels, &metadata, &identities, "me@proton.local");
        // m2 was opened: its stored body must survive a flag change.
        for mailbox in &mut mirror {
            for thread in &mut mailbox.threads {
                for message in &mut thread.messages {
                    if message.remote_id == "m2" {
                        message.body_text = Some("opened body".to_owned());
                    }
                }
            }
        }

        let events: Vec<MessageEvent> = serde_json::from_value(serde_json::json!([
            // m1 read and starred.
            {"ID": "m1", "Action": 3, "Message": meta("m1", &["0", "5", "10"], false)},
            // m2 loses its label.
            {"ID": "m2", "Action": 2, "Message": meta("m2", &["0", "5"], false)},
            // m3 deleted, m4 arrives.
            {"ID": "m3", "Action": 0},
            {"ID": "m4", "Action": 1, "Message": meta("m4", &["0", "5"], true)},
        ]))
        .expect("events");
        let changes: Vec<&MessageEvent> = events.iter().collect();
        let mailboxes = apply_message_changes(&labels, &mirror, &changes, &identities);

        let ids = |name: &str| {
            let mut ids: Vec<String> = mailboxes
                .iter()
                .find(|mailbox| mailbox.name == name)
                .map(|mailbox| {
                    mailbox
                        .threads
                        .iter()
                        .flat_map(|thread| thread.messages.iter().map(|m| m.remote_id.clone()))
                        .collect()
                })
                .unwrap_or_default();
            ids.sort();
            ids
        };
        assert_eq!(ids("INBOX"), ["m1", "m2", "m4"]);
        assert_eq!(ids("All Mail"), ["m1", "m2", "m4"]);
        assert!(ids("Labels/Family").is_empty());
        let messages: HashMap<String, &zmail_core::MessageSnapshot> = mailboxes[0]
            .threads
            .iter()
            .flat_map(|thread| thread.messages.iter())
            .map(|message| (message.remote_id.clone(), message))
            .collect();
        assert!(messages["m1"].is_read && messages["m1"].is_flagged);
        assert_eq!(messages["m2"].body_text.as_deref(), Some("opened body"));
        assert!(!messages["m4"].is_read);
        assert_eq!(messages["m4"].identity_email, "me@proton.local");
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
