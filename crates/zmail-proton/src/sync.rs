// The metadata pass is ported from proton-bridge
// `internal/services/syncservice/stage_metadata.go` (`metadataIterator`,
// `MetadataPageSize`) at v3.27.1 (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust as one sequential
// pass that builds Zmail mailbox snapshots instead of Bridge's download
// pipeline; the snapshot building is Zmail's own.

//! Read sync: labels to mailboxes and message metadata into the store.
//!
//! The metadata pass follows Bridge's sync service
//! (`internal/services/syncservice/stage_metadata.go`): newest first, 128
//! messages a page, each page continuing from the last message ID of the
//! one before. Only metadata is fetched; bodies stay encrypted on the
//! server until a message is opened (hydration).

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::{DateTime, Utc};
use zmail_core::{MailboxSnapshot, MessageSnapshot, ThreadSnapshot};
use zmail_proton_api::{Client, EmailAddress, Label, LabelType, MessageFilter, MessageMetadata};

use crate::mailbox::{DELIMITER, ProtonMailbox, mailboxes_for};

/// Bridge's `MetadataPageSize`.
pub(crate) const METADATA_PAGE_SIZE: usize = 128;

/// Every label type Bridge mirrors.
pub(crate) const LABEL_TYPES: [LabelType; 3] =
    [LabelType::System, LabelType::Folder, LabelType::Label];

/// Fetches every message's metadata, newest first (Bridge's
/// `metadataIterator`).
pub(crate) async fn fetch_all_metadata(client: &Client) -> Result<Vec<MessageMetadata>> {
    let mut all = Vec::new();
    let mut last_id = String::new();
    loop {
        let filter = MessageFilter {
            end_id: last_id.clone(),
            desc: true,
            ..MessageFilter::default()
        };
        let mut page = client
            .get_message_metadata_page(0, METADATA_PAGE_SIZE, &filter)
            .await?;
        // The cursor message comes back first when it still exists.
        if !last_id.is_empty() && page.first().is_some_and(|meta| meta.id == last_id) {
            page.remove(0);
        }
        let Some(last) = page.last() else {
            return Ok(all);
        };
        last_id = last.id.clone();
        all.extend(page);
    }
}

/// Builds the mailbox snapshots from the labels and message metadata.
///
/// A message appears in every mailbox of its labels (Proton labels, like
/// JMAP mailboxes, can overlap); Starred sets the flagged state instead.
/// Messages group into threads by Proton conversation, or stand alone when
/// the API gives none.
pub(crate) fn build_mailboxes(
    labels: &[Label],
    metadata: &[MessageMetadata],
    identity_for_address: &HashMap<String, String>,
    fallback_identity: &str,
) -> Vec<MailboxSnapshot> {
    let (mailboxes, index) = mailboxes_for(labels);
    let mut threads: Vec<BTreeMap<String, ThreadAccumulator>> =
        vec![BTreeMap::new(); mailboxes.len()];
    for meta in metadata {
        let identity = identity_for_address
            .get(&meta.address_id)
            .map(String::as_str)
            .unwrap_or(fallback_identity);
        let message = message_snapshot(meta, identity);
        let thread_id = if meta.conversation_id.is_empty() {
            meta.id.clone()
        } else {
            meta.conversation_id.clone()
        };
        for label_id in &meta.label_ids {
            if let Some(&position) = index.get(label_id) {
                threads[position]
                    .entry(thread_id.clone())
                    .or_insert_with(|| ThreadAccumulator::new(&thread_id))
                    .push(message.clone(), meta.num_attachments > 0);
            }
        }
    }
    mailboxes
        .into_iter()
        .zip(threads)
        .map(|(mailbox, threads)| mailbox_snapshot(mailbox, threads))
        .collect()
}

fn mailbox_snapshot(
    mailbox: ProtonMailbox,
    threads: BTreeMap<String, ThreadAccumulator>,
) -> MailboxSnapshot {
    let mut threads: Vec<ThreadSnapshot> = threads
        .into_values()
        .map(ThreadAccumulator::into_snapshot)
        .collect();
    threads.sort_by(|left, right| right.latest_message_at.cmp(&left.latest_message_at));
    MailboxSnapshot {
        name: mailbox.name,
        role: mailbox.role.map(str::to_owned),
        delimiter: Some(DELIMITER.to_owned()),
        parent: mailbox.parent,
        threads,
    }
}

/// A headers-only message snapshot: the body is filled in by hydration.
fn message_snapshot(meta: &MessageMetadata, identity_email: &str) -> MessageSnapshot {
    let subject = if meta.subject.trim().is_empty() {
        "(no subject)".to_owned()
    } else {
        meta.subject.clone()
    };
    let recipients = format_addresses(&meta.to_list);
    MessageSnapshot {
        remote_id: meta.id.clone(),
        identity_email: identity_email.to_owned(),
        sender: meta
            .sender
            .as_ref()
            .map(format_address)
            .filter(|sender| !sender.is_empty())
            .unwrap_or_else(|| "Unknown sender".to_owned()),
        reply_to: meta.reply_tos.first().map(format_address),
        recipients: if recipients.is_empty() {
            identity_email.to_owned()
        } else {
            recipients
        },
        subject,
        body_preview: String::new(),
        body_text: None,
        sanitized_html: None,
        has_remote_content: false,
        is_read: meta.seen(),
        is_flagged: meta.starred(),
        received_at: timestamp(meta.time),
        attachments: Vec::new(),
    }
}

fn format_address(address: &EmailAddress) -> String {
    if address.name.trim().is_empty() {
        address.address.clone()
    } else {
        format!("{} <{}>", address.name, address.address)
    }
}

fn format_addresses(addresses: &[EmailAddress]) -> String {
    addresses
        .iter()
        .map(format_address)
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

fn timestamp(seconds: i64) -> String {
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|time| time.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_owned())
}

/// Collects one conversation's messages within a mailbox.
#[derive(Debug, Clone)]
struct ThreadAccumulator {
    remote_thread_id: String,
    subject: String,
    participants: Vec<String>,
    is_flagged: bool,
    has_attachments: bool,
    unread_count: usize,
    latest_message_at: String,
    messages: Vec<MessageSnapshot>,
}

impl ThreadAccumulator {
    fn new(remote_thread_id: &str) -> Self {
        Self {
            remote_thread_id: remote_thread_id.to_owned(),
            subject: String::new(),
            participants: Vec::new(),
            is_flagged: false,
            has_attachments: false,
            unread_count: 0,
            latest_message_at: String::new(),
            messages: Vec::new(),
        }
    }

    fn push(&mut self, message: MessageSnapshot, has_attachments: bool) {
        if message.received_at >= self.latest_message_at {
            self.latest_message_at = message.received_at.clone();
            self.subject = message.subject.clone();
        }
        if !self.participants.contains(&message.sender) {
            self.participants.push(message.sender.clone());
        }
        self.is_flagged |= message.is_flagged;
        self.has_attachments |= has_attachments;
        self.unread_count += usize::from(!message.is_read);
        self.messages.push(message);
    }

    fn into_snapshot(mut self) -> ThreadSnapshot {
        self.messages
            .sort_by(|left, right| left.received_at.cmp(&right.received_at));
        ThreadSnapshot {
            remote_thread_id: self.remote_thread_id,
            subject: self.subject,
            participants: self.participants.join(", "),
            preview: String::new(),
            is_flagged: self.is_flagged,
            has_attachments: self.has_attachments,
            unread_count: self.unread_count,
            latest_message_at: self.latest_message_at,
            messages: self.messages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::tests::label;

    fn meta(
        id: &str,
        conversation: &str,
        labels: &[&str],
        time: i64,
        unread: bool,
    ) -> MessageMetadata {
        serde_json::from_value(serde_json::json!({
            "ID": id,
            "AddressID": "addr-1",
            "ConversationID": conversation,
            "LabelIDs": labels,
            "Subject": format!("Subject {id}"),
            "Sender": {"Name": "Alice", "Address": "alice@example.com"},
            "ToList": [{"Name": "", "Address": "zmail@proton.local"}],
            "Flags": 1,
            "Time": time,
            "Unread": i64::from(unread),
            "NumAttachments": 0,
        }))
        .expect("metadata")
    }

    #[test]
    fn places_messages_in_every_labelled_mailbox() {
        let labels = [
            label("0", "Inbox", "Inbox", 4),
            label("5", "All Mail", "All Mail", 4),
            label("10", "Starred", "Starred", 4),
            label("l", "Family", "Family", 1),
            label("f", "Work", "Work", 3),
        ];
        let metadata = [
            meta("m1", "c1", &["0", "5", "l", "10"], 1_700_000_100, true),
            meta("m2", "c1", &["0", "5"], 1_700_000_000, false),
            meta("m3", "", &["f", "5"], 1_700_000_200, false),
        ];
        let identities = HashMap::from([("addr-1".to_owned(), "zmail@proton.local".to_owned())]);
        let mailboxes = build_mailboxes(&labels, &metadata, &identities, "fallback@proton.local");

        let by_name: HashMap<&str, &MailboxSnapshot> =
            mailboxes.iter().map(|m| (m.name.as_str(), m)).collect();
        assert_eq!(
            mailboxes
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["INBOX", "All Mail", "Labels/Family", "Folders/Work"]
        );
        let inbox = by_name["INBOX"];
        assert_eq!(inbox.role.as_deref(), Some("inbox"));
        assert_eq!(inbox.threads.len(), 1, "m1 and m2 share conversation c1");
        let thread = &inbox.threads[0];
        assert_eq!(thread.remote_thread_id, "c1");
        assert_eq!(thread.messages.len(), 2);
        assert_eq!(thread.unread_count, 1);
        assert!(thread.is_flagged, "m1 is starred");
        assert_eq!(
            thread.subject, "Subject m1",
            "the latest message names the thread"
        );
        let m1 = thread
            .messages
            .iter()
            .find(|m| m.remote_id == "m1")
            .expect("m1");
        assert!(m1.is_flagged && !m1.is_read);
        assert_eq!(m1.sender, "Alice <alice@example.com>");
        assert_eq!(m1.recipients, "zmail@proton.local");
        assert_eq!(m1.identity_email, "zmail@proton.local");
        assert!(m1.body_text.is_none(), "bodies come from hydration");

        assert_eq!(by_name["All Mail"].threads.len(), 2);
        assert_eq!(by_name["Labels/Family"].threads.len(), 1);
        let work = by_name["Folders/Work"];
        assert_eq!(work.parent.as_deref(), Some("Folders"));
        assert_eq!(
            work.threads[0].remote_thread_id, "m3",
            "no conversation: one per message"
        );
        assert!(!by_name.contains_key("Starred"));
    }
}
