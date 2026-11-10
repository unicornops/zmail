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

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use zmail_core::{
    AccountSyncSnapshot, MailDelta, NewIdentity, OpApplyReport, OutgoingMessage, PendingOp,
    ProtocolKind, SyncTarget, text::thousands,
};
use zmail_proton_api::{Address, AddressStatus, User};

pub use checkpoint::ProtonCheckpoint;
use events::EventsOutcome;
pub use events::{EVENT_JITTER, EVENT_PERIOD, event_poll_delay, has_new_events};
pub use hydrate::fetch_message_bodies;
pub use send::{OutgoingTrace, UnverifiedContactCard};
pub use session::{
    ProtonSession, ProtonSessions, StoredSession, delete_session, has_session, store_new_session,
    stored_session,
};
pub use signin::{ProtonSignIn, SignInStep, SignedInAccount};
pub use sync::SyncPaging;
pub use zmail_proton_api::{API_HOST_URL, AppVersion, ClientConfig};

/// Reports how far a sync has got, as a status line.
pub type Progress<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// What a sync does to the account's mirror.
#[derive(Debug)]
pub enum MirrorUpdate {
    /// Replace the mirror with this snapshot: a first sync, into an empty
    /// mirror.
    Replace(AccountSyncSnapshot),
    /// Apply the changes to these messages and mailboxes (and the
    /// identities, when they changed); nothing else in the mirror is read
    /// or rewritten. Events give these, and so does a refresh, which lists
    /// the newest mail again without removing the rest.
    Apply {
        /// The changes.
        delta: MailDelta,
        /// The messages the events created. Those the mirror did not hold
        /// yet are new mail; messages an update placed before the backfill
        /// listed them are not.
        created: Vec<String>,
        /// The checkpoint to store.
        checkpoint: String,
        /// The status detail.
        detail: String,
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
            Self::Replace(snapshot) => &snapshot.checkpoint,
            Self::Apply { checkpoint, .. } | Self::Unchanged { checkpoint, .. } => checkpoint,
        }
    }

    /// The status detail.
    pub fn detail(&self) -> &str {
        match self {
            Self::Replace(snapshot) => &snapshot.detail,
            Self::Apply { detail, .. } | Self::Unchanged { detail, .. } => detail,
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
    /// While the history is listed again after a refresh, every message
    /// listed in this step: write the step as a [`MailDelta`] covering
    /// them, so each listed message ends up exactly where Proton has it.
    pub relisted: Option<Vec<String>>,
}

impl Backfill {
    /// The step as a delta over the messages it listed, while relisting.
    pub fn relist_delta(&self) -> Option<MailDelta> {
        let message_ids = self.relisted.clone()?;
        Some(MailDelta {
            mailboxes: self.snapshot.mailboxes.clone(),
            message_ids,
            ..MailDelta::default()
        })
    }
}

/// Syncs a Proton account's changes and returns what to write into the
/// store.
///
/// With a checkpoint from an earlier sync and a stored mirror (`stored`
/// holds its mailbox names), the account's events since then become
/// changes to just the messages, mailboxes and identities they name
/// (Bridge's event loop), so a new message costs the same however large
/// the mailbox is and a label or address change keeps the mail already
/// fetched. A refresh request from the server lists the newest mail again
/// and restarts the backfill over the whole history in place (see
/// `relist`); nothing is removed.
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
    stored: &HashSet<String>,
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
        && !stored.is_empty()
    {
        let outcome =
            events::apply_events(session.client(), &checkpoint, stored, paging.page_size).await?;
        let applied = match outcome {
            EventsOutcome::Applied(applied) => *applied,
            EventsOutcome::Refresh => {
                return relist(session, &checkpoint, stored, paging, progress).await;
            }
        };
        let checkpoint = ProtonCheckpoint {
            event_id: applied.event_id,
            labels: applied.labels.unwrap_or(checkpoint.labels),
            ..checkpoint
        }
        .to_json();
        return Ok(match applied.delta {
            Some(delta) => MirrorUpdate::Apply {
                delta,
                created: applied.created,
                checkpoint,
                detail: applied.summary.detail(),
            },
            None => MirrorUpdate::Unchanged {
                checkpoint,
                detail: "Proton account up to date".to_owned(),
            },
        });
    }
    if !stored.is_empty() {
        // A mirror without a usable checkpoint is listed again in place,
        // never replaced.
        return relist(
            session,
            &ProtonCheckpoint::default(),
            stored,
            paging,
            progress,
        )
        .await;
    }
    first_sync(session, paging, progress)
        .await
        .map(MirrorUpdate::Replace)
}

/// The account's identities from its enabled addresses, primary first
/// (combined address mode); an address without a display name takes the
/// user's.
fn identities_for(user: &User, addresses: &[Address]) -> Vec<NewIdentity> {
    addresses
        .iter()
        .filter(|address| address.status == AddressStatus::ENABLED)
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
        .collect()
}

/// What a refresh request does when the mirror holds mail: Bridge resyncs
/// the account from scratch, which on a large mailbox would throw away
/// hours of fetched history. Zmail lists the newest mail again, writes it
/// in place (each message where Proton has it now), follows the label and
/// address changes, and restarts the backfill from there with the
/// checkpoint's `relist` flag, so the rest of the history is rewritten in
/// place too as it is listed again. Nothing is removed, so a message
/// deleted on Proton while its events were lost stays until it is listed
/// again or deleted here.
async fn relist(
    session: &ProtonSession,
    before: &ProtonCheckpoint,
    stored: &HashSet<String>,
    paging: &SyncPaging,
    progress: Progress<'_>,
) -> Result<MirrorUpdate> {
    let client = session.client();
    let event_id = client.get_latest_event_id().await?;
    let user = client.get_user().await?;
    let addresses = client.get_addresses().await?;
    let identities = identities_for(&user, &addresses);
    let (identity_for_address, fallback_identity) = address_identities(&addresses, &user.email);
    let labels = client.get_labels(&sync::LABEL_TYPES).await?;
    let label_names = events::label_map(&labels);
    // The mail of added mailboxes is listed with the rest.
    let structure = events::mailbox_changes(&before.labels, &label_names, stored);

    let listed = |count: usize| progress(&format!("Listing Proton messages again: {count} so far"));
    let pages = sync::fetch_metadata_pages(
        client,
        "",
        paging.first_sync_pages,
        paging.page_size,
        &listed,
    )
    .await?;
    let total = match pages.next_end_id {
        Some(_) => client
            .count_messages(&zmail_proton_api::MessageFilter::default())
            .await
            .ok(),
        None => None,
    };
    let delta = MailDelta {
        mailboxes: sync::build_mailboxes(
            &labels,
            &pages.messages,
            &identity_for_address,
            &fallback_identity,
        ),
        message_ids: pages.messages.iter().map(|meta| meta.id.clone()).collect(),
        renamed: structure.renamed,
        removed: structure.removed,
        identities: (!identities.is_empty()).then_some(identities),
    };
    let checkpoint = ProtonCheckpoint {
        event_id,
        relist: pages.next_end_id.is_some(),
        backfill_end_id: pages.next_end_id,
        listed: pages.messages.len(),
        total,
        labels: label_names,
    };
    let detail = if checkpoint.backfill_pending() {
        backfill_progress(&checkpoint)
    } else {
        format!(
            "Proton asked for a full resync: listed all {} messages again",
            thousands(checkpoint.listed)
        )
    };
    Ok(MirrorUpdate::Apply {
        delta,
        // Listing again finds history, not new mail.
        created: Vec::new(),
        checkpoint: checkpoint.to_json(),
        detail,
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
    let identities = identities_for(&user, &addresses);
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
        labels: events::label_map(&labels),
        relist: false,
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
    let relisted = checkpoint
        .relist
        .then(|| pages.messages.iter().map(|meta| meta.id.clone()).collect());
    let checkpoint = ProtonCheckpoint {
        relist: checkpoint.relist && pages.next_end_id.is_some(),
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
        relisted,
    }))
}

/// The status line while older mail is still arriving, or being listed
/// again after a refresh request.
fn backfill_progress(checkpoint: &ProtonCheckpoint) -> String {
    if checkpoint.relist {
        return match checkpoint.total.filter(|total| *total > checkpoint.listed) {
            Some(total) => format!(
                "Proton asked for a full resync: listed {} of {} messages again",
                thousands(checkpoint.listed),
                thousands(total)
            ),
            None => format!(
                "Proton asked for a full resync: listed {} messages again",
                thousands(checkpoint.listed)
            ),
        };
    }
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
///
/// Every key is checked before it is used (ADR 0004, "Verifying keys").
/// When a recipient's contact card pins keys and Proton serves none of
/// them, the send fails with a
/// [`PinnedKeyChanged`](zmail_core::key_change::PinnedKeyChanged) error
/// unless `trusted` holds the user's trust in the served key for that
/// recipient.
pub async fn submit_outgoing_message(
    session: &ProtonSession,
    message: &OutgoingMessage,
    trusted: &[zmail_core::key_change::KeyTrust],
) -> Result<()> {
    send::send_message(session, message, trusted).await
}

/// Looks for the message an interrupted send was sending (Zmail stopped
/// mid-send), by its Message-ID: sent, only its draft, or nothing. An
/// error means the lookup failed, which says nothing about the message.
pub async fn find_outgoing_message(
    session: &ProtonSession,
    message_id: &str,
) -> Result<OutgoingTrace> {
    send::find_outgoing_message(session, message_id).await
}

/// Sends the draft an interrupted send left
/// ([`OutgoingTrace::Draft`]), as [`submit_outgoing_message`] would have
/// sent the message.
pub async fn send_existing_draft(
    session: &ProtonSession,
    draft_id: &str,
    message: &OutgoingMessage,
    trusted: &[zmail_core::key_change::KeyTrust],
) -> Result<()> {
    send::send_existing_draft(session, draft_id, message, trusted).await
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
            ..ProtonCheckpoint::default()
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
        let relisting = ProtonCheckpoint {
            relist: true,
            ..unknown.clone()
        };
        assert_eq!(
            backfill_progress(&relisting),
            "Proton asked for a full resync: listed 2,048 messages again"
        );
        assert_eq!(
            backfill_progress(&ProtonCheckpoint {
                total: Some(85_000),
                ..relisting
            }),
            "Proton asked for a full resync: listed 2,048 of 85,000 messages again"
        );
    }
}
