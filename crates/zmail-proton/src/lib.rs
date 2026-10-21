//! Proton Mail sync adapter for Zmail (issue #237, ADR 0004).
//!
//! The protocol client is `zmail-proton-api` (a go-proton-api port); this
//! crate follows Proton Mail Bridge's services on top of it and offers the
//! same entry points as `zmail-imap` and `zmail-jmap`, so `zmail-sync` can
//! dispatch to it. Nothing here is reachable from the UI until the local
//! store is encrypted at rest (#139); `zmail-sync` only builds it with its
//! `proton` feature, which `zmail-app` does not enable.

mod session;

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
/// The identities come from the account's enabled addresses (combined
/// address mode: one account, all addresses). The checkpoint is the
/// newest event ID, taken before anything else so no change made during
/// the sync is missed. Mailboxes and messages arrive with read sync (#237).
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
    let identities: Vec<NewIdentity> = addresses
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
        .collect();
    let detail = format!(
        "Proton account {} with {} address{}",
        user.name,
        identities.len(),
        if identities.len() == 1 { "" } else { "es" }
    );
    Ok(AccountSyncSnapshot {
        checkpoint,
        detail,
        identities: (!identities.is_empty()).then_some(identities),
        mailboxes: Vec::new(),
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
