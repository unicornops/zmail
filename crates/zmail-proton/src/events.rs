// Applying events follows proton-bridge
// `internal/services/imapservice/service_message_events.go` (create,
// update, update-flags and delete),
// `internal/services/imapservice/service_label_events.go` (labels create,
// rename and delete mailboxes), `service_address_events.go` (addresses
// update the identity state) and
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
// reading or rewriting the rest of the mirror. Modified on 2026-11-09
// (#418): label, address and user changes are applied in place as Bridge
// applies them, from the labels and addresses read again and the stored
// label map, so they no longer resync; only a refresh does.

//! Incremental sync from the account's event stream.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use rand::RngExt;
use zmail_core::{MailDelta, MailboxRename, text::count_noun};
use zmail_proton_api::{Client, Event, EventAction, Label, MessageEvent, MessageMetadata};

use crate::{
    ProtonCheckpoint, address_identities, identities_for,
    mailbox::{DELIMITER, mailbox_for},
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
    EVENT_PERIOD + Duration::from_millis(rand::rng().random_range(0..=jitter_ms))
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

/// What the events after a checkpoint ask for.
#[derive(Debug)]
pub(crate) enum EventsOutcome {
    /// Changes to apply to the mirror in place.
    Applied(Box<AppliedEvents>),
    /// Proton asked for a full resync (`RefreshMail`).
    Refresh,
}

/// The events after a checkpoint, read.
#[derive(Debug)]
pub(crate) struct AppliedEvents {
    /// The last event read: the next checkpoint.
    pub(crate) event_id: String,
    /// The changes, or `None` when no event changed anything Zmail
    /// mirrors.
    pub(crate) delta: Option<MailDelta>,
    /// Messages the events created, newest state last: the candidates for
    /// new-mail notifications.
    pub(crate) created: Vec<String>,
    /// The label map to store, when the labels were read.
    pub(crate) labels: Option<BTreeMap<String, String>>,
    /// What changed, for the status line.
    pub(crate) summary: ChangeSummary,
}

/// What a batch of events changed, for the status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChangeSummary {
    /// Message changes read.
    pub(crate) messages: usize,
    /// Mailboxes renamed or moved.
    pub(crate) renamed: usize,
    /// Mailboxes added.
    pub(crate) added: usize,
    /// Mailboxes removed.
    pub(crate) removed: usize,
    /// The identities were read again (an address or user change).
    pub(crate) identities: bool,
}

impl ChangeSummary {
    /// "Proton account: applied 2 message changes, 1 mailbox renamed,
    /// identities updated", or "Proton account: identities updated".
    pub(crate) fn detail(&self) -> String {
        let mut parts = Vec::new();
        if self.messages > 0 || (self.renamed + self.added + self.removed == 0 && !self.identities)
        {
            parts.push(format!(
                "applied {} {}",
                self.messages,
                if self.messages == 1 {
                    "message change"
                } else {
                    "message changes"
                }
            ));
        }
        for (count, verb) in [
            (self.renamed, "renamed"),
            (self.added, "added"),
            (self.removed, "removed"),
        ] {
            if count > 0 {
                parts.push(format!(
                    "{} {verb}",
                    count_noun(count, "mailbox", "mailboxes")
                ));
            }
        }
        if self.identities {
            parts.push("identities updated".to_owned());
        }
        format!("Proton account: {}", parts.join(", "))
    }
}

/// Reads the events after the checkpoint and turns them into changes to
/// the mirror, the way Bridge applies each event to its IMAP state:
///
/// - message changes become a delta of just those messages;
/// - label changes rename, add and remove mailboxes, worked out from the
///   labels read again against the stored label map and the mailboxes the
///   mirror holds ([`mailbox_changes`]); a new mailbox's messages are
///   listed by label;
/// - address and user changes read the identities again.
///
/// Only a refresh request asks for a full resync. `stored` holds the
/// mirror's mailbox names.
///
/// While older mail is still being fetched the mirror holds only part of
/// the history; an event for a message not listed yet places it now, and
/// the backfill listing it later finds it already there.
pub(crate) async fn apply_events(
    client: &Client,
    checkpoint: &ProtonCheckpoint,
    stored: &HashSet<String>,
    page_size: usize,
) -> Result<EventsOutcome> {
    let mut events = Vec::new();
    let mut from = checkpoint.event_id.clone();
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
    if events.iter().any(Event::needs_mail_refresh) {
        return Ok(EventsOutcome::Refresh);
    }

    let changes: Vec<_> = events.iter().flat_map(Event::message_events).collect();
    let labels_changed = events.iter().any(|event| !event.label_events().is_empty());
    let identities_changed = events
        .iter()
        .any(|event| event.user.is_some() || !event.address_events().is_empty());
    if changes.is_empty() && !labels_changed && !identities_changed {
        return Ok(EventsOutcome::Applied(Box::new(AppliedEvents {
            event_id: from,
            delta: None,
            created: Vec::new(),
            labels: None,
            summary: ChangeSummary::default(),
        })));
    }

    let labels = client.get_labels(&sync::LABEL_TYPES).await?;
    let addresses = client.get_addresses().await?;
    let identities = if identities_changed {
        let user = client.get_user().await?;
        Some(identities_for(&user, &addresses)).filter(|identities| !identities.is_empty())
    } else {
        None
    };
    let (identity_for_address, fallback_identity) = address_identities(&addresses, "");

    let label_names = label_map(&labels);
    let structure = if labels_changed {
        mailbox_changes(&checkpoint.labels, &label_names, stored)
    } else {
        MailboxChanges::default()
    };
    let mut listed = Vec::new();
    for label_id in &structure.added {
        listed.extend(sync::fetch_label_messages(client, label_id, page_size).await?);
    }
    let (mut delta, created) = message_delta(
        &labels,
        &changes,
        &listed,
        &identity_for_address,
        &fallback_identity,
    );
    let summary = ChangeSummary {
        messages: changes.len(),
        renamed: structure.renamed.len(),
        added: structure.added.len(),
        removed: structure.removed.len(),
        identities: identities_changed,
    };
    delta.renamed = structure.renamed;
    delta.removed = structure.removed;
    delta.identities = identities;
    // The map is kept from the labels read, once a label change is
    // applied or when an older checkpoint has none yet (the labels as the
    // mirror holds them).
    let labels = if labels_changed {
        Some(label_names)
    } else if checkpoint.labels.is_empty() {
        Some(
            label_names
                .into_iter()
                .filter(|(_, name)| stored.contains(name))
                .collect(),
        )
    } else {
        None
    };
    Ok(EventsOutcome::Applied(Box::new(AppliedEvents {
        event_id: from,
        delta: Some(delta),
        created,
        labels,
        summary,
    })))
}

/// Each mirrored label's mailbox name, by label ID.
pub(crate) fn label_map(labels: &[Label]) -> BTreeMap<String, String> {
    labels
        .iter()
        .filter_map(mailbox_for)
        .map(|mailbox| (mailbox.label_id, mailbox.name))
        .collect()
}

/// How the mirror's mailboxes follow a label change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MailboxChanges {
    /// Mailboxes whose label was renamed or moved (children of a moved
    /// folder included): renamed in place, with their mail.
    pub(crate) renamed: Vec<MailboxRename>,
    /// Mailboxes no label names any more: removed.
    pub(crate) removed: Vec<String>,
    /// Labels whose mailbox the mirror lacks, by label ID: added, and
    /// their messages listed.
    pub(crate) added: Vec<String>,
}

/// Works out the mailbox changes from the label map stored at the last
/// sync (`before`), the labels' mailbox names now (`now`) and the
/// mailboxes the mirror holds (`stored`). A label whose name changed
/// renames its stored mailbox; a stored mailbox no label names is removed;
/// a label whose mailbox is missing is added. Without a stored map (a
/// checkpoint from before 0.33.1) a rename shows up as a removal and an
/// addition, so the renamed label's mail is listed again.
pub(crate) fn mailbox_changes(
    before: &BTreeMap<String, String>,
    now: &BTreeMap<String, String>,
    stored: &HashSet<String>,
) -> MailboxChanges {
    let renamed: Vec<MailboxRename> = now
        .iter()
        .filter_map(|(label_id, name)| {
            let old = before.get(label_id)?;
            (old != name && stored.contains(old)).then(|| MailboxRename {
                from: old.clone(),
                to: name.clone(),
                parent: name
                    .rsplit_once(DELIMITER)
                    .map(|(parent, _)| parent.to_owned())
                    .filter(|parent| !parent.is_empty()),
            })
        })
        .collect();
    let sources: HashSet<&str> = renamed.iter().map(|rename| rename.from.as_str()).collect();
    let targets: HashSet<&str> = renamed.iter().map(|rename| rename.to.as_str()).collect();
    let named: HashSet<&str> = now.values().map(String::as_str).collect();
    let mut removed: Vec<String> = stored
        .iter()
        .filter(|name| !named.contains(name.as_str()) && !sources.contains(name.as_str()))
        .cloned()
        .collect();
    removed.sort();
    let added = now
        .iter()
        .filter(|(_, name)| !stored.contains(*name) && !targets.contains(name.as_str()))
        .map(|(label_id, _)| label_id.clone())
        .collect();
    MailboxChanges {
        renamed,
        removed,
        added,
    }
}

/// The delta for a batch of message events: each changed message placed by
/// its latest labels (none, once deleted), and the messages created.
/// `listed` holds messages listed after the events were read (those of a
/// new mailbox), which are newer still.
fn message_delta(
    labels: &[Label],
    changes: &[&MessageEvent],
    listed: &[MessageMetadata],
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
    for meta in listed {
        latest.insert(&meta.id, Some(meta));
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
        ..MailDelta::default()
    };
    (delta, created.into_iter().map(str::to_owned).collect())
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
        let (delta, created) =
            message_delta(&labels, &changes, &[], &identities, "me@proton.local");

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
    fn only_a_refresh_asks_for_a_resync() {
        assert!(event(serde_json::json!({"EventID": "e", "Refresh": 1})).needs_mail_refresh());
        for structural in [
            serde_json::json!({"EventID": "e", "Labels": [{"ID": "l", "Action": 1}]}),
            serde_json::json!({"EventID": "e", "Addresses": [{"ID": "a", "Action": 0}]}),
            serde_json::json!({"EventID": "e", "User": {"ID": "u"}}),
        ] {
            assert!(!event(structural).needs_mail_refresh());
        }
    }

    fn names(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
            .collect()
    }

    fn stored(list: &[&str]) -> HashSet<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_moved_folder_renames_its_mailbox_and_its_childrens() {
        let before = names(&[
            ("0", "INBOX"),
            ("f1", "Folders/Work"),
            ("f2", "Folders/Work/Sub"),
            ("l1", "Labels/Family"),
            ("l2", "Labels/Old"),
        ]);
        let now = names(&[
            ("0", "INBOX"),
            ("f1", "Folders/Jobs"),
            ("f2", "Folders/Jobs/Sub"),
            ("l1", "Labels/Family"),
            ("l3", "Labels/New"),
        ]);
        let changes = mailbox_changes(
            &before,
            &now,
            &stored(&[
                "INBOX",
                "Folders/Work",
                "Folders/Work/Sub",
                "Labels/Family",
                "Labels/Old",
            ]),
        );
        let rename = |from: &str, to: &str, parent: &str| MailboxRename {
            from: from.to_owned(),
            to: to.to_owned(),
            parent: Some(parent.to_owned()),
        };
        assert_eq!(
            changes,
            MailboxChanges {
                renamed: vec![
                    rename("Folders/Work", "Folders/Jobs", "Folders"),
                    rename("Folders/Work/Sub", "Folders/Jobs/Sub", "Folders/Jobs"),
                ],
                removed: vec!["Labels/Old".to_owned()],
                added: vec!["l3".to_owned()],
            }
        );
    }

    #[test]
    fn without_a_stored_map_a_rename_is_a_removal_and_an_addition() {
        let now = names(&[("0", "INBOX"), ("f1", "Folders/Jobs")]);
        let changes = mailbox_changes(&BTreeMap::new(), &now, &stored(&["INBOX", "Folders/Work"]));
        assert!(changes.renamed.is_empty());
        assert_eq!(changes.removed, ["Folders/Work"]);
        assert_eq!(changes.added, ["f1"], "its mail is listed again");
    }

    #[test]
    fn nothing_changes_when_the_labels_match_the_mirror() {
        let now = names(&[("0", "INBOX"), ("l1", "Labels/Family")]);
        let changes = mailbox_changes(&now, &now, &stored(&["INBOX", "Labels/Family"]));
        assert_eq!(changes, MailboxChanges::default());
    }

    #[test]
    fn words_what_a_batch_changed() {
        let summary = |messages, renamed, added, removed, identities| ChangeSummary {
            messages,
            renamed,
            added,
            removed,
            identities,
        };
        assert_eq!(
            summary(2, 0, 0, 0, false).detail(),
            "Proton account: applied 2 message changes"
        );
        assert_eq!(
            summary(0, 0, 0, 0, false).detail(),
            "Proton account: applied 0 message changes"
        );
        assert_eq!(
            summary(1, 2, 1, 1, true).detail(),
            "Proton account: applied 1 message change, 2 mailboxes renamed, 1 mailbox added, \
             1 mailbox removed, identities updated"
        );
        assert_eq!(
            summary(0, 0, 0, 0, true).detail(),
            "Proton account: identities updated"
        );
    }
}
