//! Incremental JMAP mail sync through `Email/changes` (RFC 8620 section
//! 5.2).
//!
//! The full sync queries every email id and fetches every email; this path
//! asks the server what changed since the stored `Email` state and touches
//! only those emails. New emails are fetched in full, emails whose keywords
//! or mailboxes changed are fetched as a few small properties and keep the
//! stored body, and destroyed emails are dropped from the mirror.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use jmap_client::{client::Client, core::response::EmailGetResponse, email::Property};
use zmail_core::{MailboxSnapshot, MessageSnapshot, ThreadSnapshot};

use crate::{JmapMailbox, QUERY_PAGE_SIZE, fetch_emails, message_snapshot};

/// Changes requested per `Email/changes` call.
const MAX_CHANGES_PER_CALL: usize = 500;
/// Above this many changed emails a full resync is cheaper than merging.
const MAX_INCREMENTAL_CHANGES: usize = 5_000;

/// Result of an incremental sync.
#[derive(Debug)]
pub(crate) struct IncrementalSync {
    pub(crate) mailboxes: Vec<MailboxSnapshot>,
    pub(crate) email_state: String,
    pub(crate) changed_emails: usize,
}

/// Syncs the emails that changed since `since_state` into the existing
/// mirror.
///
/// Returns `None` when the mirror cannot be updated incrementally and the
/// caller must run a full sync: the server cannot calculate the changes
/// (`cannotCalculateChanges`, an expired state), the change set is larger
/// than a full sync, or the stored mailbox ids no longer map one to one
/// onto the mirror's mailboxes.
pub(crate) async fn sync_email_changes(
    client: &Client,
    since_state: &str,
    previous_mailbox_ids: &BTreeMap<String, String>,
    details: &[JmapMailbox],
    existing: &[MailboxSnapshot],
    primary_identity: &str,
    attachments_dir: &str,
) -> Result<Option<IncrementalSync>> {
    let Some(by_id) = mailboxes_by_id(previous_mailbox_ids, existing) else {
        return Ok(None);
    };
    let Some(changes) = fetch_email_changes(client, since_state).await? else {
        return Ok(None);
    };

    let created = changes.created.iter().cloned().collect::<HashSet<_>>();
    let light_ids = changes
        .updated
        .iter()
        .filter(|id| !created.contains(*id))
        .cloned()
        .collect::<Vec<_>>();
    let index = message_index(existing);

    let mut placed = Vec::new();
    let mut full_ids = changes.created.clone();
    for update in fetch_email_updates(client, &light_ids).await? {
        match index.get(update.id.as_str()) {
            Some(stored) => {
                let mut message = (*stored).clone();
                message.is_read = !update.unread;
                message.is_flagged = update.flagged;
                placed.push(PlacedEmail {
                    thread_id: update.thread_id,
                    mailbox_ids: update.mailbox_ids,
                    has_attachments: update.has_attachments,
                    message,
                });
            }
            // An update for an email the mirror never stored is fetched in
            // full like a new one.
            None => full_ids.push(update.id),
        }
    }
    for email in fetch_emails(client, &full_ids, attachments_dir).await? {
        placed.push(PlacedEmail {
            message: message_snapshot(&email, primary_identity),
            thread_id: email.thread_id,
            mailbox_ids: email.mailbox_ids,
            has_attachments: email.has_attachments,
        });
    }

    let removed = changes
        .created
        .iter()
        .chain(&changes.updated)
        .chain(&changes.destroyed)
        .cloned()
        .collect::<HashSet<_>>();
    Ok(Some(IncrementalSync {
        mailboxes: apply_email_changes(details, by_id, &removed, placed),
        email_state: changes.new_state,
        changed_emails: removed.len(),
    }))
}

/// Reads the current `Email` state without fetching any email.
///
/// The full sync records this before it queries, so changes that land
/// while it runs are reported by the next `Email/changes` instead of lost.
pub(crate) async fn fetch_email_state(client: &Client) -> Result<String> {
    let mut request = client.build();
    request.get_email().ids(Vec::<String>::new());
    let mut response: EmailGetResponse = request
        .send_get_email()
        .await
        .context("failed to read the JMAP Email state")?;
    Ok(response.take_state())
}

#[derive(Debug, Default)]
struct EmailChanges {
    created: Vec<String>,
    updated: Vec<String>,
    destroyed: Vec<String>,
    new_state: String,
}

/// Collects every change since `since_state`, following `hasMoreChanges`.
///
/// Method errors (`cannotCalculateChanges` and friends) return `None` so
/// the caller falls back to a full sync; transport errors fail the sync.
async fn fetch_email_changes(client: &Client, since_state: &str) -> Result<Option<EmailChanges>> {
    let mut changes = EmailChanges {
        new_state: since_state.to_owned(),
        ..EmailChanges::default()
    };
    loop {
        let mut response = match client
            .email_changes(changes.new_state.clone(), Some(MAX_CHANGES_PER_CALL))
            .await
        {
            Ok(response) => response,
            Err(jmap_client::Error::Method(_)) => return Ok(None),
            Err(error) => return Err(error).context("failed to fetch JMAP Email changes"),
        };
        changes.created.extend(response.take_created());
        changes.updated.extend(response.take_updated());
        changes.destroyed.extend(response.take_destroyed());
        let more = response.has_more_changes();
        let new_state = response.take_new_state();
        // A server that reports more changes without advancing the state
        // would loop forever; the full sync is the safe answer.
        if more && new_state == changes.new_state {
            return Ok(None);
        }
        changes.new_state = new_state;
        let total = changes.created.len() + changes.updated.len() + changes.destroyed.len();
        if total > MAX_INCREMENTAL_CHANGES {
            return Ok(None);
        }
        if !more {
            return Ok(Some(changes));
        }
    }
}

/// The few properties that change on an existing email.
#[derive(Debug)]
struct EmailUpdate {
    id: String,
    thread_id: String,
    mailbox_ids: Vec<String>,
    unread: bool,
    flagged: bool,
    has_attachments: bool,
}

async fn fetch_email_updates(client: &Client, ids: &[String]) -> Result<Vec<EmailUpdate>> {
    let properties = vec![
        Property::Id,
        Property::ThreadId,
        Property::MailboxIds,
        Property::Keywords,
        Property::HasAttachment,
    ];
    let mut updates = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(QUERY_PAGE_SIZE) {
        let mut request = client.build();
        request
            .get_email()
            .ids(chunk.iter())
            .properties(properties.clone());
        let mut response: EmailGetResponse = request
            .send_get_email()
            .await
            .context("failed to fetch changed JMAP emails")?;
        updates.extend(response.take_list().into_iter().filter_map(|email| {
            let id = email.id()?.to_owned();
            Some(EmailUpdate {
                thread_id: email.thread_id().unwrap_or(&id).to_owned(),
                mailbox_ids: email.mailbox_ids().into_iter().map(str::to_owned).collect(),
                unread: !email.keywords().contains(&"$seen"),
                flagged: email.keywords().contains(&"$flagged"),
                has_attachments: email.has_attachment(),
                id,
            })
        }));
    }
    Ok(updates)
}

/// An email to (re)insert into every mailbox it belongs to.
#[derive(Debug, Clone)]
pub(crate) struct PlacedEmail {
    pub(crate) thread_id: String,
    pub(crate) mailbox_ids: Vec<String>,
    pub(crate) has_attachments: bool,
    pub(crate) message: MessageSnapshot,
}

/// Maps the mirror's mailboxes onto the JMAP mailbox ids recorded by the
/// last sync.
///
/// The mirror stores mailboxes by name, so the mapping is only trusted when
/// it is one to one: every recorded id names a distinct mailbox that the
/// mirror still has.
fn mailboxes_by_id(
    previous_mailbox_ids: &BTreeMap<String, String>,
    existing: &[MailboxSnapshot],
) -> Option<HashMap<String, MailboxSnapshot>> {
    let mut by_name = HashMap::with_capacity(existing.len());
    for mailbox in existing {
        if by_name.insert(mailbox.name.as_str(), mailbox).is_some() {
            return None;
        }
    }
    let mut by_id = HashMap::with_capacity(previous_mailbox_ids.len());
    let mut seen_names = HashSet::with_capacity(previous_mailbox_ids.len());
    for (id, name) in previous_mailbox_ids {
        if !seen_names.insert(name.as_str()) {
            return None;
        }
        let mailbox = by_name.get(name.as_str())?;
        by_id.insert(id.clone(), (*mailbox).clone());
    }
    Some(by_id)
}

/// Indexes the mirror's messages by email id; an email in several
/// mailboxes is stored once per mailbox with identical content.
fn message_index(existing: &[MailboxSnapshot]) -> HashMap<&str, &MessageSnapshot> {
    let mut index = HashMap::new();
    for mailbox in existing {
        for thread in &mailbox.threads {
            for message in &thread.messages {
                index.entry(message.remote_id.as_str()).or_insert(message);
            }
        }
    }
    index
}

/// Applies a change set to the mirror.
///
/// Every email in `removed` is dropped from every mailbox first, then each
/// placed email is inserted into the thread of every mailbox it now belongs
/// to, so moves, keyword changes and deletions all reduce to the same two
/// steps. Mailboxes follow the server's current list (`details`), which
/// also renames mailboxes and drops destroyed ones; mailboxes new on the
/// server start empty.
pub(crate) fn apply_email_changes(
    details: &[JmapMailbox],
    mut by_id: HashMap<String, MailboxSnapshot>,
    removed: &HashSet<String>,
    placed: Vec<PlacedEmail>,
) -> Vec<MailboxSnapshot> {
    let mut mailboxes = Vec::with_capacity(details.len());
    let mut positions = HashMap::with_capacity(details.len());
    for detail in details {
        let mut mailbox = by_id.remove(&detail.id).unwrap_or_else(|| MailboxSnapshot {
            name: detail.name.clone(),
            role: None,
            threads: Vec::new(),
        });
        mailbox.name = detail.name.clone();
        mailbox.role = detail.role.clone();
        positions.insert(detail.id.as_str(), mailboxes.len());
        mailboxes.push(mailbox);
    }

    let mut touched = vec![HashSet::<String>::new(); mailboxes.len()];
    if !removed.is_empty() {
        for (position, mailbox) in mailboxes.iter_mut().enumerate() {
            for thread in &mut mailbox.threads {
                let before = thread.messages.len();
                thread
                    .messages
                    .retain(|message| !removed.contains(&message.remote_id));
                if thread.messages.len() != before {
                    touched[position].insert(thread.remote_thread_id.clone());
                }
            }
        }
    }

    for email in placed {
        for mailbox_id in &email.mailbox_ids {
            let Some(&position) = positions.get(mailbox_id.as_str()) else {
                continue;
            };
            let mailbox = &mut mailboxes[position];
            let index = match mailbox
                .threads
                .iter()
                .position(|thread| thread.remote_thread_id == email.thread_id)
            {
                Some(index) => index,
                None => {
                    mailbox.threads.push(ThreadSnapshot {
                        remote_thread_id: email.thread_id.clone(),
                        subject: email.message.subject.clone(),
                        participants: String::new(),
                        preview: email.message.body_preview.clone(),
                        is_flagged: false,
                        has_attachments: false,
                        unread_count: 0,
                        latest_message_at: email.message.received_at.clone(),
                        messages: Vec::new(),
                    });
                    mailbox.threads.len() - 1
                }
            };
            let thread = &mut mailbox.threads[index];
            thread.messages.push(email.message.clone());
            thread.has_attachments |= email.has_attachments;
            touched[position].insert(email.thread_id.clone());
        }
    }

    for (mailbox, touched) in mailboxes.iter_mut().zip(&touched) {
        if touched.is_empty() {
            continue;
        }
        for thread in &mut mailbox.threads {
            if touched.contains(&thread.remote_thread_id) {
                recompute_thread(thread);
            }
        }
        mailbox.threads.retain(|thread| !thread.messages.is_empty());
        mailbox
            .threads
            .sort_by(|left, right| right.latest_message_at.cmp(&left.latest_message_at));
    }
    mailboxes
}

/// Recomputes a thread's aggregate fields from its messages, matching the
/// full sync's grouping: the newest message names the thread.
fn recompute_thread(thread: &mut ThreadSnapshot) {
    thread
        .messages
        .sort_by(|left, right| left.received_at.cmp(&right.received_at));
    if let Some(newest) = thread.messages.last() {
        thread.latest_message_at = newest.received_at.clone();
        thread.subject = newest.subject.clone();
        thread.preview = newest.body_preview.clone();
    }
    thread.participants = zmail_core::participants::thread_participants(&thread.messages);
    thread.unread_count = thread
        .messages
        .iter()
        .filter(|message| !message.is_read)
        .count();
    thread.is_flagged = thread.messages.iter().any(|message| message.is_flagged);
}

/// The ids of the server's mailboxes keyed to their names, recorded in the
/// checkpoint so the next sync can map the mirror back onto ids.
pub(crate) fn mailbox_id_map(details: &[JmapMailbox]) -> BTreeMap<String, String> {
    details
        .iter()
        .map(|mailbox| (mailbox.id.clone(), mailbox.name.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(id: &str, name: &str, role: Option<&str>) -> JmapMailbox {
        JmapMailbox {
            id: id.to_owned(),
            name: name.to_owned(),
            role: role.map(str::to_owned),
            sort_order: 0,
        }
    }

    fn message(id: &str, received_at: &str, read: bool) -> MessageSnapshot {
        MessageSnapshot {
            remote_id: id.to_owned(),
            identity_email: "me@example.test".to_owned(),
            sender: "Ada <ada@example.test>".to_owned(),
            reply_to: None,
            recipients: "me@example.test".to_owned(),
            subject: format!("Subject {id}"),
            body_preview: format!("Preview {id}"),
            body_text: Some(format!("Body {id}")),
            sanitized_html: None,
            has_remote_content: false,
            is_read: read,
            is_flagged: false,
            received_at: received_at.to_owned(),
            attachments: Vec::new(),
        }
    }

    fn thread(id: &str, messages: Vec<MessageSnapshot>) -> ThreadSnapshot {
        let mut thread = ThreadSnapshot {
            remote_thread_id: id.to_owned(),
            subject: String::new(),
            participants: String::new(),
            preview: String::new(),
            is_flagged: false,
            has_attachments: false,
            unread_count: 0,
            latest_message_at: String::new(),
            messages,
        };
        recompute_thread(&mut thread);
        thread
    }

    fn mailbox(name: &str, threads: Vec<ThreadSnapshot>) -> MailboxSnapshot {
        MailboxSnapshot {
            name: name.to_owned(),
            role: None,
            threads,
        }
    }

    fn ids(values: &[&str]) -> HashSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn by_id(entries: Vec<(&str, MailboxSnapshot)>) -> HashMap<String, MailboxSnapshot> {
        entries
            .into_iter()
            .map(|(id, mailbox)| (id.to_owned(), mailbox))
            .collect()
    }

    #[test]
    fn new_emails_join_existing_threads_and_rename_them() {
        let details = [detail("m1", "Inbox", Some("inbox"))];
        let existing = by_id(vec![(
            "m1",
            mailbox(
                "Inbox",
                vec![thread(
                    "t1",
                    vec![message("e1", "2026-10-01T10:00:00Z", true)],
                )],
            ),
        )]);
        let reply = message("e2", "2026-10-02T10:00:00Z", false);
        let mailboxes = apply_email_changes(
            &details,
            existing,
            &ids(&["e2"]),
            vec![PlacedEmail {
                thread_id: "t1".to_owned(),
                mailbox_ids: vec!["m1".to_owned()],
                has_attachments: true,
                message: reply,
            }],
        );

        assert_eq!(mailboxes.len(), 1);
        assert_eq!(mailboxes[0].role.as_deref(), Some("inbox"));
        let thread = &mailboxes[0].threads[0];
        assert_eq!(thread.messages.len(), 2);
        assert_eq!(thread.subject, "Subject e2", "the newest message names it");
        assert_eq!(thread.latest_message_at, "2026-10-02T10:00:00Z");
        assert_eq!(thread.unread_count, 1);
        assert!(thread.has_attachments);
    }

    #[test]
    fn moves_keyword_changes_and_destroys_apply_in_one_pass() {
        let details = [
            detail("m1", "Inbox", Some("inbox")),
            detail("m2", "Archive", Some("archive")),
        ];
        let existing = by_id(vec![
            (
                "m1",
                mailbox(
                    "Inbox",
                    vec![
                        thread("t1", vec![message("e1", "2026-10-01T10:00:00Z", false)]),
                        thread("t2", vec![message("e2", "2026-10-02T10:00:00Z", false)]),
                        thread("t3", vec![message("e3", "2026-10-03T10:00:00Z", false)]),
                    ],
                ),
            ),
            ("m2", mailbox("Archive", Vec::new())),
        ]);

        // e1 moves to Archive, e2 is read in place, e3 is destroyed.
        let mut moved = message("e1", "2026-10-01T10:00:00Z", false);
        moved.is_flagged = true;
        let read = message("e2", "2026-10-02T10:00:00Z", true);
        let mailboxes = apply_email_changes(
            &details,
            existing,
            &ids(&["e1", "e2", "e3"]),
            vec![
                PlacedEmail {
                    thread_id: "t1".to_owned(),
                    mailbox_ids: vec!["m2".to_owned()],
                    has_attachments: false,
                    message: moved,
                },
                PlacedEmail {
                    thread_id: "t2".to_owned(),
                    mailbox_ids: vec!["m1".to_owned()],
                    has_attachments: false,
                    message: read,
                },
            ],
        );

        let inbox = &mailboxes[0];
        assert_eq!(inbox.threads.len(), 1, "moved and destroyed threads leave");
        assert_eq!(inbox.threads[0].remote_thread_id, "t2");
        assert_eq!(inbox.threads[0].unread_count, 0);
        let archive = &mailboxes[1];
        assert_eq!(archive.threads.len(), 1);
        assert_eq!(archive.threads[0].remote_thread_id, "t1");
        assert!(archive.threads[0].is_flagged);
        assert_eq!(
            archive.threads[0].messages[0].body_text.as_deref(),
            Some("Body e1"),
            "a moved email keeps its stored body"
        );
    }

    #[test]
    fn emails_in_several_mailboxes_land_in_each() {
        let details = [detail("m1", "Inbox", None), detail("m2", "Work", None)];
        let mailboxes = apply_email_changes(
            &details,
            by_id(vec![
                ("m1", mailbox("Inbox", Vec::new())),
                ("m2", mailbox("Work", Vec::new())),
            ]),
            &ids(&["e1"]),
            vec![PlacedEmail {
                thread_id: "t1".to_owned(),
                mailbox_ids: vec!["m1".to_owned(), "m2".to_owned(), "gone".to_owned()],
                has_attachments: false,
                message: message("e1", "2026-10-01T10:00:00Z", false),
            }],
        );
        assert_eq!(mailboxes[0].threads.len(), 1);
        assert_eq!(mailboxes[1].threads.len(), 1);
    }

    #[test]
    fn mailboxes_follow_the_server_list() {
        let details = [
            detail("m1", "Inbox", Some("inbox")),
            detail("m3", "Receipts", None),
            detail("m2", "Old Name Renamed", None),
        ];
        let existing = by_id(vec![
            (
                "m1",
                mailbox(
                    "Inbox",
                    vec![thread(
                        "t1",
                        vec![message("e1", "2026-10-01T10:00:00Z", true)],
                    )],
                ),
            ),
            (
                "m2",
                mailbox(
                    "Old Name",
                    vec![thread(
                        "t2",
                        vec![message("e2", "2026-10-01T10:00:00Z", true)],
                    )],
                ),
            ),
            ("m9", mailbox("Destroyed", Vec::new())),
        ]);
        let mailboxes = apply_email_changes(&details, existing, &HashSet::new(), Vec::new());
        let names = mailboxes
            .iter()
            .map(|mailbox| mailbox.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["Inbox", "Receipts", "Old Name Renamed"]);
        assert_eq!(
            mailboxes[2].threads[0].remote_thread_id, "t2",
            "a renamed mailbox keeps its threads"
        );
        assert!(mailboxes[1].threads.is_empty());
    }

    #[test]
    fn mailbox_ids_map_only_when_one_to_one() {
        let existing = vec![mailbox("Inbox", Vec::new()), mailbox("Archive", Vec::new())];
        let map = |entries: &[(&str, &str)]| {
            entries
                .iter()
                .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
                .collect::<BTreeMap<_, _>>()
        };

        let mapped = mailboxes_by_id(&map(&[("m1", "Inbox"), ("m2", "Archive")]), &existing)
            .expect("a one-to-one mapping");
        assert_eq!(mapped["m2"].name, "Archive");

        assert!(
            mailboxes_by_id(&map(&[("m1", "Inbox"), ("m2", "Missing")]), &existing).is_none(),
            "a recorded mailbox the mirror lost forces a full sync"
        );
        assert!(
            mailboxes_by_id(&map(&[("m1", "Inbox"), ("m2", "Inbox")]), &existing).is_none(),
            "duplicate names cannot be told apart"
        );
        let duplicated = vec![mailbox("Inbox", Vec::new()), mailbox("Inbox", Vec::new())];
        assert!(mailboxes_by_id(&map(&[("m1", "Inbox")]), &duplicated).is_none());
    }

    #[test]
    fn message_index_finds_stored_messages() {
        let existing = vec![mailbox(
            "Inbox",
            vec![thread(
                "t1",
                vec![message("e1", "2026-10-01T10:00:00Z", true)],
            )],
        )];
        let index = message_index(&existing);
        assert_eq!(
            index.get("e1").map(|message| message.subject.as_str()),
            Some("Subject e1")
        );
        assert!(!index.contains_key("e2"));
    }
}
