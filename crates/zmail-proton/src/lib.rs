//! Proton Mail sync adapter for Zmail (issue #237, ADR 0004).
//!
//! The protocol client is `zmail-proton-api` (a go-proton-api port); this
//! crate follows Proton Mail Bridge's services on top of it and offers the
//! same entry points as `zmail-imap` and `zmail-jmap`, so `zmail-sync` can
//! dispatch to it. Nothing here is reachable from the UI until the local
//! store is encrypted at rest (#139); `zmail-sync` only builds it with its
//! `proton` feature, which `zmail-app` does not enable.

mod mailbox;
mod session;
mod sync;

use std::collections::HashMap;

use anyhow::{Result, bail};
use zmail_core::{
    AccountSyncSnapshot, MailboxSnapshot, NewIdentity, OpApplyReport, PendingOp, ProtocolKind,
    SyncTarget,
};
use zmail_proton_api::AddressStatus;

pub use session::{ProtonSession, ProtonSessions, delete_session, has_session, store_new_session};
pub use zmail_proton_api::{AppVersion, ClientConfig};

/// Syncs a Proton account and returns the snapshot written into the store.
///
/// The checkpoint is the newest event ID, taken before anything else so no
/// change made during the sync is missed. Identities come from the
/// account's enabled addresses (combined address mode: one account, all
/// addresses). Then the labels become mailboxes and every message's
/// metadata is placed in them; bodies are fetched when a message is opened.
pub async fn discover_and_sync(
    target: &SyncTarget,
    session: &ProtonSession,
    _existing_mailboxes: &[MailboxSnapshot],
) -> Result<AccountSyncSnapshot> {
    if target.protocol != ProtocolKind::Proton {
        bail!("Proton adapter received a non-Proton sync target");
    }
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

/// Writes queued mailbox operations back to Proton. Not supported yet
/// (#237): every operation is reported as failed and stays queued.
pub async fn apply_pending_ops(
    target: &SyncTarget,
    _session: &ProtonSession,
    ops: &[PendingOp],
) -> Result<OpApplyReport> {
    if target.protocol != ProtocolKind::Proton {
        bail!("Proton adapter received a non-Proton sync target");
    }
    Ok(OpApplyReport {
        applied: Vec::new(),
        failed: ops
            .iter()
            .map(|op| {
                (
                    op.id,
                    "Proton mailbox changes are not written back yet".to_owned(),
                )
            })
            .collect(),
    })
}
