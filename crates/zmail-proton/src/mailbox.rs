// Ported from proton-bridge `internal/services/imapservice/helpers.go`
// (`WantLabel`), `internal/services/imapservice/imap_updates.go`
// (`GetMailboxName`, `newSystemMailboxCreatedUpdate`) and the
// `folderPrefix`/`labelPrefix` constants in
// `internal/services/imapservice/connector.go` at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. IMAP mailbox
// attributes become Zmail mailbox roles; Starred is not a mailbox but the
// flagged state (as Bridge's \Flagged flag); All Mail keeps no role
// because Archive holds the archive role.

//! Proton labels as Zmail mailboxes, named the way Bridge names them over
//! IMAP: system labels by name, folders under `Folders/` and labels under
//! `Labels/`.

use std::collections::HashMap;

use zmail_proton_api::{
    ALL_MAIL_LABEL, ALL_SCHEDULED_LABEL, ARCHIVE_LABEL, DRAFTS_LABEL, INBOX_LABEL, Label,
    LabelType, SENT_LABEL, SPAM_LABEL, STARRED_LABEL, TRASH_LABEL,
};

/// Bridge's parent mailbox for user folders.
pub(crate) const FOLDER_PREFIX: &str = "Folders";
/// Bridge's parent mailbox for user labels.
pub(crate) const LABEL_PREFIX: &str = "Labels";
/// The hierarchy delimiter Bridge uses.
pub(crate) const DELIMITER: &str = "/";

/// A Proton label mirrored as a mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtonMailbox {
    /// The label ID.
    pub(crate) label_id: String,
    /// Mailbox name, such as `INBOX`, `Folders/Work` or `Labels/Family`.
    pub(crate) name: String,
    /// Zmail mailbox role, for system labels that have one.
    pub(crate) role: Option<&'static str>,
    /// Parent mailbox name, one level up (like the IMAP adapter derives it).
    pub(crate) parent: Option<String>,
}

/// Whether Bridge shows a label as a mailbox (`WantLabel`): user folders
/// and labels, and the useful system labels. Starred is wanted by Bridge
/// as a mailbox but becomes the flagged state here, so it is left out.
pub(crate) fn want_label(label: &Label) -> bool {
    match label.kind {
        LabelType::System => [
            INBOX_LABEL,
            TRASH_LABEL,
            SPAM_LABEL,
            ALL_MAIL_LABEL,
            ARCHIVE_LABEL,
            SENT_LABEL,
            DRAFTS_LABEL,
            ALL_SCHEDULED_LABEL,
        ]
        .contains(&label.id.as_str()),
        LabelType::ContactGroup => false,
        LabelType::Label | LabelType::Folder | LabelType::Other(_) => true,
    }
}

/// The mailbox for a label, if it has one.
pub(crate) fn mailbox_for(label: &Label) -> Option<ProtonMailbox> {
    if !want_label(label) || label.id == STARRED_LABEL {
        return None;
    }
    let name = mailbox_name(label).join(DELIMITER);
    let parent = name
        .rsplit_once(DELIMITER)
        .map(|(parent, _)| parent.to_owned())
        .filter(|parent| !parent.is_empty());
    Some(ProtonMailbox {
        label_id: label.id.clone(),
        role: system_role(&label.id),
        name,
        parent,
    })
}

/// Bridge's `GetMailboxName`, with its system-label renames (`INBOX`,
/// `Scheduled`).
fn mailbox_name(label: &Label) -> Vec<String> {
    let path = if label.path.is_empty() {
        vec![label.name.clone()]
    } else {
        label.path.clone()
    };
    match label.kind {
        LabelType::Folder => prefixed(FOLDER_PREFIX, path),
        LabelType::Label => prefixed(LABEL_PREFIX, path),
        LabelType::System => vec![match label.id.as_str() {
            INBOX_LABEL => "INBOX".to_owned(),
            // The API calls it "All Scheduled".
            ALL_SCHEDULED_LABEL => "Scheduled".to_owned(),
            _ => label.name.clone(),
        }],
        LabelType::ContactGroup | LabelType::Other(_) => path,
    }
}

fn prefixed(prefix: &str, path: Vec<String>) -> Vec<String> {
    std::iter::once(prefix.to_owned()).chain(path).collect()
}

/// The Zmail role of a system label (Bridge's special-use attributes).
fn system_role(label_id: &str) -> Option<&'static str> {
    match label_id {
        INBOX_LABEL => Some("inbox"),
        DRAFTS_LABEL => Some("drafts"),
        SENT_LABEL => Some("sent"),
        ARCHIVE_LABEL => Some("archive"),
        SPAM_LABEL => Some("junk"),
        TRASH_LABEL => Some("trash"),
        _ => None,
    }
}

/// The mailboxes for the account's labels, system labels first, keyed by
/// label ID for message placement.
pub(crate) fn mailboxes_for(labels: &[Label]) -> (Vec<ProtonMailbox>, HashMap<String, usize>) {
    let mut mailboxes: Vec<ProtonMailbox> = labels.iter().filter_map(mailbox_for).collect();
    mailboxes.sort_by_key(|mailbox| (mailbox.role.is_none(), system_order(&mailbox.label_id)));
    let index = mailboxes
        .iter()
        .enumerate()
        .map(|(position, mailbox)| (mailbox.label_id.clone(), position))
        .collect();
    (mailboxes, index)
}

/// Keeps system mailboxes in Proton's order; user folders and labels keep
/// the API's order after them (the sort is stable).
fn system_order(label_id: &str) -> usize {
    [
        INBOX_LABEL,
        DRAFTS_LABEL,
        SENT_LABEL,
        ARCHIVE_LABEL,
        SPAM_LABEL,
        TRASH_LABEL,
        ALL_MAIL_LABEL,
        ALL_SCHEDULED_LABEL,
    ]
    .iter()
    .position(|id| *id == label_id)
    .unwrap_or(usize::MAX)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn label(id: &str, name: &str, path: &str, kind: i64) -> Label {
        let value = serde_json::json!({"ID": id, "Name": name, "Path": path, "Type": kind});
        serde_json::from_value(value).expect("label")
    }

    #[test]
    fn names_mailboxes_like_bridge() {
        let cases = [
            (
                label("0", "Inbox", "Inbox", 4),
                Some(("INBOX", Some("inbox"), None)),
            ),
            (
                label("3", "Trash", "Trash", 4),
                Some(("Trash", Some("trash"), None)),
            ),
            (
                label("4", "Spam", "Spam", 4),
                Some(("Spam", Some("junk"), None)),
            ),
            (
                label("5", "All Mail", "All Mail", 4),
                Some(("All Mail", None, None)),
            ),
            (
                label("6", "Archive", "Archive", 4),
                Some(("Archive", Some("archive"), None)),
            ),
            (
                label("7", "Sent", "Sent", 4),
                Some(("Sent", Some("sent"), None)),
            ),
            (
                label("8", "Drafts", "Drafts", 4),
                Some(("Drafts", Some("drafts"), None)),
            ),
            (
                label("12", "All Scheduled", "All Scheduled", 4),
                Some(("Scheduled", None, None)),
            ),
            (label("10", "Starred", "Starred", 4), None),
            (label("1", "All Drafts", "All Drafts", 4), None),
            (label("2", "All Sent", "All Sent", 4), None),
            (label("9", "Outbox", "Outbox", 4), None),
            (label("g", "Friends", "Friends", 2), None),
            (
                label("f", "Work", "Work", 3),
                Some(("Folders/Work", None, Some("Folders"))),
            ),
            (
                label("s", "Sub", "Work/Sub", 3),
                Some(("Folders/Work/Sub", None, Some("Folders/Work"))),
            ),
            (
                label("l", "Family", "Family", 1),
                Some(("Labels/Family", None, Some("Labels"))),
            ),
        ];
        for (label, expected) in cases {
            let mailbox = mailbox_for(&label);
            let actual = mailbox
                .as_ref()
                .map(|m| (m.name.as_str(), m.role, m.parent.as_deref()));
            assert_eq!(actual, expected, "{label:?}");
        }
    }

    #[test]
    fn orders_system_mailboxes_first() {
        let (mailboxes, index) = mailboxes_for(&[
            label("l", "Family", "Family", 1),
            label("5", "All Mail", "All Mail", 4),
            label("7", "Sent", "Sent", 4),
            label("0", "Inbox", "Inbox", 4),
            label("f", "Work", "Work", 3),
        ]);
        let names: Vec<&str> = mailboxes.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            ["INBOX", "Sent", "All Mail", "Labels/Family", "Folders/Work"]
        );
        assert_eq!(index["l"], 3);
    }
}
