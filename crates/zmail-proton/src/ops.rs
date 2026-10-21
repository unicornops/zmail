// Ported from proton-bridge `internal/services/imapservice/connector.go`
// (`MarkMessagesSeen`, `MarkMessagesFlagged`,
// `moveMessagesWithUnlabelCallOnFolders`,
// `removeMessagesFromMailboxWithUnlabelCallOnFolders` and
// `isAllMailOrScheduled` from `helpers.go`) at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4,
// https://github.com/ProtonMail/proton-bridge).
// Copyright (c) 2026 Proton AG. Proton Mail Bridge is free software under the
// GNU General Public License, version 3 or (at your option) any later
// version; this file is part of Zmail and is used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust for Zmail's
// queued mailbox operations, one message at a time, with mailboxes named
// as in `mailbox.rs`. Bridge's default (unlabel calls on folders enabled)
// is the behaviour ported. A destroy outside Trash and Drafts is a move to
// Trash followed by Bridge's expunge from Trash.

//! Writing queued mailbox operations back to Proton, with Bridge's rules.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use zmail_core::{OpApplyReport, PendingOp, PendingOpKind};
use zmail_proton_api::{
    ALL_DRAFTS_LABEL, ALL_MAIL_LABEL, ALL_SCHEDULED_LABEL, ALL_SENT_LABEL, Client, DRAFTS_LABEL,
    INBOX_LABEL, Label, LabelType, MessageFilter, SENT_LABEL, STARRED_LABEL, TRASH_LABEL,
};

use crate::{
    mailbox::{mailboxes_for, want_label},
    sync,
};

/// Applies the operations in order, reporting each one's outcome.
pub(crate) async fn apply(client: &Client, ops: &[PendingOp]) -> Result<OpApplyReport> {
    let mut report = OpApplyReport::default();
    if ops.is_empty() {
        return Ok(report);
    }
    let labels = client.get_labels(&sync::LABEL_TYPES).await?;
    let locations = Locations::new(&labels);
    for op in ops {
        match apply_one(client, &locations, op).await {
            Ok(()) => report.applied.push(op.id),
            Err(error) => report.failed.push((op.id, format!("{error:#}"))),
        }
    }
    Ok(report)
}

/// Mailbox names to labels.
struct Locations<'a> {
    by_name: HashMap<String, &'a Label>,
    by_id: HashMap<&'a str, &'a Label>,
}

impl<'a> Locations<'a> {
    fn new(labels: &'a [Label]) -> Self {
        let by_id: HashMap<&str, &Label> = labels
            .iter()
            .map(|label| (label.id.as_str(), label))
            .collect();
        let (mailboxes, _) = mailboxes_for(labels);
        let by_name = mailboxes
            .into_iter()
            .filter_map(|mailbox| {
                by_id
                    .get(mailbox.label_id.as_str())
                    .map(|label| (mailbox.name, *label))
            })
            .collect();
        Self { by_name, by_id }
    }

    fn label(&self, mailbox: &str) -> Result<&'a Label> {
        self.by_name
            .get(mailbox)
            .copied()
            .ok_or_else(|| anyhow!("no Proton label for mailbox {mailbox:?}"))
    }
}

async fn apply_one(client: &Client, locations: &Locations<'_>, op: &PendingOp) -> Result<()> {
    let ids = [op.remote_id.clone()];
    match op.kind {
        // `MarkMessagesSeen`.
        PendingOpKind::MarkRead => client.mark_messages_read(&ids).await?,
        PendingOpKind::MarkUnread => client.mark_messages_unread(&ids).await?,
        // `MarkMessagesFlagged`: starring is the Starred label.
        PendingOpKind::SetFlagged => client.label_messages(&ids, STARRED_LABEL).await?,
        PendingOpKind::ClearFlagged => client.unlabel_messages(&ids, STARRED_LABEL).await?,
        PendingOpKind::Move => {
            let target = op
                .target_mailbox
                .as_deref()
                .ok_or_else(|| anyhow!("a move without a target mailbox"))?;
            let from = locations.label(&op.source_mailbox)?;
            let to = locations.label(target)?;
            move_message(client, &ids, from, to).await?;
        }
        // Zmail queues a destroy when a message is deleted from Trash (or
        // where there is no Trash); a quick second delete can leave the
        // message's server location elsewhere. Outside Trash and Drafts the
        // message is first moved to Trash, then removed from it, so the
        // outcome is Bridge's expunge-from-Trash either way.
        PendingOpKind::Destroy => {
            let mut from = locations.label(&op.source_mailbox)?;
            if from.id != TRASH_LABEL && from.id != DRAFTS_LABEL {
                let trash = locations
                    .by_id
                    .get(TRASH_LABEL)
                    .copied()
                    .ok_or_else(|| anyhow!("the account has no Trash"))?;
                move_message(client, &ids, from, trash).await?;
                from = trash;
            }
            remove_message(client, locations, &ids, from).await?;
        }
    }
    Ok(())
}

/// `isAllMailOrScheduled`.
fn is_all_mail_or_scheduled(label_id: &str) -> bool {
    label_id == ALL_MAIL_LABEL || label_id == ALL_SCHEDULED_LABEL
}

/// `moveMessagesWithUnlabelCallOnFolders`: label the target, and take the
/// message out of the source when the source is a label or the target is a
/// folder or system location.
async fn move_message(client: &Client, ids: &[String], from: &Label, to: &Label) -> Result<()> {
    if move_not_allowed(&from.id, &to.id) {
        bail!(
            "Proton does not allow moving from {} to {}",
            from.name,
            to.name
        );
    }
    client.label_messages(ids, &to.id).await?;
    if expunges_old_location(from, to) {
        client.unlabel_messages(ids, &from.id).await?;
    }
    Ok(())
}

fn move_not_allowed(from: &str, to: &str) -> bool {
    (from == INBOX_LABEL && to == SENT_LABEL)
        || (from == SENT_LABEL && to == INBOX_LABEL)
        || is_all_mail_or_scheduled(from)
        || is_all_mail_or_scheduled(to)
}

fn expunges_old_location(from: &Label, to: &Label) -> bool {
    from.kind == LabelType::Label || matches!(to.kind, LabelType::Folder | LabelType::System)
}

/// `removeMessagesFromMailboxWithUnlabelCallOnFolders`: unlabel the
/// mailbox, and from Trash or Drafts delete permanently what is left in no
/// other location.
async fn remove_message(
    client: &Client,
    locations: &Locations<'_>,
    ids: &[String],
    from: &Label,
) -> Result<()> {
    if is_all_mail_or_scheduled(&from.id) {
        bail!("Proton does not allow removing messages from {}", from.name);
    }
    client.unlabel_messages(ids, &from.id).await?;
    if from.id != TRASH_LABEL && from.id != DRAFTS_LABEL {
        return Ok(());
    }
    let filter = MessageFilter {
        id: ids.to_vec(),
        ..MessageFilter::default()
    };
    let metadata = client
        .get_message_metadata_page(0, ids.len(), &filter)
        .await?;
    let deletable: Vec<String> = metadata
        .iter()
        .filter(|meta| {
            !meta
                .label_ids
                .iter()
                .any(|id| remains_in(locations.by_id.get(id.as_str()).copied(), id, &from.id))
        })
        .map(|meta| meta.id.clone())
        .collect();
    if !deletable.is_empty() {
        client.delete_messages(&deletable).await?;
    }
    Ok(())
}

/// Whether a label still holds the message after it left `source`
/// (Bridge's `remainingLabels`). Unknown labels do not count; Bridge looks
/// them up first and skips them when they no longer exist. Starred counts,
/// as in Bridge, where it is a mailbox.
fn remains_in(label: Option<&Label>, id: &str, source: &str) -> bool {
    let Some(label) = label else {
        return false;
    };
    (want_label(label) || id == STARRED_LABEL)
        && id != source
        && !(label.kind == LabelType::System
            && [
                ALL_DRAFTS_LABEL,
                ALL_MAIL_LABEL,
                ALL_SENT_LABEL,
                ALL_SCHEDULED_LABEL,
            ]
            .contains(&id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::tests::label;

    #[test]
    fn follows_bridges_move_rules() {
        assert!(move_not_allowed(INBOX_LABEL, SENT_LABEL));
        assert!(move_not_allowed(SENT_LABEL, INBOX_LABEL));
        assert!(move_not_allowed(ALL_MAIL_LABEL, TRASH_LABEL));
        assert!(move_not_allowed(INBOX_LABEL, ALL_SCHEDULED_LABEL));
        assert!(!move_not_allowed(INBOX_LABEL, TRASH_LABEL));

        let inbox = label("0", "Inbox", "Inbox", 4);
        let trash = label("3", "Trash", "Trash", 4);
        let work = label("f", "Work", "Work", 3);
        let family = label("l", "Family", "Family", 1);
        let friends = label("k", "Friends", "Friends", 1);
        // Into a folder or system location: the old location is left.
        assert!(expunges_old_location(&inbox, &trash));
        assert!(expunges_old_location(&inbox, &work));
        // Out of a label: the label is removed.
        assert!(expunges_old_location(&family, &friends));
        // From a folder to a label: the message stays in the folder too.
        assert!(!expunges_old_location(&work, &family));
    }

    #[test]
    fn deletes_from_trash_only_what_is_nowhere_else() {
        let labels = [
            label("0", "Inbox", "Inbox", 4),
            label("10", "Starred", "Starred", 4),
            label("3", "Trash", "Trash", 4),
            label("5", "All Mail", "All Mail", 4),
            label("1", "All Drafts", "All Drafts", 4),
            label("l", "Family", "Family", 1),
        ];
        let locations = Locations::new(&labels);
        let remaining = |ids: &[&str]| {
            ids.iter()
                .any(|id| remains_in(locations.by_id.get(id).copied(), id, TRASH_LABEL))
        };
        assert!(
            !remaining(&["3", "5", "1"]),
            "only trash and the All* labels"
        );
        assert!(remaining(&["3", "5", "l"]), "still labelled Family");
        assert!(remaining(&["3", "0"]), "still in the Inbox");
        assert!(
            remaining(&["3", "10"]),
            "starred messages stay, as in Bridge"
        );
        assert!(!remaining(&["3", "unknown"]), "unknown labels do not count");
        assert_eq!(
            locations.label("Labels/Family").map(|l| l.id.as_str()).ok(),
            Some("l")
        );
        assert!(locations.label("Nowhere").is_err());
    }
}
