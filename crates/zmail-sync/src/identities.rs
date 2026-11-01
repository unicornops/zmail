//! Sending an identity's values to its JMAP server, or creating it there
//! again, when the user chooses to (Rob's decision of 1 November 2026;
//! the bookkeeping is `zmail_store`'s, the classification
//! `zmail_core::identity_sync`'s). The sync itself only reads the
//! server's identities and never writes them.

use anyhow::{Context, Result, bail};
use zmail_core::{ProtocolKind, SyncTarget, identity_sync::IdentitySyncStatus};
use zmail_store::{IdentityLink, IdentityMergeReport};

use crate::SyncService;

impl SyncService {
    /// "Update the server": sets the linked server identity's name,
    /// reply-to and signature to the identity's values in Zmail
    /// (`Identity/set` update), and records that the server holds them.
    /// The server's refusal is the error, with its reason.
    ///
    /// Runs on the background runtime; awaiting it from another executor
    /// only waits for it.
    pub async fn push_identity_and_wait(&self, identity_id: i64) -> Result<()> {
        let service = self.clone();
        self.inner
            .handle
            .spawn(async move { service.push_identity(identity_id).await })
            .await
            .context("the identity worker exited before returning a result")?
    }

    /// Creates an identity the server no longer lists on the server again,
    /// with its values in Zmail (`Identity/set` create), and links it to
    /// the new server identity. Like [`Self::push_identity_and_wait`], it
    /// runs on the background runtime.
    pub async fn recreate_identity_and_wait(&self, identity_id: i64) -> Result<()> {
        let service = self.clone();
        self.inner
            .handle
            .spawn(async move { service.recreate_identity(identity_id).await })
            .await
            .context("the identity worker exited before returning a result")?
    }

    async fn push_identity(&self, identity_id: i64) -> Result<()> {
        let (target, link) = self.jmap_identity(identity_id)?;
        let Some(server_id) = link.server_id.filter(|_| link.status.differs()) else {
            bail!("{} has nothing to send to the server", link.email);
        };
        let secret = self.connect_secret(&target).await?;
        zmail_jmap::update_server_identity(&target, &secret, &server_id, &link.local).await?;
        self.inner
            .store
            .record_identity_on_server(identity_id, &server_id, &link.local)
    }

    async fn recreate_identity(&self, identity_id: i64) -> Result<()> {
        let (target, link) = self.jmap_identity(identity_id)?;
        if link.status != IdentitySyncStatus::NotOnServer {
            bail!("{} is already on the server", link.email);
        }
        let secret = self.connect_secret(&target).await?;
        let server_id =
            zmail_jmap::create_server_identity(&target, &secret, &link.email, &link.local).await?;
        self.inner
            .store
            .record_identity_on_server(identity_id, &server_id, &link.local)
    }

    /// The identity and its JMAP account.
    fn jmap_identity(&self, identity_id: i64) -> Result<(SyncTarget, IdentityLink)> {
        let link = self
            .inner
            .store
            .identity_link(identity_id)?
            .context("that identity no longer exists")?;
        let target = self
            .inner
            .store
            .sync_target(link.account_id)?
            .context("that identity's account no longer exists")?;
        if target.protocol != ProtocolKind::Jmap {
            bail!("only JMAP accounts keep identities on the server");
        }
        Ok((target, link))
    }
}

/// What a sync's identity merge adds to the account's sync status, when
/// it found something for the user to look at.
pub(crate) fn sync_note(report: &IdentityMergeReport) -> Option<String> {
    let mut notes = Vec::new();
    match report.newly_differing {
        0 => {}
        1 => notes.push("an identity differs from the server".to_owned()),
        count => notes.push(format!("{count} identities differ from the server")),
    }
    match report.newly_removed {
        0 => {}
        1 => notes.push("an identity is no longer on the server".to_owned()),
        count => notes.push(format!("{count} identities are no longer on the server")),
    }
    if notes.is_empty() {
        return None;
    }
    let mut note = notes.join(", ");
    if let Some(first) = note.get(..1) {
        note = first.to_uppercase() + &note[1..];
    }
    Some(format!("{note}: see Settings \u{2192} Accounts"))
}

#[cfg(test)]
mod tests {
    use zmail_store::IdentityMergeReport;

    use super::sync_note;

    #[test]
    fn the_sync_status_mentions_new_differences_only() {
        assert_eq!(sync_note(&IdentityMergeReport::default()), None);
        let report = IdentityMergeReport {
            imported: vec!["help@example.com".to_owned()],
            ..IdentityMergeReport::default()
        };
        assert_eq!(sync_note(&report), None, "an import needs no attention");
        let report = IdentityMergeReport {
            newly_differing: 2,
            newly_removed: 1,
            ..IdentityMergeReport::default()
        };
        assert_eq!(
            sync_note(&report).as_deref(),
            Some(
                "2 identities differ from the server, an identity is no longer on the server: \
                 see Settings \u{2192} Accounts"
            )
        );
    }
}
