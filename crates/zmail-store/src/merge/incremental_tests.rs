//! Partial writes keep the mailbox counters and the address history up
//! to date from what they add (#478). These tests hold that to two
//! things: the result is what counting every thread and rebuilding the
//! history after every write gave, and the work one write does no longer
//! grows with the mirror.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use rusqlite::params;
use zmail_core::{
    AccountSyncSnapshot, MailDelta, MailboxRename, MailboxSnapshot, MessageSnapshot, NewIdentity,
    ProtocolKind, ThreadSnapshot,
};
use zmail_test_support::{PROTON_STRETCH_MESSAGES, proton_mirror_relist, proton_mirror_stretch};

use crate::{
    LocalStore,
    address_history::{ALWAYS_REBUILD, REBUILDS, rebuild_account_address_history},
};

/// The demo Proton account's addresses.
const OWN: &str = "private1@proton.zmail.dev";
const ALIAS: &str = "alias1@proton.zmail.dev";

const MAILBOXES: &[(&str, Option<&str>)] = &[
    ("All Mail", None),
    ("INBOX", Some("inbox")),
    ("Archive", Some("archive")),
    ("Sent", Some("sent")),
    ("Spam", Some("junk")),
    ("Labels/Later", None),
];

/// The same people under different display names, and without one.
const PEOPLE: &[&str] = &[
    "Ann <ann@example.com>",
    "Ann Archer <ann@example.com>",
    "ann@example.com",
    "Bea <bea@example.com>",
    "Bea Brook <BEA@example.com>",
    "Cy <cy@example.org>",
    "cy@example.org",
    "Dee Dale <dee@example.org>",
    "Eve <eve@example.net>",
];

struct Random(u64);

impl Random {
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % bound.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// A message as the server has it.
#[derive(Clone)]
struct ServerMessage {
    conversation: String,
    sender: String,
    recipients: String,
    received_at: String,
    read: bool,
    places: BTreeSet<&'static str>,
}

impl ServerMessage {
    fn random(random: &mut Random, number: usize) -> Self {
        let outgoing = random.chance(25);
        let person = |random: &mut Random| PEOPLE[random.below(PEOPLE.len())].to_owned();
        let (sender, recipients) = if outgoing {
            let own = if random.chance(80) { OWN } else { ALIAS };
            let mut to = person(random);
            if random.chance(30) {
                to = format!("{to}, {}", person(random));
            }
            (format!("Private Desk <{own}>"), to)
        } else {
            (person(random), OWN.to_owned())
        };
        let mut places = BTreeSet::from(["All Mail"]);
        places.insert(match random.below(10) {
            0 => "Spam",
            1 | 2 => "Sent",
            3..=5 => "Archive",
            _ => "INBOX",
        });
        if random.chance(25) {
            places.insert("Labels/Later");
        }
        Self {
            conversation: format!("c{}", number / 3),
            sender,
            recipients,
            // Few distinct times, so display names tie on them.
            received_at: format!("2026-10-{:02}T12:00:00+00:00", 1 + random.below(20)),
            read: random.chance(70),
            places,
        }
    }

    fn snapshot(&self, id: &str) -> MessageSnapshot {
        MessageSnapshot {
            remote_id: id.to_owned(),
            identity_email: OWN.to_owned(),
            sender: self.sender.clone(),
            reply_to: None,
            recipients: self.recipients.clone(),
            subject: format!("Subject {}", self.conversation),
            body_preview: String::new(),
            body_text: None,
            sanitized_html: None,
            raw_html: None,
            has_remote_content: false,
            is_read: self.read,
            is_flagged: false,
            received_at: self.received_at.clone(),
            attachments: Vec::new(),
            headers: Default::default(),
        }
    }
}

/// The mailboxes holding `messages`, each in all of its places, as the
/// Proton adapter builds them.
fn mailboxes_of<'a>(
    messages: impl IntoIterator<Item = (&'a String, &'a ServerMessage)>,
) -> Vec<MailboxSnapshot> {
    let mut placed: BTreeMap<&str, BTreeMap<&str, Vec<MessageSnapshot>>> = BTreeMap::new();
    for (id, message) in messages {
        for place in &message.places {
            placed
                .entry(place)
                .or_default()
                .entry(&message.conversation)
                .or_default()
                .push(message.snapshot(id));
        }
    }
    placed
        .into_iter()
        .map(|(name, threads)| MailboxSnapshot {
            name: name.to_owned(),
            role: MAILBOXES
                .iter()
                .find(|(mailbox, _)| *mailbox == name)
                .and_then(|(_, role)| role.map(str::to_owned)),
            threads: threads
                .into_iter()
                .map(|(conversation, mut messages)| {
                    messages.sort_by(|left, right| {
                        left.received_at
                            .cmp(&right.received_at)
                            .then_with(|| left.remote_id.cmp(&right.remote_id))
                    });
                    let newest = messages.last().expect("a message");
                    let mut participants: Vec<&str> = Vec::new();
                    for message in messages.iter().rev() {
                        if !participants.contains(&message.sender.as_str()) {
                            participants.push(&message.sender);
                        }
                    }
                    ThreadSnapshot {
                        remote_thread_id: conversation.to_owned(),
                        subject: newest.subject.clone(),
                        participants: participants.join(", "),
                        preview: String::new(),
                        is_flagged: false,
                        has_attachments: false,
                        unread_count: messages.iter().filter(|message| !message.is_read).count(),
                        latest_message_at: newest.received_at.clone(),
                        messages,
                    }
                })
                .collect(),
            ..MailboxSnapshot::default()
        })
        .collect()
}

fn snapshot(mailboxes: Vec<MailboxSnapshot>) -> AccountSyncSnapshot {
    AccountSyncSnapshot {
        checkpoint: "checkpoint".to_owned(),
        detail: "detail".to_owned(),
        identities: None,
        mailboxes,
    }
}

/// One write to the store, as a sync or the user makes it.
enum Write {
    Merge(AccountSyncSnapshot),
    Delta(MailDelta),
    Replace(AccountSyncSnapshot),
    /// The user deletes a conversation.
    Delete(&'static str, String),
    /// The user moves a conversation to another mailbox.
    Move(&'static str, String, &'static str),
    /// The user marks a conversation read or unread.
    Read(&'static str, String, bool),
}

/// A store with one Proton account. `counting` makes every partial write
/// count its mailboxes and rebuild the history, as before #478.
struct Mirror {
    store: LocalStore,
    account_id: i64,
    counting: bool,
    rebuilds: usize,
}

impl Mirror {
    fn new(counting: bool) -> Self {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Proton)
            .expect("account should be created");
        Self {
            store,
            account_id,
            counting,
            rebuilds: 0,
        }
    }

    fn thread_id(&self, mailbox: &str, conversation: &str) -> Option<i64> {
        let conn = self.store.connection().expect("connection");
        conn.query_row(
            "SELECT threads.id FROM threads JOIN mailboxes ON mailboxes.id = threads.mailbox_id \
             WHERE mailboxes.account_id = ?1 AND mailboxes.name = ?2 \
               AND threads.remote_thread_id = ?3 ORDER BY threads.id LIMIT 1",
            params![self.account_id, mailbox, conversation],
            |row| row.get(0),
        )
        .ok()
    }

    fn mailbox_id(&self, mailbox: &str) -> Option<i64> {
        self.store
            .list_mailboxes(self.account_id)
            .expect("mailboxes")
            .into_iter()
            .find(|stored| stored.name == mailbox)
            .map(|stored| stored.id)
    }

    fn apply(&mut self, write: &Write) {
        ALWAYS_REBUILD.with(|always| always.set(self.counting));
        let before = REBUILDS.with(std::cell::Cell::get);
        match write {
            Write::Merge(snapshot) => self
                .store
                .merge_account_snapshot(self.account_id, snapshot)
                .expect("merge"),
            Write::Delta(delta) => {
                self.store
                    .apply_mail_delta(self.account_id, delta)
                    .expect("delta");
            }
            Write::Replace(snapshot) => self
                .store
                .replace_account_snapshot(self.account_id, snapshot)
                .expect("replace"),
            Write::Delete(mailbox, conversation) => {
                if let Some(thread_id) = self.thread_id(mailbox, conversation) {
                    self.store.delete_thread(thread_id).expect("delete");
                }
            }
            Write::Move(mailbox, conversation, to) => {
                if let (Some(thread_id), Some(to)) =
                    (self.thread_id(mailbox, conversation), self.mailbox_id(to))
                {
                    self.store.move_thread(thread_id, to).expect("move");
                }
            }
            Write::Read(mailbox, conversation, read) => {
                if let Some(thread_id) = self.thread_id(mailbox, conversation) {
                    self.store.set_thread_read(thread_id, *read).expect("read");
                }
            }
        }
        self.rebuilds += REBUILDS.with(std::cell::Cell::get) - before;
        ALWAYS_REBUILD.with(|always| always.set(false));
    }

    /// Every address history row of the account.
    fn history(&self) -> Vec<String> {
        let conn = self.store.connection().expect("connection");
        let mut statement = conn
            .prepare(
                "SELECT address || '|' || display_name || '|' || sent_count || '|' \
                 || received_count || '|' || COALESCE(mirror_last_used_at, '-') || '|' \
                 || COALESCE(mirror_named_at, '-') || '|' || outbox_sent_count \
                 FROM address_history WHERE account_id = ?1 ORDER BY address",
            )
            .expect("statement");
        statement
            .query_map(params![self.account_id], |row| row.get(0))
            .expect("rows")
            .collect::<rusqlite::Result<Vec<String>>>()
            .expect("history")
    }

    /// Each mailbox's stored counters.
    fn counters(&self) -> Vec<(String, usize, usize)> {
        let mut counters: Vec<(String, usize, usize)> = self
            .store
            .list_mailboxes(self.account_id)
            .expect("mailboxes")
            .into_iter()
            .map(|mailbox| (mailbox.name, mailbox.total_threads, mailbox.unread_threads))
            .collect();
        counters.sort();
        counters
    }

    /// Each mailbox's threads, counted.
    fn counted(&self) -> Vec<(String, usize, usize)> {
        let conn = self.store.connection().expect("connection");
        let mut statement = conn
            .prepare(
                "SELECT name, \
                   (SELECT COUNT(*) FROM threads WHERE mailbox_id = mailboxes.id), \
                   (SELECT COUNT(*) FROM threads WHERE mailbox_id = mailboxes.id \
                      AND unread_count > 0) \
                 FROM mailboxes WHERE account_id = ?1 ORDER BY name",
            )
            .expect("statement");
        statement
            .query_map(params![self.account_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .expect("rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("counts")
    }

    /// The history after rebuilding it from the mirror.
    fn rebuilt_history(&self) -> Vec<String> {
        {
            let mut conn = self.store.connection().expect("connection");
            let transaction = conn.transaction().expect("transaction");
            rebuild_account_address_history(&transaction, self.account_id).expect("rebuild");
            transaction.commit().expect("commit");
        }
        self.history()
    }
}

/// The next write of a random session: a backfill stretch, an event
/// batch (new mail, moves, deletions, read changes, a message back under
/// a new id, a changed sender, mailbox and address changes), a full
/// resync, or something the user does.
fn next_write(
    random: &mut Random,
    server: &mut BTreeMap<String, ServerMessage>,
    listed: &mut BTreeSet<String>,
    next_number: &mut usize,
) -> Write {
    let conversation = |random: &mut Random| format!("c{}", random.below(12));
    let mailbox = |random: &mut Random| MAILBOXES[random.below(MAILBOXES.len())].0;
    match random.below(100) {
        // A stretch of mail the mirror does not hold yet.
        0..=34 => {
            let mut stretch = Vec::new();
            for _ in 0..1 + random.below(6) {
                let id = format!("m{next_number}");
                server.insert(id.clone(), ServerMessage::random(random, *next_number % 36));
                listed.insert(id.clone());
                stretch.push(id);
                *next_number += 1;
            }
            Write::Merge(snapshot(mailboxes_of(
                server.iter().filter(|(id, _)| stretch.contains(id)),
            )))
        }
        // A stretch listed again, some of it read since.
        35..=44 => {
            let again: Vec<String> = listed
                .iter()
                .filter(|_| random.chance(30))
                .cloned()
                .collect();
            for id in &again {
                if let Some(message) = server.get_mut(id)
                    && random.chance(40)
                {
                    message.read = !message.read;
                }
            }
            Write::Merge(snapshot(mailboxes_of(
                server.iter().filter(|(id, _)| again.contains(id)),
            )))
        }
        // An event batch.
        45..=74 => {
            let mut touched = BTreeSet::new();
            let mut delta = MailDelta::default();
            for _ in 0..1 + random.below(4) {
                let known: Vec<String> = listed.iter().cloned().collect();
                let pick = (!known.is_empty()).then(|| known[random.below(known.len())].clone());
                match (random.below(8), pick) {
                    // New mail.
                    (0..=2, _) | (_, None) => {
                        let id = format!("m{next_number}");
                        server.insert(id.clone(), ServerMessage::random(random, *next_number % 36));
                        listed.insert(id.clone());
                        touched.insert(id);
                        *next_number += 1;
                    }
                    // Moved to other mailboxes.
                    (3, Some(id)) => {
                        if let Some(message) = server.get_mut(&id) {
                            message.places = ServerMessage::random(random, 0).places;
                        }
                        touched.insert(id);
                    }
                    // Deleted.
                    (4, Some(id)) => {
                        server.remove(&id);
                        listed.remove(&id);
                        touched.insert(id);
                    }
                    // Read or unread.
                    (5, Some(id)) => {
                        if let Some(message) = server.get_mut(&id) {
                            message.read = !message.read;
                        }
                        touched.insert(id);
                    }
                    // Back under a new remote id, as a move is on some
                    // servers.
                    (6, Some(id)) => {
                        if let Some(message) = server.remove(&id) {
                            let twin = format!("m{next_number}");
                            *next_number += 1;
                            server.insert(twin.clone(), message);
                            listed.insert(twin.clone());
                            touched.insert(twin);
                        }
                        listed.remove(&id);
                        touched.insert(id);
                    }
                    // Another sender.
                    (_, Some(id)) => {
                        if let Some(message) = server.get_mut(&id) {
                            message.sender = PEOPLE[random.below(PEOPLE.len())].to_owned();
                        }
                        touched.insert(id);
                    }
                }
            }
            if random.chance(6) {
                delta.removed.push("Labels/Later".to_owned());
                for message in server.values_mut() {
                    message.places.remove("Labels/Later");
                }
            }
            if random.chance(4) {
                delta.renamed.push(MailboxRename {
                    from: "Archive".to_owned(),
                    to: "Archive".to_owned(),
                    parent: None,
                });
            }
            if random.chance(8) {
                let mut identities = vec![NewIdentity {
                    display_name: "Private Desk".to_owned(),
                    email_address: OWN.to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }];
                // The alias comes and goes, and with it whether mail from
                // it counts as sent.
                if random.chance(50) {
                    identities.push(NewIdentity {
                        display_name: "Private Alias".to_owned(),
                        email_address: ALIAS.to_owned(),
                        signature: String::new(),
                        is_primary: false,
                    });
                }
                delta.identities = Some(identities);
            }
            delta.mailboxes = mailboxes_of(server.iter().filter(|(id, _)| touched.contains(*id)));
            delta.message_ids = touched.into_iter().collect();
            Write::Delta(delta)
        }
        // A full resync of what has been listed.
        75..=79 => Write::Replace(snapshot(mailboxes_of(
            server.iter().filter(|(id, _)| listed.contains(*id)),
        ))),
        80..=86 => Write::Delete(mailbox(random), conversation(random)),
        87..=93 => Write::Move(
            mailbox(random),
            conversation(random),
            if random.chance(50) { "Spam" } else { "Archive" },
        ),
        _ => Write::Read(mailbox(random), conversation(random), random.chance(50)),
    }
}

/// Whatever is written, in whatever order, the counters and the history
/// are what counting and rebuilding after every write give, and what a
/// rebuild from the mirror gives.
#[test]
fn any_sequence_of_writes_matches_counting_and_rebuilding_every_time() {
    let mut saved_rebuilds = 0;
    for seed in 0..8 {
        let mut random = Random(seed * 7_919 + 1);
        let mut incremental = Mirror::new(false);
        let mut counting = Mirror::new(true);
        let mut server = BTreeMap::new();
        let mut listed = BTreeSet::new();
        let mut next_number = 0;
        for step in 0..80 {
            let write = next_write(&mut random, &mut server, &mut listed, &mut next_number);
            incremental.apply(&write);
            counting.apply(&write);
            assert_eq!(
                incremental.history(),
                counting.history(),
                "address history, seed {seed} step {step}"
            );
            assert_eq!(
                incremental.counters(),
                counting.counters(),
                "counters, seed {seed} step {step}"
            );
            assert_eq!(
                incremental.counters(),
                incremental.counted(),
                "counted threads, seed {seed} step {step}"
            );
        }
        // Local changes leave the history stale until a sync changes mail;
        // one more stretch brings both up to date.
        let last = Write::Merge(snapshot(mailboxes_of([(
            &"last".to_owned(),
            &ServerMessage::random(&mut random, 99),
        )])));
        incremental.apply(&last);
        counting.apply(&last);
        let history = incremental.history();
        assert_eq!(history, counting.history(), "seed {seed}");
        assert_eq!(history, incremental.rebuilt_history(), "seed {seed}");
        assert!(!history.is_empty());
        assert!(incremental.rebuilds < counting.rebuilds);
        saved_rebuilds += counting.rebuilds - incremental.rebuilds;
    }
    eprintln!("{saved_rebuilds} rebuilds saved");
    // The sessions are mostly additive, so most rebuilds are not needed.
    assert!(saved_rebuilds > 100, "{saved_rebuilds} rebuilds saved");
}

fn message(id: &str, sender: &str, recipients: &str, at: &str) -> (String, ServerMessage) {
    (
        id.to_owned(),
        ServerMessage {
            conversation: format!("conversation-{id}"),
            sender: sender.to_owned(),
            recipients: recipients.to_owned(),
            received_at: at.to_owned(),
            read: true,
            places: BTreeSet::from(["All Mail", "INBOX"]),
        },
    )
}

fn stretch(messages: &[(String, ServerMessage)]) -> Write {
    Write::Merge(snapshot(mailboxes_of(
        messages.iter().map(|(id, message)| (id, message)),
    )))
}

#[test]
fn a_stretch_of_older_mail_adds_to_the_history_without_a_rebuild() {
    let mut mirror = Mirror::new(false);
    mirror.apply(&Write::Replace(snapshot(mailboxes_of(
        [message(
            "m2",
            "Ann Archer <ann@example.com>",
            OWN,
            "2026-10-02T00:00:00+00:00",
        )]
        .iter()
        .map(|(id, message)| (id, message)),
    ))));
    assert_eq!(mirror.rebuilds, 1, "a full snapshot rebuilds");

    // Older mail: Ann under an older name, and someone new.
    mirror.apply(&stretch(&[
        message(
            "m1",
            "Ann <ann@example.com>",
            OWN,
            "2026-09-01T00:00:00+00:00",
        ),
        message(
            "m0",
            "Bea <bea@example.com>",
            OWN,
            "2026-08-01T00:00:00+00:00",
        ),
    ]));
    assert_eq!(mirror.rebuilds, 1, "an additive stretch does not");
    // Each message is in All Mail and the Inbox, and counts in both.
    assert_eq!(
        mirror.history(),
        [
            "ann@example.com|Ann Archer|0|4|2026-10-02T00:00:00Z|2026-10-02T00:00:00Z|0",
            "bea@example.com|Bea|0|2|2026-08-01T00:00:00Z|2026-08-01T00:00:00Z|0",
        ]
    );
    assert_eq!(mirror.history(), mirror.rebuilt_history());

    // Merging the same stretch again changes nothing and counts nothing.
    mirror.apply(&stretch(&[message(
        "m1",
        "Ann <ann@example.com>",
        OWN,
        "2026-09-01T00:00:00+00:00",
    )]));
    assert_eq!(mirror.history()[0].split('|').nth(3), Some("4"));
}

#[test]
fn a_message_back_under_a_new_remote_id_is_not_counted_twice() {
    let mut mirror = Mirror::new(false);
    let original = message(
        "m1",
        "Ann <ann@example.com>",
        OWN,
        "2026-09-01T00:00:00+00:00",
    );
    mirror.apply(&stretch(std::slice::from_ref(&original)));
    assert_eq!(mirror.history()[0].split('|').nth(3), Some("2"));

    // The same message, moved on the server and listed under a new id.
    let mut twin = original.1.clone();
    twin.places = BTreeSet::from(["All Mail", "Archive"]);
    let twin = ("m1-moved".to_owned(), twin);
    let rebuilds = mirror.rebuilds;
    mirror.apply(&Write::Delta(MailDelta {
        mailboxes: mailboxes_of([(&twin.0, &twin.1)]),
        message_ids: vec!["m1".to_owned(), "m1-moved".to_owned()],
        ..MailDelta::default()
    }));
    assert_eq!(mirror.rebuilds, rebuilds + 1, "removed mail rebuilds");
    assert_eq!(
        mirror.history(),
        ["ann@example.com|Ann|0|2|2026-09-01T00:00:00Z|2026-09-01T00:00:00Z|0"]
    );
}

#[test]
fn what_the_user_removes_is_followed_by_the_next_sync_that_changes_mail() {
    let mut mirror = Mirror::new(false);
    mirror.apply(&stretch(&[
        message(
            "m1",
            "Ann <ann@example.com>",
            OWN,
            "2026-09-01T00:00:00+00:00",
        ),
        message(
            "m2",
            "Bea <bea@example.com>",
            OWN,
            "2026-09-02T00:00:00+00:00",
        ),
    ]));
    // A new account's history starts stale, so its first write rebuilt.
    assert_eq!(mirror.rebuilds, 1);

    // Deleted here from both of its mailboxes: Ann's mail is gone.
    mirror.apply(&Write::Delete("INBOX", "conversation-m1".to_owned()));
    mirror.apply(&Write::Delete("All Mail", "conversation-m1".to_owned()));
    assert_eq!(mirror.history().len(), 2, "not until a sync changes mail");

    mirror.apply(&stretch(&[message(
        "m3",
        "Cy <cy@example.org>",
        OWN,
        "2026-08-01T00:00:00+00:00",
    )]));
    assert_eq!(mirror.rebuilds, 2, "the stale history is rebuilt");
    let addresses: Vec<String> = mirror
        .history()
        .iter()
        .map(|row| row.split('|').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(addresses, ["bea@example.com", "cy@example.org"]);

    // The next stretch adds again.
    mirror.apply(&stretch(&[message(
        "m4",
        "Dee <dee@example.org>",
        OWN,
        "2026-07-01T00:00:00+00:00",
    )]));
    assert_eq!(mirror.rebuilds, 2);
    assert_eq!(mirror.history(), mirror.rebuilt_history());
}

#[test]
fn a_time_that_is_not_utc_seconds_is_left_to_the_rebuild() {
    let mut mirror = Mirror::new(false);
    mirror.apply(&stretch(&[message(
        "m0",
        "Bea <bea@example.com>",
        OWN,
        "2026-09-02T00:00:00+00:00",
    )]));
    mirror.apply(&Write::Delete("INBOX", "conversation-m0".to_owned()));
    mirror.apply(&Write::Delete("All Mail", "conversation-m0".to_owned()));
    mirror.apply(&stretch(&[message(
        "m1",
        "Ann <ann@example.com>",
        OWN,
        "2026-09-01T00:00:00+00:00",
    )]));
    let rebuilds = mirror.rebuilds;
    mirror.apply(&stretch(&[message(
        "m2",
        "Ann <ann@example.com>",
        OWN,
        "2026-09-01T02:00:00+02:00",
    )]));
    assert_eq!(mirror.rebuilds, rebuilds + 1);
    assert_eq!(
        mirror.history(),
        ["ann@example.com|Ann|0|4|2026-09-01T00:00:00Z|2026-09-01T00:00:00Z|0"]
    );
}

/// Runs `write` and returns the SQLite virtual machine steps the store's
/// connections on this thread ran for it: a measure of the work that
/// does not depend on the machine or on what else it is doing.
fn steps(store: &LocalStore, write: impl FnOnce()) -> u64 {
    let counter = Arc::new(AtomicU64::new(0));
    let install = |counter: Option<Arc<AtomicU64>>| {
        // A thread keeps up to two connections to a store; both count.
        let mut first = store.connection().expect("connection");
        let mut second = store.connection().expect("connection");
        for conn in [&mut first, &mut second] {
            let handler = counter.clone().map(|counter| {
                move || {
                    counter.fetch_add(1, Ordering::Relaxed);
                    false
                }
            });
            conn.progress_handler(1, handler).expect("progress handler");
        }
    };
    install(Some(counter.clone()));
    write();
    install(None);
    counter.load(Ordering::Relaxed)
}

/// A Proton-shaped mirror of `messages` messages and the steps it takes
/// to write the next stretch into it: merged as the backfill merges it,
/// written as a relist delta of mail the mirror does not hold, that delta
/// again, when it changes nothing, and merged while counting the
/// mailboxes and rebuilding the history as every stretch did before.
fn stretch_steps(messages: usize) -> [u64; 4] {
    let store = LocalStore::open_ephemeral().expect("store should open");
    let account_id = store
        .create_demo_account(ProtocolKind::Proton)
        .expect("account should be created");
    let mut held = 0;
    while held < messages {
        let end = messages.min(held + 4 * PROTON_STRETCH_MESSAGES);
        store
            .merge_account_snapshot(account_id, &proton_mirror_stretch(OWN, held..end))
            .expect("the mirror should grow");
        held = end;
    }

    let merged = steps(&store, || {
        store
            .merge_account_snapshot(
                account_id,
                &proton_mirror_stretch(OWN, held..held + PROTON_STRETCH_MESSAGES),
            )
            .expect("merge");
    });
    held += PROTON_STRETCH_MESSAGES;
    let delta = proton_mirror_relist(OWN, held..held + PROTON_STRETCH_MESSAGES);
    let relisted_new = steps(&store, || {
        store.apply_mail_delta(account_id, &delta).expect("delta");
    });
    let relisted_same = steps(&store, || {
        store.apply_mail_delta(account_id, &delta).expect("delta");
    });
    held += PROTON_STRETCH_MESSAGES;
    ALWAYS_REBUILD.with(|always| always.set(true));
    let counting = steps(&store, || {
        store
            .merge_account_snapshot(
                account_id,
                &proton_mirror_stretch(OWN, held..held + PROTON_STRETCH_MESSAGES),
            )
            .expect("merge");
    });
    ALWAYS_REBUILD.with(|always| always.set(false));
    [merged, relisted_new, relisted_same, counting]
}

/// The scaling guard of #478: writing a stretch into a mirror sixteen
/// times larger must not take much more work. Before the fix every
/// stretch read the whole mirror three times over (the mailbox's threads
/// to match against, its threads again to count them, and every message
/// to rebuild the address history), so the work grew with the mirror and
/// a backfill was quadratic.
#[test]
fn writing_a_stretch_costs_the_same_in_a_small_and_a_large_mirror() {
    const SMALL: usize = 2 * PROTON_STRETCH_MESSAGES;
    const LARGE: usize = 16 * SMALL;
    let small = stretch_steps(SMALL);
    let large = stretch_steps(LARGE);
    for (name, small, large) in [
        ("a merged stretch", small[0], large[0]),
        ("a relisted stretch of new mail", small[1], large[1]),
        (
            "a relisted stretch that changes nothing",
            small[2],
            large[2],
        ),
    ] {
        eprintln!("{name}: {small} steps into {SMALL} messages, {large} into {LARGE}");
        // Deeper indexes cost a little more; reading the mirror would cost
        // several times more.
        assert!(
            large * 4 < small * 5,
            "{name} took {small} steps into a mirror of {SMALL} messages \
             and {large} into one of {LARGE}"
        );
    }

    // The measure sees what it guards against: counting and rebuilding on
    // every stretch, as before, costs far more in the larger mirror.
    eprintln!(
        "counting and rebuilding: {} steps into {SMALL} messages, {} into {LARGE}",
        small[3], large[3]
    );
    assert!(large[3] > 2 * small[3]);
}
