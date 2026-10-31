//! What the screens list, read from the local store. Names and text from
//! mail and servers are filtered for the terminal here.

use std::collections::HashMap;

use anyhow::Result;
use zmail_core::{
    AccountSummary, DraftState, MailboxRole, MailboxSelection, MailboxSummary, MessageSummary,
    OutboxJobState, SearchHit, ThreadSummary, VirtualMailbox, participants::sender_display_name,
};
use zmail_security::terminal::sanitize_line;
use zmail_store::LocalStore;

/// A folder the Message Index can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// What the index lists.
    pub selection: MailboxSelection,
    /// The account, for one account's mailbox.
    pub account_id: Option<i64>,
    /// The name shown.
    pub name: String,
    /// How deep it is nested.
    pub depth: usize,
    /// Conversations with unread mail.
    pub unread: usize,
    /// Conversations.
    pub total: usize,
}

/// A row of the Folder List: accounts are collections, as Alpine shows
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderRow {
    /// A collection's title.
    Collection(String),
    /// A folder.
    Folder(Folder),
    /// The postponed messages or the Outbox, in the Local collection.
    Local(LocalFolder),
    /// Space between collections.
    Gap,
}

impl FolderRow {
    /// Whether the cursor can rest on it and Enter opens it.
    pub fn selectable(&self) -> bool {
        matches!(self, Self::Folder(_) | Self::Local(_))
    }
}

/// A folder of mail that is Zmail's, not a server's, as Alpine lists its
/// local folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFolder {
    /// Drafts being written.
    Postponed {
        /// How many.
        count: usize,
    },
    /// Messages not sent yet.
    Outbox {
        /// How many.
        count: usize,
        /// How many of them couldn't be sent.
        failed: usize,
    },
}

/// The Folder List: the unified folders when there are several accounts,
/// then each account's mailboxes, then the postponed messages and the
/// Outbox.
pub fn folder_rows(store: &LocalStore, accounts: &[AccountSummary]) -> Result<Vec<FolderRow>> {
    let mut rows = Vec::new();
    if accounts.len() > 1 {
        rows.push(FolderRow::Collection("All accounts".to_owned()));
        for mailbox in [
            VirtualMailbox::Unified(MailboxRole::Inbox),
            VirtualMailbox::Flagged,
        ] {
            let selection = MailboxSelection::Virtual(mailbox);
            let counts = store.selection_counts(&selection)?;
            rows.push(FolderRow::Folder(Folder {
                selection,
                account_id: None,
                name: mailbox.label().to_owned(),
                depth: 0,
                unread: counts.unread_threads,
                total: counts.total_threads,
            }));
        }
    }
    for account in accounts {
        if !rows.is_empty() {
            rows.push(FolderRow::Gap);
        }
        rows.push(FolderRow::Collection(account_title(account)));
        let mailboxes = store.list_mailboxes(account.id)?;
        let parents: HashMap<&str, Option<&str>> = mailboxes
            .iter()
            .map(|mailbox| (mailbox.name.as_str(), mailbox.parent.as_deref()))
            .collect();
        for mailbox in &mailboxes {
            rows.push(FolderRow::Folder(Folder {
                selection: MailboxSelection::Mailbox {
                    account_id: account.id,
                    mailbox_id: mailbox.id,
                },
                account_id: Some(account.id),
                name: sanitize_line(mailbox.leaf_name()),
                depth: depth(mailbox, &parents),
                unread: mailbox.unread_threads,
                total: mailbox.total_threads,
            }));
        }
    }
    if !accounts.is_empty() {
        let postponed = store
            .list_drafts(None)?
            .iter()
            .filter(|draft| draft.state == DraftState::Editing)
            .count();
        let jobs = store.list_outbox_jobs(None)?;
        let waiting = jobs
            .iter()
            .filter(|job| job.state != OutboxJobState::Sent)
            .count();
        let failed = jobs
            .iter()
            .filter(|job| job.state == OutboxJobState::Failed)
            .count();
        rows.extend([
            FolderRow::Gap,
            FolderRow::Collection("Local".to_owned()),
            FolderRow::Local(LocalFolder::Postponed { count: postponed }),
            FolderRow::Local(LocalFolder::Outbox {
                count: waiting,
                failed,
            }),
        ]);
    }
    Ok(rows)
}

/// `Work <me@work.example>`.
pub fn account_title(account: &AccountSummary) -> String {
    let name = sanitize_line(&account.display_name);
    let username = sanitize_line(&account.username);
    if username.is_empty() || username == name {
        name
    } else {
        format!("{name} <{username}>")
    }
}

/// How many parents a mailbox has: from its IMAP path, or by following
/// JMAP parents (at most 16, in case of a loop).
fn depth(mailbox: &MailboxSummary, parents: &HashMap<&str, Option<&str>>) -> usize {
    if let Some(delimiter) = mailbox.delimiter.as_deref().filter(|d| !d.is_empty()) {
        return mailbox.name.matches(delimiter).count();
    }
    let mut depth = 0;
    let mut parent = mailbox.parent.as_deref();
    while let Some(name) = parent
        && depth < 16
    {
        depth += 1;
        parent = parents.get(name).copied().flatten();
    }
    depth
}

/// A row of the Message Index: a conversation, or one message when the
/// index lists messages singly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// The conversation.
    pub thread_id: i64,
    /// The message, when the row is one message.
    pub message_id: Option<i64>,
    /// The account it belongs to.
    pub account_id: i64,
    /// Its subject, filtered.
    pub subject: String,
    /// The sender's name, filtered.
    pub sender: String,
    /// When it arrived (the newest message's date for a conversation).
    pub date: String,
    /// Messages in the conversation.
    pub count: usize,
    /// Whether anything in it is unread.
    pub unread: bool,
    /// Whether anything in it is flagged.
    pub flagged: bool,
    /// Whether it has attachments.
    pub attachments: bool,
    /// Found only by the server search.
    pub from_server: bool,
}

impl IndexEntry {
    /// A conversation row.
    pub fn from_thread(thread: &ThreadSummary) -> Self {
        Self {
            thread_id: thread.id,
            message_id: None,
            account_id: thread.account_id,
            subject: sanitize_line(&thread.subject),
            sender: sender_name(&thread.latest_sender),
            date: thread.latest_message_at.clone(),
            count: thread.message_count,
            unread: thread.unread_count > 0,
            flagged: thread.is_flagged,
            attachments: thread.has_attachments,
            from_server: false,
        }
    }

    /// A search result row.
    pub fn from_hit(hit: &SearchHit) -> Self {
        Self {
            thread_id: hit.thread_id,
            message_id: None,
            account_id: hit.account_id,
            subject: sanitize_line(&hit.subject),
            sender: sender_name(&hit.latest_sender),
            date: hit.latest_message_at.clone(),
            count: 0,
            unread: hit.unread_count > 0,
            flagged: hit.is_flagged,
            attachments: hit.has_attachments,
            from_server: hit.from_server,
        }
    }

    /// A single message row.
    pub fn from_message(message: &MessageSummary, account_id: i64) -> Self {
        Self {
            thread_id: message.thread_id,
            message_id: Some(message.id),
            account_id,
            subject: sanitize_line(&message.subject),
            sender: sender_name(&message.sender),
            date: message.received_at.clone(),
            count: 1,
            unread: !message.is_read,
            flagged: message.is_flagged,
            attachments: message.has_attachments,
            from_server: false,
        }
    }
}

/// The sender's display name, filtered.
fn sender_name(sender: &str) -> String {
    sanitize_line(&sender_display_name(sender))
}

/// The index rows for `threads`: one per conversation, or one per message,
/// newest first.
pub fn index_entries(
    store: &LocalStore,
    threads: &[ThreadSummary],
    grouped: bool,
) -> Result<Vec<IndexEntry>> {
    if grouped {
        return Ok(threads.iter().map(IndexEntry::from_thread).collect());
    }
    let accounts: HashMap<i64, i64> = threads
        .iter()
        .map(|thread| (thread.id, thread.account_id))
        .collect();
    let ids = threads.iter().map(|thread| thread.id).collect::<Vec<_>>();
    Ok(store
        .thread_message_summaries(&ids)?
        .iter()
        .map(|message| {
            IndexEntry::from_message(
                message,
                accounts
                    .get(&message.thread_id)
                    .copied()
                    .unwrap_or_default(),
            )
        })
        .collect())
}
