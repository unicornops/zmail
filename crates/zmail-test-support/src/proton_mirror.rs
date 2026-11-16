//! A synthetic account shaped like a large Proton mailbox, for measuring
//! what one backfill stretch costs to write (#478).
//!
//! Proton labels overlap: every message is in All Mail and in one folder
//! (Inbox, Archive, Sent, Trash or Spam), and one in four also carries a
//! label. Mail arrives newest first in stretches of
//! [`PROTON_STRETCH_MESSAGES`] metadata-only messages, as
//! `zmail_proton::backfill_older_mail` lists them: no bodies and no
//! previews, the Message-ID and the delivery address as the only headers.
//!
//! Each message is worked out from its position alone (0 is the newest),
//! so any stretch can be generated without the ones before it, and the
//! same position always gives the same message.

use std::collections::BTreeMap;

use zmail_core::{
    AccountSyncSnapshot, MailDelta, MailboxSnapshot, MessageHeaders, MessageSnapshot,
    ThreadSnapshot,
};

/// Messages in one backfill stretch: 8 pages of 128, Zmail's
/// `BACKFILL_PAGES_PER_RUN` times Bridge's `MetadataPageSize`.
pub const PROTON_STRETCH_MESSAGES: usize = 1_024;

/// People the account corresponds with; a few of them send most of the
/// mail.
const CONTACTS: u64 = 4_000;
/// Senders of newsletters, receipts and notifications.
const BULK_SENDERS: u64 = 20_000;

const FOLDERS: &[(&str, Option<&str>, u64)] = &[
    ("Archive", Some("archive"), 45),
    ("INBOX", Some("inbox"), 33),
    ("Sent", Some("sent"), 12),
    ("Trash", Some("trash"), 5),
    ("Spam", Some("junk"), 5),
];
const LABELS: &[&str] = &[
    "Labels/Family",
    "Labels/Receipts",
    "Labels/Travel",
    "Labels/Work",
    "Labels/Later",
    "Labels/Projects",
];
const ALL_MAIL: &str = "All Mail";

/// A fixed mix of `value` (SplitMix64's finaliser), so a message depends
/// only on its position.
fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A contact, skewed so low numbers come up far more often.
fn contact(hash: u64) -> String {
    let a = hash % CONTACTS;
    let b = (hash >> 20) % CONTACTS;
    let c = (hash >> 40) % CONTACTS;
    let index = a * b / CONTACTS * c / CONTACTS;
    let first = super::FIRST_NAMES[(index % super::FIRST_NAMES.len() as u64) as usize];
    let last = super::LAST_NAMES[(index / 16 % super::LAST_NAMES.len() as u64) as usize];
    format!(
        "{first} {last} <{}.{}{index}@{}>",
        first.to_lowercase(),
        last.to_lowercase(),
        super::DOMAINS[(index % super::DOMAINS.len() as u64) as usize]
    )
}

fn bulk_sender(hash: u64) -> String {
    let index = hash % BULK_SENDERS;
    format!("Shop {index} News <news@shop{index}.example>")
}

/// The conversation a message belongs to: positions come in runs of
/// seven, and the first few of some runs share a conversation, so some
/// conversations straddle two stretches.
fn conversation(position: usize) -> String {
    let run = position / 7;
    let together = match mix(run as u64 ^ 0x51ed) % 10 {
        0..=5 => 1,
        6 | 7 => 2,
        8 => 4,
        _ => 6,
    };
    if position % 7 < together {
        format!("conversation-{run}")
    } else {
        format!("conversation-{run}-{}", position % 7)
    }
}

struct Placed {
    message: MessageSnapshot,
    conversation: String,
    mailboxes: Vec<&'static str>,
}

fn place(position: usize, own_address: &str) -> Placed {
    let hash = mix(position as u64);
    let mut roll = hash % 100;
    let mut folder = FOLDERS[0].0;
    for (name, _, share) in FOLDERS {
        if roll < *share {
            folder = name;
            break;
        }
        roll -= share;
    }
    let mut mailboxes = vec![ALL_MAIL, folder];
    let label = mix(hash ^ 0x1abe1);
    if label.is_multiple_of(4) {
        mailboxes.push(LABELS[(label >> 8) as usize % LABELS.len()]);
    }

    let (sender, recipients) = if folder == "Sent" {
        let count = 1 + mix(hash ^ 0x70) % 3;
        let to: Vec<String> = (0..count).map(|n| contact(mix(hash ^ (n + 1)))).collect();
        (format!("Private Desk <{own_address}>"), to.join(", "))
    } else {
        let sender = if mix(hash ^ 0xb01c) % 10 < 7 {
            contact(mix(hash ^ 0x5e4d))
        } else {
            bulk_sender(mix(hash ^ 0x5e4d))
        };
        let recipients = if mix(hash ^ 0xcc).is_multiple_of(20) {
            format!("{own_address}, {}", contact(mix(hash ^ 0xcc01)))
        } else {
            own_address.to_owned()
        };
        (sender, recipients)
    };
    // Newest first: each position is about 47 s older than the one before.
    let at = 1_790_812_800_i64 - position as i64 * 47;
    let topic = [
        super::SUBJECT_WORDS[(hash >> 8) as usize % super::SUBJECT_WORDS.len()],
        super::SUBJECT_WORDS[(hash >> 24) as usize % super::SUBJECT_WORDS.len()],
        super::SUBJECT_WORDS[(hash >> 40) as usize % super::SUBJECT_WORDS.len()],
    ]
    .join(" ");
    Placed {
        message: MessageSnapshot {
            remote_id: format!("proton-message-{position}"),
            identity_email: own_address.to_owned(),
            sender,
            reply_to: None,
            recipients,
            subject: format!("{topic} {position}"),
            body_preview: String::new(),
            body_text: None,
            sanitized_html: None,
            raw_html: None,
            has_remote_content: false,
            is_read: mix(hash ^ 0x4ead) % 100 < 85,
            is_flagged: mix(hash ^ 0xf1a6) % 100 < 3,
            received_at: chrono::DateTime::from_timestamp(at, 0)
                .unwrap_or_default()
                .to_rfc3339(),
            attachments: Vec::new(),
            headers: MessageHeaders {
                captured: false,
                message_id: Some(format!("proton-message-{position}@protonmail.internalid")),
                in_reply_to: Vec::new(),
                references: Vec::new(),
                cc: String::new(),
                delivered_to: vec![own_address.to_owned()],
            },
        },
        conversation: conversation(position),
        mailboxes,
    }
}

/// The messages at `positions` (0 is the account's newest message) in
/// their mailboxes, as one stretch of a Proton account's history.
/// `own_address` is the account's address: mail in Sent is from it, the
/// rest is addressed to it.
pub fn proton_mirror_stretch(
    own_address: &str,
    positions: std::ops::Range<usize>,
) -> AccountSyncSnapshot {
    let mut mailboxes: BTreeMap<&'static str, BTreeMap<String, Vec<MessageSnapshot>>> =
        BTreeMap::new();
    for position in positions.clone() {
        let placed = place(position, own_address);
        for mailbox in placed.mailboxes {
            mailboxes
                .entry(mailbox)
                .or_default()
                .entry(placed.conversation.clone())
                .or_default()
                .push(placed.message.clone());
        }
    }
    let mailboxes = mailboxes
        .into_iter()
        .map(|(name, threads)| MailboxSnapshot {
            name: name.to_owned(),
            role: FOLDERS
                .iter()
                .find(|(folder, _, _)| *folder == name)
                .and_then(|(_, role, _)| role.map(str::to_owned)),
            delimiter: Some("/".to_owned()),
            parent: name.split_once('/').map(|(parent, _)| parent.to_owned()),
            threads: threads
                .into_iter()
                .map(|(conversation, messages)| thread(conversation, messages))
                .collect(),
        })
        .collect();
    AccountSyncSnapshot {
        checkpoint: format!("listed-{}", positions.end),
        detail: format!("Fetched {} messages", positions.end),
        identities: None,
        mailboxes,
    }
}

/// The same stretch as the delta a relist writes after a refresh request:
/// every listed message, each in all of its places.
pub fn proton_mirror_relist(own_address: &str, positions: std::ops::Range<usize>) -> MailDelta {
    MailDelta {
        message_ids: positions
            .clone()
            .map(|position| format!("proton-message-{position}"))
            .collect(),
        mailboxes: proton_mirror_stretch(own_address, positions).mailboxes,
        ..MailDelta::default()
    }
}

/// A conversation as the Proton adapter builds one: its newest message
/// names it, and its senders are listed newest first.
fn thread(conversation: String, mut messages: Vec<MessageSnapshot>) -> ThreadSnapshot {
    messages.sort_by(|left, right| left.received_at.cmp(&right.received_at));
    let mut participants: Vec<&str> = Vec::new();
    for message in messages.iter().rev() {
        if !participants.contains(&message.sender.as_str()) {
            participants.push(&message.sender);
        }
    }
    let newest = messages.last().expect("a conversation has a message");
    ThreadSnapshot {
        remote_thread_id: conversation,
        subject: newest.subject.clone(),
        participants: participants.join(", "),
        preview: String::new(),
        is_flagged: messages.iter().any(|message| message.is_flagged),
        has_attachments: false,
        unread_count: messages.iter().filter(|message| !message.is_read).count(),
        latest_message_at: newest.received_at.clone(),
        messages,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn a_stretch_is_the_same_however_it_is_cut() {
        let whole = proton_mirror_stretch("me@zmail.test", 0..2_048);
        let again = proton_mirror_stretch("me@zmail.test", 0..2_048);
        assert_eq!(whole, again);

        let all_mail = |snapshot: &AccountSyncSnapshot| -> Vec<MessageSnapshot> {
            let mut messages: Vec<MessageSnapshot> = snapshot
                .mailboxes
                .iter()
                .find(|mailbox| mailbox.name == ALL_MAIL)
                .expect("All Mail")
                .threads
                .iter()
                .flat_map(|thread| thread.messages.clone())
                .collect();
            messages.sort_by(|left, right| left.remote_id.cmp(&right.remote_id));
            messages
        };
        let mut halves = all_mail(&proton_mirror_stretch("me@zmail.test", 0..1_024));
        halves.extend(all_mail(&proton_mirror_stretch(
            "me@zmail.test",
            1_024..2_048,
        )));
        halves.sort_by(|left, right| left.remote_id.cmp(&right.remote_id));
        assert_eq!(all_mail(&whole), halves);
    }

    #[test]
    fn every_message_is_in_all_mail_and_one_folder() {
        let snapshot = proton_mirror_stretch("me@zmail.test", 0..PROTON_STRETCH_MESSAGES);
        let ids = |name: &str| -> HashSet<String> {
            snapshot
                .mailboxes
                .iter()
                .filter(|mailbox| mailbox.name == name)
                .flat_map(|mailbox| &mailbox.threads)
                .flat_map(|thread| &thread.messages)
                .map(|message| message.remote_id.clone())
                .collect()
        };
        let all = ids(ALL_MAIL);
        assert_eq!(all.len(), PROTON_STRETCH_MESSAGES);
        let mut in_folders = 0;
        for (folder, _, _) in FOLDERS {
            let folder = ids(folder);
            assert!(folder.is_subset(&all));
            in_folders += folder.len();
        }
        assert_eq!(in_folders, PROTON_STRETCH_MESSAGES);
        let senders: HashSet<&str> = snapshot
            .mailboxes
            .iter()
            .flat_map(|mailbox| &mailbox.threads)
            .flat_map(|thread| &thread.messages)
            .map(|message| message.sender.as_str())
            .collect();
        assert!(senders.len() > 200, "{} senders", senders.len());
    }
}
