//! Proton Mail sync adapter for Zmail (issue #237, ADR 0004).
//!
//! The protocol client is `zmail-proton-api` (a go-proton-api port); this
//! crate follows Proton Mail Bridge's services on top of it and offers the
//! same entry points as `zmail-imap` and `zmail-jmap`, so `zmail-sync` can
//! dispatch to it. Nothing here is reachable from the UI until the local
//! store is encrypted at rest (#139); `zmail-sync` only builds it with its
//! `proton` feature, which `zmail-app` does not enable.

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

use std::collections::HashMap;

use anyhow::{Result, bail};
use zmail_core::{
    AccountSyncSnapshot, MailboxSnapshot, NewIdentity, OpApplyReport, OutgoingMessage, PendingOp,
    ProtocolKind, SyncTarget,
};
use zmail_proton_api::AddressStatus;

pub use events::{EVENT_JITTER, EVENT_PERIOD, event_poll_delay, has_new_events};
pub use hydrate::fetch_message_bodies;
pub use session::{ProtonSession, ProtonSessions, delete_session, has_session, store_new_session};
pub use signin::{ProtonSignIn, SignInStep, SignedInAccount};
pub use zmail_proton_api::{API_HOST_URL, AppVersion, ClientConfig};

/// Syncs a Proton account and returns the snapshot written into the store.
///
/// With a checkpoint from an earlier sync, the account's events since then
/// are applied to the stored mirror (Bridge's event loop): message changes
/// update their mailboxes in place. A refresh request from the server, or a
/// label, address or user change, falls back to a full sync.
///
/// A full sync takes the newest event ID first, so no change made during
/// it is missed, then mirrors the identities (combined address mode: one
/// account, all addresses), the labels as mailboxes and every message's
/// metadata. Bodies are fetched when a message is opened.
pub async fn discover_and_sync(
    target: &SyncTarget,
    session: &ProtonSession,
    existing_mailboxes: &[MailboxSnapshot],
) -> Result<AccountSyncSnapshot> {
    if target.protocol != ProtocolKind::Proton {
        bail!("Proton adapter received a non-Proton sync target");
    }
    if let Some(checkpoint) = target.checkpoint.as_deref().filter(|id| !id.is_empty())
        && !existing_mailboxes.is_empty()
        && let Some(snapshot) =
            events::apply_events(session.client(), checkpoint, existing_mailboxes).await?
    {
        return Ok(snapshot);
    }
    full_sync(session).await
}

async fn full_sync(session: &ProtonSession) -> Result<AccountSyncSnapshot> {
    let client = session.client();
    let checkpoint = client.get_latest_event_id().await?;
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
    let identity_for_address: HashMap<String, String> = addresses
        .iter()
        .map(|address| (address.id.clone(), address.email.clone()))
        .collect();
    let fallback_identity = enabled
        .first()
        .map(|address| address.email.clone())
        .unwrap_or_else(|| user.email.clone());

    let labels = client.get_labels(&sync::LABEL_TYPES).await?;
    let metadata = sync::fetch_all_metadata(client).await?;
    let mailboxes = sync::build_mailboxes(
        &labels,
        &metadata,
        &identity_for_address,
        &fallback_identity,
    );

    let detail = format!(
        "Proton account {}: {} messages in {} mailboxes",
        user.name,
        metadata.len(),
        mailboxes.len()
    );
    Ok(AccountSyncSnapshot {
        checkpoint,
        detail,
        identities: (!identities.is_empty()).then_some(identities),
        mailboxes,
    })
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
