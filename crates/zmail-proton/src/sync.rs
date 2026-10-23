// The metadata pass is ported from proton-bridge
// `internal/services/syncservice/stage_metadata.go` (`metadataIterator`,
// `MetadataPageSize`) at v3.27.1 (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust as one sequential
// pass that builds Zmail mailbox snapshots instead of Bridge's download
// pipeline; the snapshot building is Zmail's own. Modified on 2026-10-23:
// the pass stops after a number of pages and resumes later from its cursor,
// so a first sync shows the newest mail and the rest arrives in chunks.

//! Read sync: labels to mailboxes and message metadata into the store.
//!
//! The metadata pass follows Bridge's sync service
//! (`internal/services/syncservice/stage_metadata.go`): newest first, 128
//! messages a page, each page continuing from the last message ID of the
//! one before. Only metadata is fetched; bodies stay encrypted on the
//! server until a message is opened (hydration).
//!
//! Unlike Bridge, Zmail does not list the whole mailbox before showing it.
//! A first sync lists the newest [`FIRST_SYNC_PAGES`] pages and writes
//! them; each later sync lists up to [`BACKFILL_PAGES_PER_RUN`] older pages
//! from the stored cursor and adds them to the mirror, until a page comes
//! back empty.

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use zmail_core::{MailboxSnapshot, MessageSnapshot, ThreadSnapshot};
use zmail_proton_api::{
    Client, EmailAddress, Label, LabelType, MessageFilter, MessageMetadata, STARRED_LABEL,
};

use crate::mailbox::{DELIMITER, ProtonMailbox, mailboxes_for};

/// Bridge's `MetadataPageSize`.
pub(crate) const METADATA_PAGE_SIZE: usize = 128;

/// Pages a first sync lists before the account is shown: the newest 512
/// messages.
pub(crate) const FIRST_SYNC_PAGES: usize = 4;

/// Older pages each later sync lists while the history is still arriving:
/// 1,024 messages a run, with event polling, write-back and sending in
/// between runs.
pub(crate) const BACKFILL_PAGES_PER_RUN: usize = 8;

/// Every label type Bridge mirrors.
pub(crate) const LABEL_TYPES: [LabelType; 3] =
    [LabelType::System, LabelType::Folder, LabelType::Label];

/// How much of the message listing one sync does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncPaging {
    /// Messages a page (Bridge's `MetadataPageSize`).
    pub page_size: usize,
    /// Pages a first sync lists before it writes the mirror.
    pub first_sync_pages: usize,
    /// Older pages each later sync lists until the history is complete.
    pub backfill_pages_per_run: usize,
}

impl Default for SyncPaging {
    fn default() -> Self {
        Self {
            page_size: METADATA_PAGE_SIZE,
            first_sync_pages: FIRST_SYNC_PAGES,
            backfill_pages_per_run: BACKFILL_PAGES_PER_RUN,
        }
    }
}

/// One stretch of the message listing.
#[derive(Debug, Default)]
pub(crate) struct MetadataPages {
    /// The messages listed, newest first.
    pub(crate) messages: Vec<MessageMetadata>,
    /// The oldest message listed, to carry on from, when the listing
    /// stopped at its page limit; `None` when it reached the oldest
    /// message.
    pub(crate) next_end_id: Option<String>,
}

/// Lists up to `pages` pages of metadata, newest first, from the message
/// `end_id` (or the newest message when it is empty), the way Bridge's
/// `metadataIterator` does: each page ends at the last ID of the page
/// before, which the server returns again first and is dropped. The
/// listing is complete when a page comes back with nothing new. `listed`
/// is called after each page with the number listed so far.
pub(crate) async fn fetch_metadata_pages(
    client: &Client,
    end_id: &str,
    pages: usize,
    page_size: usize,
    listed: &(dyn Fn(usize) + Send + Sync),
) -> Result<MetadataPages> {
    list_pages(
        |filter| async move {
            client
                .get_message_metadata_page(0, page_size, &filter)
                .await
                .map_err(anyhow::Error::from)
        },
        end_id,
        pages,
        listed,
    )
    .await
}

/// The cursor walk of [`fetch_metadata_pages`], over any page source.
async fn list_pages<F, Fut>(
    mut fetch: F,
    end_id: &str,
    pages: usize,
    listed: &(dyn Fn(usize) + Send + Sync),
) -> Result<MetadataPages>
where
    F: FnMut(MessageFilter) -> Fut,
    Fut: Future<Output = Result<Vec<MessageMetadata>>>,
{
    let mut all = Vec::new();
    let mut last_id = end_id.to_owned();
    for _ in 0..pages.max(1) {
        let mut page = fetch(MessageFilter {
            end_id: last_id.clone(),
            desc: true,
            ..MessageFilter::default()
        })
        .await?;
        // The cursor message comes back first when it still exists.
        if !last_id.is_empty() && page.first().is_some_and(|meta| meta.id == last_id) {
            page.remove(0);
        }
        let Some(last) = page.last() else {
            return Ok(MetadataPages {
                messages: all,
                next_end_id: None,
            });
        };
        last_id = last.id.clone();
        all.extend(page);
        listed(all.len());
    }
    Ok(MetadataPages {
        messages: all,
        next_end_id: Some(last_id),
    })
}

/// A message and where it goes: its labels and conversation.
#[derive(Debug, Clone)]
pub(crate) struct PlacedMessage {
    pub(crate) snapshot: MessageSnapshot,
    pub(crate) label_ids: Vec<String>,
    pub(crate) thread_id: String,
    pub(crate) has_attachments: bool,
}

impl PlacedMessage {
    /// Places a message from its metadata.
    pub(crate) fn from_metadata(meta: &MessageMetadata, identity_email: &str) -> Self {
        Self {
            snapshot: message_snapshot(meta, identity_email),
            label_ids: meta.label_ids.clone(),
            thread_id: if meta.conversation_id.is_empty() {
                meta.id.clone()
            } else {
                meta.conversation_id.clone()
            },
            has_attachments: meta.num_attachments > 0,
        }
    }

    /// Keeps the body fields a previous hydration stored, so a flag or
    /// label change does not drop an opened message's body.
    pub(crate) fn keep_body_of(mut self, previous: &MessageSnapshot) -> Self {
        self.snapshot.body_preview = previous.body_preview.clone();
        self.snapshot.body_text = previous.body_text.clone();
        self.snapshot.sanitized_html = previous.sanitized_html.clone();
        self.snapshot.has_remote_content = previous.has_remote_content;
        self.snapshot.attachments = previous.attachments.clone();
        self
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
    place_messages(
        labels,
        metadata.iter().map(|meta| {
            let identity = identity_for_address
                .get(&meta.address_id)
                .map(String::as_str)
                .unwrap_or(fallback_identity);
            PlacedMessage::from_metadata(meta, identity)
        }),
    )
}

/// Builds the mailbox snapshots from placed messages.
pub(crate) fn place_messages(
    labels: &[Label],
    messages: impl IntoIterator<Item = PlacedMessage>,
) -> Vec<MailboxSnapshot> {
    let (mailboxes, index) = mailboxes_for(labels);
    let mut threads: Vec<BTreeMap<String, ThreadAccumulator>> =
        vec![BTreeMap::new(); mailboxes.len()];
    for placed in messages {
        for label_id in &placed.label_ids {
            if let Some(&position) = index.get(label_id) {
                threads[position]
                    .entry(placed.thread_id.clone())
                    .or_insert_with(|| ThreadAccumulator::new(&placed.thread_id))
                    .push(placed.snapshot.clone(), placed.has_attachments);
            }
        }
    }
    mailboxes
        .into_iter()
        .zip(threads)
        .map(|(mailbox, threads)| mailbox_snapshot(mailbox, threads))
        .collect()
}

/// Recovers the placed messages from the stored mirror: each message's
/// labels are the mailboxes it is in, plus Starred when it is flagged.
pub(crate) fn placed_from_mirror(
    labels: &[Label],
    mirror: &[MailboxSnapshot],
) -> HashMap<String, PlacedMessage> {
    let (mailboxes, _) = mailboxes_for(labels);
    let label_for_name: HashMap<&str, &str> = mailboxes
        .iter()
        .map(|mailbox| (mailbox.name.as_str(), mailbox.label_id.as_str()))
        .collect();
    let mut placed: HashMap<String, PlacedMessage> = HashMap::new();
    for mailbox in mirror {
        let Some(label_id) = label_for_name.get(mailbox.name.as_str()) else {
            continue;
        };
        for thread in &mailbox.threads {
            for message in &thread.messages {
                let entry =
                    placed
                        .entry(message.remote_id.clone())
                        .or_insert_with(|| PlacedMessage {
                            snapshot: message.clone(),
                            label_ids: if message.is_flagged {
                                vec![STARRED_LABEL.to_owned()]
                            } else {
                                Vec::new()
                            },
                            thread_id: thread.remote_thread_id.clone(),
                            has_attachments: thread.has_attachments,
                        });
                entry.label_ids.push((*label_id).to_owned());
            }
        }
    }
    placed
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

    /// A listing that pages like Proton's: newest first, `EndID`
    /// inclusive, `page_size` at a time.
    fn listing(
        messages: &[MessageMetadata],
        page_size: usize,
        requests: &std::sync::Mutex<usize>,
        filter: MessageFilter,
    ) -> Result<Vec<MessageMetadata>> {
        *requests.lock().expect("requests") += 1;
        assert!(filter.desc);
        let start = if filter.end_id.is_empty() {
            0
        } else {
            messages
                .iter()
                .position(|meta| meta.id == filter.end_id)
                .expect("the cursor message exists")
        };
        Ok(messages
            .iter()
            .skip(start)
            .take(page_size)
            .cloned()
            .collect())
    }

    fn ids(messages: &[MessageMetadata]) -> Vec<String> {
        messages.iter().map(|meta| meta.id.clone()).collect()
    }

    fn newest_first(count: usize) -> Vec<MessageMetadata> {
        (0..count)
            .map(|index| {
                let id = format!("m{:03}", count - index);
                meta(&id, "", &["5"], 1_700_000_000 - index as i64, false)
            })
            .collect()
    }

    #[tokio::test]
    async fn pages_resume_from_the_cursor_without_duplicates() {
        let messages = newest_first(10);
        let requests = std::sync::Mutex::new(0);
        let fetch = |filter| std::future::ready(listing(&messages, 4, &requests, filter));
        let counts = std::sync::Mutex::new(Vec::new());
        let listed = |count| counts.lock().expect("counts").push(count);

        // The first stretch stops at its page limit with a cursor.
        let first = list_pages(fetch, "", 2, &listed).await.expect("first");
        assert_eq!(ids(&first.messages), ids(&messages[..7]));
        assert_eq!(first.next_end_id.as_deref(), Some("m004"));
        assert_eq!(*counts.lock().expect("counts"), [4, 7]);

        // The next carries on after the cursor and finds the end.
        let end_id = first.next_end_id.expect("cursor");
        let rest = list_pages(fetch, &end_id, 8, &|_| {}).await.expect("rest");
        assert_eq!(ids(&rest.messages), ids(&messages[7..]));
        assert_eq!(rest.next_end_id, None, "an empty page ends the listing");
        // Two pages, then one more that held only the cursor message.
        assert_eq!(*requests.lock().expect("requests"), 2 + 2);
    }

    #[tokio::test]
    async fn a_short_history_completes_in_the_first_stretch() {
        let messages = newest_first(5);
        let requests = std::sync::Mutex::new(0);
        let fetch = |filter| std::future::ready(listing(&messages, 4, &requests, filter));
        let all = list_pages(fetch, "", 4, &|_| {}).await.expect("listing");
        assert_eq!(ids(&all.messages), ids(&messages));
        assert_eq!(all.next_end_id, None);

        let empty: Vec<MessageMetadata> = Vec::new();
        let fetch = |filter| std::future::ready(listing(&empty, 4, &requests, filter));
        let none = list_pages(fetch, "", 4, &|_| {}).await.expect("listing");
        assert!(none.messages.is_empty() && none.next_end_id.is_none());
    }

    #[tokio::test]
    async fn a_stretch_ending_exactly_at_the_oldest_message_finishes_next_time() {
        // 4 + 3 messages fill exactly two pages; only the next request
        // can tell that nothing older exists.
        let messages = newest_first(7);
        let requests = std::sync::Mutex::new(0);
        let fetch = |filter| std::future::ready(listing(&messages, 4, &requests, filter));
        let first = list_pages(fetch, "", 2, &|_| {}).await.expect("first");
        assert_eq!(first.messages.len(), 7);
        let end_id = first.next_end_id.expect("cursor");
        let rest = list_pages(fetch, &end_id, 8, &|_| {}).await.expect("rest");
        assert!(rest.messages.is_empty() && rest.next_end_id.is_none());
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
