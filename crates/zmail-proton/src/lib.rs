//! Proton Mail sync adapter for Zmail (issue #237, ADR 0004).
//!
//! The protocol client is `zmail-proton-api` (a go-proton-api port); this
//! crate follows Proton Mail Bridge's services on top of it and offers the
//! same entry points as `zmail-imap` and `zmail-jmap`, so `zmail-sync` can
//! dispatch to it. Nothing here is reachable from the UI until the local
//! store is encrypted at rest (#139); `zmail-sync` only builds it with its
//! `proton` feature, which `zmail-app` does not enable.

mod checkpoint;
mod events;
mod hydrate;
mod mailbox;
mod message;
mod ops;
mod outgoing;
mod send;
mod send_prefs;
mod session;
mod signin;
mod sync;

use std::{collections::HashMap, sync::Arc};

use anyhow::{Result, bail};
use zmail_core::{
    AccountSyncSnapshot, MailboxSnapshot, NewIdentity, OpApplyReport, OutgoingMessage, PendingOp,
    ProtocolKind, SyncTarget,
};
use zmail_proton_api::{Address, AddressStatus};

pub use checkpoint::ProtonCheckpoint;
pub use events::{EVENT_JITTER, EVENT_PERIOD, event_poll_delay, has_new_events};
pub use hydrate::fetch_message_bodies;
pub use session::{ProtonSession, ProtonSessions, delete_session, has_session, store_new_session};
pub use signin::{ProtonSignIn, SignInStep, SignedInAccount};
pub use sync::SyncPaging;
pub use zmail_proton_api::{API_HOST_URL, AppVersion, ClientConfig};

/// Loads the account's stored mirror. A sync calls it at most once, and
/// only when events changed messages.
pub type MirrorLoader<'a> = &'a (dyn Fn() -> Result<Arc<Vec<MailboxSnapshot>>> + Send + Sync);

/// Reports how far a sync has got, as a status line.
pub type Progress<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// What a sync does to the account's mirror.
#[derive(Debug)]
pub enum MirrorUpdate {
    /// Replace the mirror with this snapshot: a first (or full) sync, or
    /// the events since the checkpoint applied to the stored mirror.
    Replace {
        /// The account's mailboxes.
        snapshot: AccountSyncSnapshot,
        /// Messages it adds that are not new mail: older messages an event
        /// changed before the backfill listed them. They must not raise
        /// new-mail notifications.
        not_new: Vec<String>,
    },
    /// The events since the checkpoint changed nothing Zmail mirrors: only
    /// the checkpoint moves.
    Unchanged {
        /// The checkpoint to store.
        checkpoint: String,
        /// The status detail.
        detail: String,
    },
}

impl MirrorUpdate {
    /// The checkpoint to store once the update is written.
    pub fn checkpoint(&self) -> &str {
        match self {
            Self::Replace { snapshot, .. } => &snapshot.checkpoint,
            Self::Unchanged { checkpoint, .. } => checkpoint,
        }
    }

    /// The status detail.
    pub fn detail(&self) -> &str {
        match self {
            Self::Replace { snapshot, .. } => &snapshot.detail,
            Self::Unchanged { detail, .. } => detail,
        }
    }
}

/// Older mail listed by one backfill step.
#[derive(Debug)]
pub struct Backfill {
    /// The older messages in their mailboxes. Merge it into the mirror
    /// (it holds only this step's messages, so it must never replace the
    /// mirror); its checkpoint carries the advanced cursor.
    pub snapshot: AccountSyncSnapshot,
    /// Whether the account's whole history is now listed.
    pub complete: bool,
}

/// Syncs a Proton account's changes and returns what to write into the
/// store.
///
/// With a checkpoint from an earlier sync and a stored mirror, the
/// account's events since then are applied to the mirror (Bridge's event
/// loop): message changes update their mailboxes in place. A refresh
/// request from the server, or a label, address or user change, falls back
/// to a first sync.
///
/// A first sync takes the newest event ID first, so no change made during
/// it is missed, then mirrors the identities (combined address mode: one
/// account, all addresses), the labels as mailboxes and the metadata of the
/// newest messages only (`paging.first_sync_pages` pages), so the account
/// shows its recent mail within seconds. The checkpoint then carries a
/// cursor, and [`backfill_older_mail`] lists the older messages over the
/// following syncs. Bodies are fetched when a message is opened.
pub async fn discover_and_sync(
    target: &SyncTarget,
    session: &ProtonSession,
    has_mirror: bool,
    mirror: MirrorLoader<'_>,
    paging: &SyncPaging,
    progress: Progress<'_>,
) -> Result<MirrorUpdate> {
    if target.protocol != ProtocolKind::Proton {
        bail!("Proton adapter received a non-Proton sync target");
    }
    if let Some(checkpoint) = target
        .checkpoint
        .as_deref()
        .and_then(ProtonCheckpoint::parse)
        && has_mirror
        && let Some(applied) =
            events::apply_events(session.client(), &checkpoint.event_id, mirror).await?
    {
        let checkpoint = ProtonCheckpoint {
            event_id: applied.event_id,
            ..checkpoint
        }
        .to_json();
        return Ok(match applied.mailboxes {
            Some(mailboxes) => MirrorUpdate::Replace {
                snapshot: AccountSyncSnapshot {
                    checkpoint,
                    detail: format!(
                        "Proton account: applied {} message changes",
                        applied.changes
                    ),
                    identities: None,
                    mailboxes,
                },
                not_new: applied.not_new,
            },
            None => MirrorUpdate::Unchanged {
                checkpoint,
                detail: "Proton account up to date".to_owned(),
            },
        });
    }
    first_sync(session, paging, progress)
        .await
        .map(|snapshot| MirrorUpdate::Replace {
            snapshot,
            not_new: Vec::new(),
        })
}

async fn first_sync(
    session: &ProtonSession,
    paging: &SyncPaging,
    progress: Progress<'_>,
) -> Result<AccountSyncSnapshot> {
    let client = session.client();
    let event_id = client.get_latest_event_id().await?;
    let user = client.get_user().await?;
    let addresses = client.get_addresses().await?;
    let enabled: Vec<_> = addresses
        .iter()
        .filter(|address| address.status == AddressStatus::ENABLED)
        .collect();
    let identities: Vec<NewIdentity> = enabled
        .iter()
        .enumerate()
        .map(|(index, address)| NewIdentity {
            display_name: if address.display_name.is_empty() {
                user.display_name.clone()
            } else {
                address.display_name.clone()
            },
            email_address: address.email.clone(),
            signature: String::new(),
            is_primary: index == 0,
        })
        .collect();
    let (identity_for_address, fallback_identity) = address_identities(&addresses, &user.email);

    let labels = client.get_labels(&sync::LABEL_TYPES).await?;
    let listed = |count: usize| progress(&format!("Listing Proton messages: {count} so far"));
    let pages = sync::fetch_metadata_pages(
        client,
        "",
        paging.first_sync_pages,
        paging.page_size,
        &listed,
    )
    .await?;
    let mailboxes = sync::build_mailboxes(
        &labels,
        &pages.messages,
        &identity_for_address,
        &fallback_identity,
    );

    // The total only words the progress; a failed count does not stop
    // the sync.
    let total = match pages.next_end_id {
        Some(_) => client
            .count_messages(&zmail_proton_api::MessageFilter::default())
            .await
            .ok(),
        None => None,
    };
    let checkpoint = ProtonCheckpoint {
        event_id,
        backfill_end_id: pages.next_end_id,
        listed: pages.messages.len(),
        total,
    };
    let detail = if checkpoint.backfill_pending() {
        backfill_progress(&checkpoint)
    } else {
        format!(
            "Proton account {}: {} messages in {} mailboxes",
            user.name,
            pages.messages.len(),
            mailboxes.len()
        )
    };
    Ok(AccountSyncSnapshot {
        checkpoint: checkpoint.to_json(),
        detail,
        identities: (!identities.is_empty()).then_some(identities),
        mailboxes,
    })
}

/// Lists the next stretch of older mail when the checkpoint has a backfill
/// cursor: up to `paging.backfill_pages_per_run` pages older than the
/// oldest message listed so far, placed in their mailboxes like a first
/// sync places them. Returns `None` when the history is already complete.
///
/// Messages that change while they wait to be listed are still right: the
/// events put them in the mirror first, and listing them again later only
/// confirms them.
pub async fn backfill_older_mail(
    session: &ProtonSession,
    checkpoint: &str,
    paging: &SyncPaging,
    progress: Progress<'_>,
) -> Result<Option<Backfill>> {
    let Some(checkpoint) = ProtonCheckpoint::parse(checkpoint) else {
        return Ok(None);
    };
    let Some(end_id) = checkpoint.backfill_end_id.clone() else {
        return Ok(None);
    };
    let client = session.client();
    let addresses = client.get_addresses().await?;
    let (identity_for_address, fallback_identity) = address_identities(&addresses, "");
    let labels = client.get_labels(&sync::LABEL_TYPES).await?;
    let listed = |count: usize| {
        progress(&backfill_progress(&ProtonCheckpoint {
            listed: checkpoint.listed + count,
            ..checkpoint.clone()
        }))
    };
    let pages = sync::fetch_metadata_pages(
        client,
        &end_id,
        paging.backfill_pages_per_run,
        paging.page_size,
        &listed,
    )
    .await?;
    let mailboxes = sync::build_mailboxes(
        &labels,
        &pages.messages,
        &identity_for_address,
        &fallback_identity,
    );
    let checkpoint = ProtonCheckpoint {
        backfill_end_id: pages.next_end_id,
        listed: checkpoint.listed + pages.messages.len(),
        ..checkpoint
    };
    let complete = !checkpoint.backfill_pending();
    let detail = if complete {
        format!(
            "Proton account: all {} messages fetched",
            thousands(checkpoint.listed)
        )
    } else {
        backfill_progress(&checkpoint)
    };
    Ok(Some(Backfill {
        snapshot: AccountSyncSnapshot {
            checkpoint: checkpoint.to_json(),
            detail,
            identities: None,
            mailboxes,
        },
        complete,
    }))
}

/// The status line while older mail is still arriving.
fn backfill_progress(checkpoint: &ProtonCheckpoint) -> String {
    match checkpoint.total.filter(|total| *total > checkpoint.listed) {
        Some(total) => format!(
            "Fetched {} of {} messages; older mail is still arriving",
            thousands(checkpoint.listed),
            thousands(total)
        ),
        None => format!(
            "Fetched {} messages; older mail is still arriving",
            thousands(checkpoint.listed)
        ),
    }
}

/// A count with thousands separators: `85,000`.
fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Each address ID's email, and the email for messages of an unknown
/// address: the first enabled address, else `fallback`.
fn address_identities(addresses: &[Address], fallback: &str) -> (HashMap<String, String>, String) {
    let identity_for_address = addresses
        .iter()
        .map(|address| (address.id.clone(), address.email.clone()))
        .collect();
    let fallback_identity = addresses
        .iter()
        .find(|address| address.status == AddressStatus::ENABLED)
        .map(|address| address.email.clone())
        .unwrap_or_else(|| fallback.to_owned());
    (identity_for_address, fallback_identity)
}

/// Writes queued mailbox operations back to Proton, with Bridge's rules
/// for read state, stars, moves and deletion (see `ops.rs`).
pub async fn apply_pending_ops(
    target: &SyncTarget,
    session: &ProtonSession,
    ops: &[PendingOp],
) -> Result<OpApplyReport> {
    if target.protocol != ProtocolKind::Proton {
        bail!("Proton adapter received a non-Proton sync target");
    }
    ops::apply(session.client(), ops).await
}

/// Sends a composed message from one of the account's addresses, the way
/// Bridge's SMTP service does (see `send.rs`): a draft encrypted to the
/// sender, the attachments uploaded to it, and one package per body format.
/// Each recipient's scheme comes from Bridge's send preferences: the
/// recipient's keys (Proton's own, or found through WKD), the user's
/// signed contact card and the mail settings. Proton recipients and
/// PGP/Inline and PGP/MIME contacts get key packets, clear recipients the
/// session keys. Proton files the sent message in Sent; the next sync
/// brings it in.
pub async fn submit_outgoing_message(
    session: &ProtonSession,
    message: &OutgoingMessage,
) -> Result<()> {
    send::send_message(session, message).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_the_backfill_progress() {
        let checkpoint = ProtonCheckpoint {
            event_id: "e".to_owned(),
            backfill_end_id: Some("m".to_owned()),
            listed: 2_048,
            total: Some(85_000),
        };
        assert_eq!(
            backfill_progress(&checkpoint),
            "Fetched 2,048 of 85,000 messages; older mail is still arriving"
        );
        let unknown = ProtonCheckpoint {
            total: None,
            ..checkpoint.clone()
        };
        assert_eq!(
            backfill_progress(&unknown),
            "Fetched 2,048 messages; older mail is still arriving"
        );
        // New mail can take the count past the total taken at the start.
        let overtaken = ProtonCheckpoint {
            total: Some(2_000),
            ..checkpoint
        };
        assert_eq!(
            backfill_progress(&overtaken),
            "Fetched 2,048 messages; older mail is still arriving"
        );
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
