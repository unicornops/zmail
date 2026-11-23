//! Real JMAP adapter for Phase 2.

mod batches;
mod changes;
mod identities;
mod push;
mod wording;

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, anyhow, bail};
use jmap_client::client::Client;
use jmap_wire::{
    BlobDownload, Comparator, Filter, Get, Query, SetErrorType, SetOutcome,
    mail::{
        AsRaw, Email, EmailAddress, EmailBodyPart, EmailFilterCondition, EmailImport,
        EmailProperty, EmailRecord, EmailToImport, EmailUpdate, HeaderProperty, Mailbox, UtcDate,
    },
};
use serde::{Deserialize, Serialize};
use url::Url;
use zmail_core::threading::bare_message_id;
use zmail_core::{
    AccountSyncSnapshot, AuthKind, BlobStore, MailboxSnapshot, MessageAttachment, MessageHeaders,
    MessageSnapshot, NewIdentity, OpApplyReport, OutgoingMessage, PendingOp, PendingOpKind,
    ProtocolKind, SyncTarget, ThreadSnapshot,
};
use zmail_security::sanitize_html_for_display;
use zmail_smtp::render_rfc822_message;

pub use identities::{create_server_identity, destroy_server_identity, update_server_identity};
pub use push::{
    PUSH_PING_INTERVAL, PushEnd, PushEvent, PushPreference, PushTransport, watch_mail_changes,
};

/// The Phase 0 decision for the first JMAP implementation spike.
pub const IMPLEMENTATION_STRATEGY: &str =
    "Start behind an internal adapter boundary with a third-party JMAP client candidate.";

/// JMAP features that are explicitly in MVP scope.
pub const MVP_SCOPE: &[&str] = &[
    "mailbox sync",
    "email sync",
    "identity support",
    "EmailSubmission",
];

pub(crate) const QUERY_PAGE_SIZE: usize = 250;
/// Maximum number of attachment blobs persisted per message, mirroring the
/// IMAP adapter's cap.
const MAX_ATTACHMENTS_PER_MESSAGE: usize = 25;
/// Maximum size of a single persisted attachment blob, mirroring the IMAP
/// adapter's cap.
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

/// What a JMAP sync brings back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmapSync {
    /// The mailbox snapshot written into the local store. It carries no
    /// identities: the server's are merged separately, so a sync never
    /// overwrites the user's own.
    pub snapshot: AccountSyncSnapshot,
    /// The server's identities (`Identity/get`), for
    /// `LocalStore::merge_server_identities`; `None` when the server would
    /// not list them, which leaves the local identities as they are.
    pub identities: Option<Vec<zmail_core::identity_sync::ServerIdentity>>,
}

/// Connects to a live JMAP account and returns the mailbox snapshot written
/// into the local store, with the server's identities.
pub async fn discover_and_sync(
    target: &SyncTarget,
    secret: &str,
    existing_mailboxes: &[MailboxSnapshot],
) -> Result<JmapSync> {
    if target.protocol != ProtocolKind::Jmap {
        bail!("JMAP adapter received a non-JMAP sync target");
    }
    if !(target.endpoint.starts_with("https://") || target.endpoint.starts_with("http://")) {
        bail!("JMAP endpoints should be absolute HTTP or HTTPS URLs");
    }

    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;

    let checkpoint = JmapCheckpoint::from_json(target.checkpoint.as_deref())?;
    // A server that won't list its identities (no submission capability,
    // say) still syncs mail; the local identities stay as they are.
    let server_identities = identities::fetch_server_identities(&client).await.ok();
    // Mail without a sender identity is filed under the local primary.
    let identities = target
        .identities
        .iter()
        .enumerate()
        .map(|(index, identity)| NewIdentity {
            display_name: identity.display_name.clone(),
            email_address: identity.email_address.clone(),
            signature: String::new(),
            is_primary: identity.is_primary || index == 0,
        })
        .collect::<Vec<_>>();
    let finish = |snapshot| JmapSync {
        snapshot,
        identities: server_identities.clone(),
    };
    let (mailbox_details, mailbox_state) = fetch_mailboxes(&client).await?;
    let mailbox_ids = changes::mailbox_id_map(&mailbox_details);

    // Incremental path: once a sync has recorded the Email state, only the
    // emails changed since then are fetched (`Email/changes`). It falls
    // back to the full sync below whenever the server cannot say what
    // changed.
    if let (Some(email_state), Some(previous_mailbox_ids)) = (
        checkpoint.email_state.as_deref(),
        checkpoint.mailbox_ids.as_ref(),
    ) && !existing_mailboxes.is_empty()
    {
        let primary = primary_identity(&identities, &target.username).to_owned();
        if let Some(incremental) = changes::sync_email_changes(
            &client,
            email_state,
            previous_mailbox_ids,
            &mailbox_details,
            existing_mailboxes,
            &primary,
            &target.blobs,
        )
        .await?
        {
            let detail = if incremental.changed_emails == 0 {
                format!("JMAP mailbox state unchanged for {}", target.display_name)
            } else {
                format!(
                    "JMAP incremental sync applied {} email changes for {}",
                    incremental.changed_emails, target.display_name
                )
            };
            return Ok(finish(AccountSyncSnapshot {
                checkpoint: JmapCheckpoint {
                    mailbox_query_state: Some(mailbox_state),
                    email_query_state: None,
                    email_state: Some(incremental.email_state),
                    mailbox_ids: Some(mailbox_ids),
                }
                .to_json()?,
                detail,
                identities: None,
                mailboxes: incremental.mailboxes,
            }));
        }
    }

    // The state is read before the query, so anything that changes while
    // the full sync runs shows up in the next `Email/changes`.
    let email_state = changes::fetch_email_state(&client).await?;
    let email_query = fetch_email_query(&client).await?;

    if checkpoint.mailbox_query_state.as_deref() == Some(mailbox_state.as_str())
        && checkpoint.email_query_state.as_deref() == Some(email_query.query_state.as_str())
        && !existing_mailboxes.is_empty()
    {
        return Ok(finish(AccountSyncSnapshot {
            checkpoint: JmapCheckpoint {
                mailbox_query_state: Some(mailbox_state),
                email_query_state: Some(email_query.query_state),
                email_state: Some(email_state),
                mailbox_ids: Some(mailbox_ids),
            }
            .to_json()?,
            detail: format!("JMAP mailbox state unchanged for {}", target.display_name),
            identities: None,
            mailboxes: existing_mailboxes.to_vec(),
        }));
    }

    let emails = fetch_emails(&client, &email_query.ids, &target.blobs).await?;
    let mailboxes = group_mailboxes(mailbox_details, emails, &identities, &target.username);

    Ok(finish(AccountSyncSnapshot {
        checkpoint: JmapCheckpoint {
            mailbox_query_state: Some(mailbox_state),
            email_query_state: Some(email_query.query_state),
            email_state: Some(email_state),
            mailbox_ids: Some(mailbox_ids),
        }
        .to_json()?,
        detail: format!(
            "JMAP sync complete against {} with {}",
            target.endpoint,
            MVP_SCOPE.join(", ")
        ),
        identities: None,
        mailboxes,
    }))
}

/// Applies queued mailbox operations to the live JMAP account.
///
/// Read and flag operations set or clear the `$seen` and `$flagged`
/// keywords, a move changes the email's mailboxes ([`move_update`]), all
/// through `Email/set` updates, and destroy operations rely on the
/// server's trash handling through `Email/set` destroy.
pub async fn apply_pending_ops(
    target: &SyncTarget,
    secret: &str,
    ops: &[PendingOp],
) -> Result<OpApplyReport> {
    if target.protocol != ProtocolKind::Jmap {
        bail!("JMAP adapter received a non-JMAP sync target");
    }

    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;
    let mut report = OpApplyReport::default();

    // The queue names mailboxes as the mirror does, by name; the server
    // is told their ids. Two mailboxes with one name (#512) resolve to
    // one of them, as they do in the mirror.
    let needs_mailbox_map = ops.iter().any(|op| matches!(op.kind, PendingOpKind::Move));
    let mailboxes: HashMap<String, JmapMailbox> = if needs_mailbox_map {
        fetch_mailboxes(&client)
            .await?
            .0
            .into_iter()
            .map(|mailbox| (mailbox.name.clone(), mailbox))
            .collect()
    } else {
        HashMap::new()
    };

    for op in ops {
        let result = apply_single_jmap_op(&client, op, &mailboxes).await;
        match result {
            Ok(()) => report.applied.push(op.id),
            Err(error) => report.failed.push((op.id, format!("{error:#}"))),
        }
    }

    Ok(report)
}

async fn apply_single_jmap_op(
    client: &Connection,
    op: &PendingOp,
    mailboxes: &HashMap<String, JmapMailbox>,
) -> Result<()> {
    let email_id = op.remote_id.as_str();
    match op.kind {
        PendingOpKind::MarkRead => {
            update_email(client, email_id, EmailUpdate::new().set_keyword("$seen"))
                .await
                .with_context(|| format!("failed to mark JMAP email {email_id} as read"))
        }
        PendingOpKind::MarkUnread => {
            update_email(client, email_id, EmailUpdate::new().clear_keyword("$seen"))
                .await
                .with_context(|| format!("failed to mark JMAP email {email_id} as unread"))
        }
        PendingOpKind::SetFlagged => {
            update_email(client, email_id, EmailUpdate::new().set_keyword("$flagged"))
                .await
                .with_context(|| format!("failed to flag JMAP email {email_id}"))
        }
        PendingOpKind::ClearFlagged => update_email(
            client,
            email_id,
            EmailUpdate::new().clear_keyword("$flagged"),
        )
        .await
        .with_context(|| format!("failed to unflag JMAP email {email_id}")),
        // An email the server no longer has is as good as destroyed: the
        // write-back of a refused send's copy (#406) may find the next
        // attempt removed it first.
        PendingOpKind::Destroy => destroy_emails(client, &[email_id])
            .await
            .with_context(|| format!("failed to destroy JMAP email {email_id}")),
        PendingOpKind::Move => {
            let Some(target_mailbox) = op.target_mailbox.as_deref() else {
                bail!("move operation without a target mailbox");
            };
            let Some(target) = mailboxes.get(target_mailbox) else {
                bail!("the JMAP account has no mailbox named {target_mailbox}");
            };
            let source = mailboxes.get(&op.source_mailbox);
            move_email(client, email_id, source, target)
                .await
                .with_context(|| {
                    format!("failed to move JMAP email {email_id} to {target_mailbox}")
                })
        }
    }
}

/// The change to an email's mailboxes that moves it from `source` to
/// `target`.
///
/// A JMAP email can be in several mailboxes at once, which servers use
/// for labels, so a move changes only the two mailboxes it names: the
/// email leaves `source` and joins `target`, and stays wherever else it
/// is. That is one `Email/set` update, so the email is never in no
/// mailbox on the way (RFC 8620 section 5.3: the entries of a patch are
/// applied together).
///
/// The whole set of mailboxes is replaced by `target` alone in two
/// cases:
///
/// - `target` is the trash or the junk mailbox. RFC 8621 section 4.6
///   says how an email is deleted to the trash: it is put there "and
///   remove all other Mailbox ids". An email left under a label as well
///   would still show there, and be destroyed with the trash; and a
///   message reported as junk is junk under every label.
/// - the server has no mailbox with the source's name (renamed or
///   removed since the move was queued), so there is nothing to take
///   the email out of. This is what every move did before, and moves
///   the email all the same.
fn move_update(source: Option<&JmapMailbox>, target: &JmapMailbox) -> EmailUpdate {
    let deletes = matches!(target.role.as_deref(), Some("trash" | "junk"));
    match source {
        Some(source) if !deletes => EmailUpdate::new().move_between(&source.id, &target.id),
        _ => EmailUpdate::new().replace_mailboxes([target.id.as_str()]),
    }
}

/// Moves the email `email_id` from `source` to `target`
/// ([`move_update`]).
///
/// A server that refuses the per-mailbox patch as a patch (`invalidPatch`,
/// `invalidProperties` that does not point elsewhere, or the
/// `invalidArguments` Apache James gives) is asked a
/// second way: the email's mailboxes are read, and the same change is
/// sent as the whole set. RFC 8621 section 4.6 gives both forms; this
/// is for a server that implements only the second, or that checks the
/// removal before the addition and so sees an email in no mailbox. The
/// first refusal is reported when the second way fails too.
async fn move_email(
    client: &Connection,
    email_id: &str,
    source: Option<&JmapMailbox>,
    target: &JmapMailbox,
) -> Result<()> {
    let update = move_update(source, target);
    let (Some(source), false) = (source, update_replaces_mailboxes(&update)) else {
        return update_email(client, email_id, update).await;
    };
    let refusal = match try_update_email(client, email_id, update).await? {
        Ok(()) => return Ok(()),
        Err(refusal) => refusal,
    };
    let patch_refused = refusal.set_error.as_ref().is_some_and(|error| {
        match &error.error_type {
            SetErrorType::InvalidPatch => true,
            SetErrorType::InvalidProperties => {
                error.properties.as_ref().is_none_or(Vec::is_empty)
                    || error.names_property("mailboxIds")
            }
            // Not a SetError type of the RFCs, but what Apache James
            // answers for an update it cannot read.
            other => other.as_str() == "invalidArguments",
        }
    });
    if patch_refused && let Ok(Some(mut mailbox_ids)) = email_mailbox_ids(client, email_id).await {
        mailbox_ids.retain(|id| *id != source.id);
        if !mailbox_ids.contains(&target.id) {
            mailbox_ids.push(target.id.clone());
        }
        let whole = EmailUpdate::new().replace_mailboxes(mailbox_ids);
        if matches!(try_update_email(client, email_id, whole).await, Ok(Ok(()))) {
            return Ok(());
        }
    }
    Err(refusal.error)
}

/// Whether `update` sets `mailboxIds` as a whole.
fn update_replaces_mailboxes(update: &EmailUpdate) -> bool {
    jmap_wire::PatchObject::from(update.clone())
        .entries()
        .any(|(path, _)| path == "mailboxIds")
}

/// The ids of the mailboxes the email `email_id` is in; `None` when the
/// server no longer has it.
async fn email_mailbox_ids(client: &Connection, email_id: &str) -> Result<Option<Vec<String>>> {
    let response = client
        .call(
            Get::<Email>::new(client.account_id())
                .ids([email_id])
                .properties([EmailProperty::Id, EmailProperty::MailboxIds]),
        )
        .await
        .map_err(wording::reported)?;
    Ok(response
        .list
        .into_iter()
        .find(|email| email.id.as_deref() == Some(email_id))
        .map(|email| members(email.mailbox_ids.as_ref())))
}

/// A server's refusal to update one email: how it is reported, and the
/// SetError when that is what it was.
struct UpdateRefused {
    error: anyhow::Error,
    set_error: Option<jmap_wire::SetError>,
}

/// Updates the email `email_id` (`Email/set`).
async fn update_email(client: &Connection, email_id: &str, update: EmailUpdate) -> Result<()> {
    try_update_email(client, email_id, update)
        .await?
        .map_err(|refusal| refusal.error)
}

/// Updates the email `email_id` (`Email/set`). The outer error is a
/// failure of the call; the inner one is the server's answer that it
/// did not update this email, or that does not say.
async fn try_update_email(
    client: &Connection,
    email_id: &str,
    update: EmailUpdate,
) -> Result<std::result::Result<(), UpdateRefused>> {
    let response = client
        .call(email_update(client.account_id(), email_id, update))
        .await
        .map_err(wording::reported)?;
    Ok(match response.outcome_of_update(email_id) {
        SetOutcome::Done(_) => Ok(()),
        SetOutcome::Refused(error) => Err(UpdateRefused {
            error: wording::set_refused(error),
            set_error: Some(error.clone()),
        }),
        SetOutcome::Unanswered => Err(UpdateRefused {
            error: anyhow!("the JMAP server did not say whether it changed email {email_id}"),
            set_error: None,
        }),
    })
}

/// The `Email/set` that changes the email `email_id` as `update` says.
fn email_update(account_id: &str, email_id: &str, update: EmailUpdate) -> jmap_wire::Set<Email> {
    let mut set = jmap_wire::Set::<Email>::new(account_id);
    set.update(email_id, update);
    set
}

/// Submits a composed outgoing message through JMAP `EmailSubmission`:
/// imports its copy into Sent, marked [`SUBMIT_PENDING`], and submits
/// that copy. When the server refuses the submission
/// ([`SubmissionRefused`]), the copy is destroyed again, so Sent never
/// shows a message that was not sent.
pub async fn submit_outgoing_message(
    endpoint: &str,
    username: &str,
    secret: &str,
    auth: AuthKind,
    identity_email: &str,
    message: &OutgoingMessage,
) -> Result<()> {
    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        bail!("JMAP submission requires at least one recipient");
    }
    // The envelope is JSON, but the server takes its addresses into SMTP.
    message.check_addresses()?;

    let client = connect_client(endpoint, username, secret, auth).await?;
    import_and_submit(&client, identity_email, message).await
}

/// Sends `message` again after an earlier attempt, begun no sooner than
/// `attempted_since` (Unix seconds; every email is looked at when
/// unknown), ended without it being sent for sure: a lost answer or a
/// timeout, which may hide an accepted submission, or a refusal whose
/// copy could not be destroyed. The copies that attempt left with the
/// message's Message-ID settle it, by the keywords [`submit_email`] sets:
///
/// - a copy marked [`SUBMITTED`]: the server accepted the earlier
///   submission, so nothing is sent again;
/// - copies still marked [`SUBMIT_PENDING`] were never submitted. They
///   are destroyed, as the draft may have been edited since, and the
///   message is imported and submitted afresh; when one cannot be
///   destroyed, nothing is sent, so Sent never holds two copies;
/// - a copy with neither (an older Zmail's) proves nothing either way
///   and is left as it is, as before Zmail marked copies.
pub async fn resubmit_outgoing_message(
    endpoint: &str,
    username: &str,
    secret: &str,
    auth: AuthKind,
    identity_email: &str,
    message: &OutgoingMessage,
    attempted_since: Option<i64>,
) -> Result<()> {
    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        bail!("JMAP submission requires at least one recipient");
    }
    // The envelope is JSON, but the server takes its addresses into SMTP.
    message.check_addresses()?;

    let client = connect_client(endpoint, username, secret, auth).await?;
    if let Some(message_id) = message.message_id.as_deref().and_then(bare_message_id) {
        let copies = find_copies(&client, &message_id, attempted_since).await?;
        if copies.iter().any(|copy| copy.submitted) {
            return Ok(());
        }
        let unsent = copies
            .iter()
            .filter(|copy| copy.pending)
            .map(|copy| copy.id.as_str())
            .collect::<Vec<_>>();
        if !unsent.is_empty() {
            destroy_emails(&client, &unsent)
                .await
                .context("failed to remove the copy an earlier attempt left in Sent")?;
        }
    }
    import_and_submit(&client, identity_email, message).await
}

/// Imports `message`'s copy into Sent and submits it from the identity
/// with `identity_email` ([`submit_or_discard`]).
async fn import_and_submit(
    client: &Connection,
    identity_email: &str,
    message: &OutgoingMessage,
) -> Result<()> {
    let identity_id = resolve_identity_id(client, identity_email)
        .await?
        .with_context(|| format!("no JMAP identity matches {identity_email}"))?;
    let sent_mailbox_ids = sent_mailbox_ids(client).await?;
    let raw_message = render_rfc822_message(message)?;
    let email_id = import_message(client, raw_message, &sent_mailbox_ids)
        .await
        .context("failed to import the outgoing message into JMAP")?
        .with_context(|| "JMAP import did not return an email id")?;

    submit_or_discard(client, &email_id, &identity_id, message).await
}

/// Uploads `raw_message` and imports it into `mailbox_ids`
/// (`Email/import`), marked [`SUBMIT_PENDING`] until the submission
/// succeeds, so an interrupted send can tell an imported copy that never
/// went from one that did. Returns the new email's id, `None` when the
/// server created it without saying which it is.
async fn import_message(
    client: &Connection,
    raw_message: Vec<u8>,
    mailbox_ids: &[String],
) -> Result<Option<String>> {
    let blob = client
        .upload(raw_message)
        .await
        .map_err(wording::reported)?;
    let (import, creation_id) = import_request(client.account_id(), &blob.blob_id, mailbox_ids);
    let response = client.call(import).await.map_err(wording::reported)?;
    match response.outcome_of(&creation_id) {
        SetOutcome::Done(email) => Ok(email.id.clone()),
        SetOutcome::Refused(error) => Err(wording::set_refused(error)),
        SetOutcome::Unanswered => Err(anyhow!(
            "the JMAP server did not say whether it imported the message"
        )),
    }
}

/// The `Email/import` of the uploaded message `blob_id` into
/// `mailbox_ids`, marked [`SUBMIT_PENDING`]; with its creation id.
fn import_request(
    account_id: &str,
    blob_id: &str,
    mailbox_ids: &[String],
) -> (EmailImport, String) {
    let mut import = EmailImport::new(account_id);
    let creation_id =
        import.email(EmailToImport::new(blob_id, mailbox_ids).keywords([SUBMIT_PENDING]));
    (import, creation_id)
}

/// The keyword an outgoing message's copy is imported with until its
/// submission succeeds (RFC 5550's `$SubmitPending`; JMAP keywords are
/// lower case).
const SUBMIT_PENDING: &str = "$submitpending";
/// The keyword the server gives the copy, in place of
/// [`SUBMIT_PENDING`], when it accepts the submission (RFC 5550's
/// `$Submitted`).
const SUBMITTED: &str = "$submitted";
/// How far before an interrupted send's claim its copy is looked for, in
/// case the server's clock is behind this computer's.
const SUBMISSION_CLOCK_SLACK: i64 = 24 * 60 * 60;

/// The JMAP server answered that it did not create the submission, so
/// the message was not sent: a `notCreated` SetError (RFC 8621, section
/// 7.5: `forbiddenFrom`, `invalidRecipients`, `tooManyRecipients`,
/// `forbiddenToSend` and the rest), or a method error, after which the
/// server is as it was (RFC 8620, section 3.6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionRefused {
    /// The error's type, as the server gave it.
    pub error_type: String,
    /// The server's description, when it gave one.
    pub description: Option<String>,
    /// The copy in Sent that could not be destroyed after the refusal:
    /// still marked `$submitpending`, for the caller to remove later.
    pub unsent_copy: Option<String>,
}

impl SubmissionRefused {
    /// The refusal a SetError or method error (`type`, `description`)
    /// states.
    fn from_error(error: &serde_json::Value) -> Self {
        let text = |name: &str| {
            error
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        Self {
            error_type: text("type").unwrap_or_default(),
            description: text("description").filter(|text| !text.trim().is_empty()),
            unsent_copy: None,
        }
    }

    /// Whether sending the same message again cannot work until the
    /// message, the account or the server changes, so the send fails at
    /// once. A rate limit, the server's own failures (`serverFail`,
    /// `serverUnavailable`) and types Zmail does not know, which RFC 8620
    /// (section 3.6.2) says to treat as `serverFail`, may pass later.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self.error_type.as_str(),
            // EmailSubmission's own (RFC 8621, section 7.5).
            "invalidEmail"
                | "tooManyRecipients"
                | "noRecipients"
                | "invalidRecipients"
                | "forbiddenMailFrom"
                | "forbiddenFrom"
                | "forbiddenToSend"
                // Any create's and any method's (RFC 8620, sections 3.6.2
                // and 5.3); Apache James also gives `invalidArguments` for
                // an address it cannot parse.
                | "forbidden"
                | "overQuota"
                | "tooLarge"
                | "invalidProperties"
                | "invalidArguments"
                | "invalidResultReference"
                | "unknownMethod"
                | "accountNotFound"
                | "accountNotSupportedByMethod"
                | "accountReadOnly"
        )
    }
}

impl std::fmt::Display for SubmissionRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match (&self.description, self.error_type.as_str()) {
            (Some(description), _) => description.as_str(),
            (None, "") => "no reason given",
            (None, error_type) => error_type,
        };
        write!(f, "the JMAP server refused the submission: {reason}")
    }
}

impl std::error::Error for SubmissionRefused {}

/// Why a submission did not go through.
enum SubmitError {
    /// The server said it did not create it.
    Refused(SubmissionRefused),
    /// No answer that says: the server may have accepted it, which the
    /// copy's keywords then show.
    Unknown(anyhow::Error),
}

/// Submits the copy `email_id` ([`submit_email`]). When the server
/// refuses, the copy is destroyed, as it would otherwise sit in Sent as a
/// message that was never sent; one that cannot be destroyed is named in
/// the refusal ([`SubmissionRefused::unsent_copy`]). Any other failure
/// leaves the copy alone: the submission may have happened, and only its
/// keywords can say.
async fn submit_or_discard(
    client: &Connection,
    email_id: &str,
    identity_id: &str,
    message: &OutgoingMessage,
) -> Result<()> {
    match submit_email(client, email_id, identity_id, message).await {
        Ok(()) => Ok(()),
        Err(SubmitError::Refused(mut refused)) => {
            if destroy_emails(client, &[email_id]).await.is_err() {
                refused.unsent_copy = Some(email_id.to_owned());
            }
            Err(refused.into())
        }
        Err(SubmitError::Unknown(error)) => Err(error),
    }
}

/// Submits the imported email `email_id` (`EmailSubmission/set`). On
/// success the server swaps the copy's [`SUBMIT_PENDING`] for
/// [`SUBMITTED`] in the same call (`onSuccessUpdateEmail`), so the copy
/// says whether the submission happened.
async fn submit_email(
    client: &Connection,
    email_id: &str,
    identity_id: &str,
    message: &OutgoingMessage,
) -> std::result::Result<(), SubmitError> {
    let (request, create_id) =
        submission_request(client.account_id(), email_id, identity_id, message)
            .map_err(SubmitError::Unknown)?;
    // Read as plain JSON: the server's implicit `Email/set` answer follows
    // the submission's, under the same call id; the first is the
    // submission's.
    let response = client.send_first(&request).await.map_err(|error| {
        SubmitError::Unknown(anyhow!(
            "failed to create the JMAP EmailSubmission: {error}"
        ))
    })?;
    submission_created(response.as_ref(), &create_id)
}

/// The `EmailSubmission/set` that submits the email `email_id` from the
/// identity `identity_id` to `message`'s recipients and, on success,
/// replaces the copy's keywords with [`SUBMITTED`]
/// (`onSuccessUpdateEmail`); with the submission's creation id.
fn submission_request(
    account_id: &str,
    email_id: &str,
    identity_id: &str,
    message: &OutgoingMessage,
) -> Result<(jmap_wire::Request, String)> {
    use jmap_wire::mail::{EmailSubmission, Envelope, NewEmailSubmission};

    let mut set = jmap_wire::Set::<EmailSubmission>::new(account_id);
    let create_id = set.create(NewEmailSubmission::new(identity_id, email_id).envelope(
        Envelope::new(message.from_email.as_str(), all_recipients(message)),
    ));
    set.arguments().on_success_update_email(
        format!("#{create_id}"),
        jmap_wire::PatchObject::new().set("keywords", serde_json::json!({ SUBMITTED: true })),
    );
    let mut request = jmap_wire::Request::new();
    request.call(set)?;
    Ok((request, create_id))
}

/// Whether an `EmailSubmission/set` answer created submission `create_id`,
/// or why not.
fn submission_created(
    method_response: Option<&serde_json::Value>,
    create_id: &str,
) -> std::result::Result<(), SubmitError> {
    let Some([name, arguments, _]) = method_response
        .and_then(serde_json::Value::as_array)
        .and_then(|call| <&[serde_json::Value; 3]>::try_from(call.as_slice()).ok())
    else {
        return Err(SubmitError::Unknown(anyhow!(
            "the JMAP server answered EmailSubmission/set with no method response"
        )));
    };
    match name.as_str() {
        Some("EmailSubmission/set") => {}
        // A method error leaves the server as it was, but for a partial
        // failure, which says nothing about this submission.
        Some("error") if arguments["type"] != "serverPartialFail" => {
            return Err(SubmitError::Refused(SubmissionRefused::from_error(
                arguments,
            )));
        }
        _ => {
            return Err(SubmitError::Unknown(anyhow!(
                "the JMAP server answered EmailSubmission/set with {}: {}",
                name,
                identities::refusal_reason(arguments)
            )));
        }
    }
    if arguments
        .get("created")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|created| created.contains_key(create_id))
    {
        return Ok(());
    }
    match arguments
        .get("notCreated")
        .and_then(|refused| refused.get(create_id))
    {
        Some(error) => Err(SubmitError::Refused(SubmissionRefused::from_error(error))),
        None => Err(SubmitError::Unknown(anyhow!(
            "the JMAP server did not say whether it accepted the submission"
        ))),
    }
}

/// Destroys the emails `ids` (`Email/set`). One the server no longer has
/// counts as destroyed.
async fn destroy_emails(client: &Connection, ids: &[&str]) -> Result<()> {
    let response = client
        .send_first(&destroy_emails_request(client.account_id(), ids)?)
        .await
        .map_err(|error| anyhow!("failed to destroy the JMAP email: {error}"))?;
    let arguments = identities::method_arguments(response.as_ref(), "Email/set")?;
    for id in ids {
        let destroyed = arguments
            .get("destroyed")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|destroyed| destroyed.iter().any(|done| done.as_str() == Some(id)));
        let refusal = arguments
            .get("notDestroyed")
            .and_then(|refused| refused.get(id));
        let gone = refusal.is_some_and(|error| error["type"] == "notFound");
        if !destroyed && !gone {
            bail!(
                "the JMAP server did not destroy email {id}: {}",
                refusal.map_or("no reason given", identities::refusal_reason)
            );
        }
    }
    Ok(())
}

/// The `Email/set` that destroys the emails `ids`.
fn destroy_emails_request(account_id: &str, ids: &[&str]) -> Result<jmap_wire::Request> {
    let mut set = jmap_wire::Set::<jmap_wire::mail::Email>::new(account_id);
    set.destroy(ids.iter().copied());
    let mut request = jmap_wire::Request::new();
    request.call(set)?;
    Ok(request)
}

/// What a JMAP account shows of a message whose send was interrupted
/// (Zmail stopped mid-send), found by its Message-ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionTrace {
    /// Its copy says the server accepted the submission.
    Submitted,
    /// Its copy was imported and never submitted: the copy's id.
    Imported(String),
    /// A copy that says neither, imported before Zmail marked copies (or
    /// by a server that dropped the marks): whether it went is unknown.
    Unknown,
    /// No copy: the send stopped before importing it, so nothing went.
    None,
}

/// Looks for the copy of the message an interrupted send was sending:
/// emails received since shortly before `claimed_at` (Unix seconds, when
/// the attempt began; every email when unknown) with its Message-ID, and
/// their submission keywords. An error means the lookup failed, which
/// says nothing about the message.
pub async fn find_submitted_message(
    endpoint: &str,
    username: &str,
    secret: &str,
    auth: AuthKind,
    message_id: &str,
    claimed_at: Option<i64>,
) -> Result<SubmissionTrace> {
    let message_id = bare_message_id(message_id)
        .context("an outgoing message without a Message-ID cannot be looked up")?;
    let client = connect_client(endpoint, username, secret, auth).await?;
    let copies = find_copies(&client, &message_id, claimed_at).await?;

    if copies.iter().any(|copy| copy.submitted) {
        return Ok(SubmissionTrace::Submitted);
    }
    if copies.iter().any(|copy| !copy.pending) {
        return Ok(SubmissionTrace::Unknown);
    }
    Ok(copies.first().map_or(SubmissionTrace::None, |copy| {
        SubmissionTrace::Imported(copy.id.clone())
    }))
}

/// An email with an outgoing message's Message-ID, and the submission
/// keywords it has.
struct SentCopy {
    id: String,
    /// Marked [`SUBMITTED`].
    submitted: bool,
    /// Marked [`SUBMIT_PENDING`].
    pending: bool,
}

/// The emails received since shortly before `since` (Unix seconds; every
/// email when unknown) whose Message-ID is `message_id` (bare).
async fn find_copies(
    client: &Connection,
    message_id: &str,
    since: Option<i64>,
) -> Result<Vec<SentCopy>> {
    let mut ids = Vec::new();
    let mut position = 0;
    loop {
        let response = client
            .call(copies_query(client.account_id(), since, position))
            .await
            .map_err(wording::reported)
            .context("failed to look for the sent message")?;
        let batch = response.ids;
        if batch.is_empty() {
            break;
        }
        position += batch.len() as i64;
        ids.extend(batch);
    }

    let mut copies = Vec::new();
    let mut batches = batches::GetBatches::new(client, "email's Message-ID");
    let mut pending = ids.as_slice();
    while !pending.is_empty() {
        let response = batches
            .next(&mut pending, |batch| {
                client.call(copies_get(client.account_id(), batch))
            })
            .await
            .context("failed to look for the sent message")?;
        copies.extend(response.list.into_iter().filter_map(|email| {
            let matches = email.message_id.as_ref().is_some_and(|ids| {
                ids.iter()
                    .any(|id| bare_message_id(id).as_deref() == Some(message_id))
            });
            let id = email.id.clone().filter(|_| matches)?;
            let has = |keyword: &str| email.has_keyword(keyword);
            Some(SentCopy {
                id,
                submitted: has(SUBMITTED),
                pending: has(SUBMIT_PENDING),
            })
        }));
    }
    Ok(copies)
}

/// The `Email/query` for one page of the emails received since shortly
/// before `since`, newest first.
fn copies_query(account_id: &str, since: Option<i64>, position: i64) -> Query<Email> {
    let mut query = Query::<Email>::new(account_id);
    if let Some(after) = since.and_then(|since| utc_date(since - SUBMISSION_CLOCK_SLACK)) {
        query = query.filter(EmailFilterCondition::new().after(after));
    }
    query
        .sort([Comparator::new("receivedAt").descending()])
        .position(position)
        .limit(QUERY_PAGE_SIZE as u64)
}

/// The `Email/get` of the Message-ID and keywords of the emails `ids`.
fn copies_get(account_id: &str, ids: &[String]) -> Get<Email> {
    Get::<Email>::new(account_id).ids(ids).properties([
        EmailProperty::Id,
        EmailProperty::MessageId,
        EmailProperty::Keywords,
    ])
}

/// A time as the `UTCDate` of a filter: `2026-06-01T00:00:00Z`.
fn utc_date(seconds: i64) -> Option<UtcDate> {
    zmail_core::time::Stamp::from_seconds(seconds).map(|stamp| UtcDate::new(stamp.format_utc()))
}

/// Submits the copy an interrupted send imported and never submitted
/// ([`SubmissionTrace::Imported`]), as [`submit_outgoing_message`] would
/// have, destroying it if the server refuses.
pub async fn submit_imported_message(
    endpoint: &str,
    username: &str,
    secret: &str,
    auth: AuthKind,
    identity_email: &str,
    email_id: &str,
    message: &OutgoingMessage,
) -> Result<()> {
    message.check_addresses()?;
    let client = connect_client(endpoint, username, secret, auth).await?;
    let identity_id = resolve_identity_id(&client, identity_email)
        .await?
        .with_context(|| format!("no JMAP identity matches {identity_email}"))?;
    submit_or_discard(&client, email_id, &identity_id, message).await
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JmapCheckpoint {
    mailbox_query_state: Option<String>,
    email_query_state: Option<String>,
    /// `Email` state of the mirror, the `sinceState` of the next
    /// `Email/changes`.
    #[serde(default)]
    email_state: Option<String>,
    /// Server mailbox ids and the names the mirror stores them under.
    #[serde(default)]
    mailbox_ids: Option<BTreeMap<String, String>>,
}

#[derive(Debug)]
struct JmapQueryResult {
    ids: Vec<String>,
    query_state: String,
}

#[derive(Debug)]
pub(crate) struct JmapMailbox {
    id: String,
    name: String,
    role: Option<String>,
    sort_order: u32,
    /// The parent mailbox's name (`parentId` resolved), if nested.
    parent: Option<String>,
}

#[derive(Debug)]
pub(crate) struct JmapEmail {
    id: String,
    thread_id: String,
    mailbox_ids: Vec<String>,
    from: String,
    reply_to: Option<String>,
    recipients: String,
    subject: String,
    preview: String,
    body_text: Option<String>,
    sanitized_html: Option<String>,
    raw_html: Option<String>,
    received_at: String,
    unread: bool,
    flagged: bool,
    has_attachments: bool,
    has_remote_content: bool,
    identity_email: String,
    attachments: Vec<MessageAttachment>,
    headers: MessageHeaders,
}

#[derive(Debug)]
struct SubmissionIdentity {
    id: String,
    email: String,
}

impl JmapCheckpoint {
    fn from_json(value: Option<&str>) -> Result<Self> {
        match value {
            Some(value) if !value.trim().is_empty() => {
                serde_json::from_str(value).context("failed to decode the JMAP sync checkpoint")
            }
            _ => Ok(Self::default()),
        }
    }

    fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).context("failed to encode the JMAP sync checkpoint")
    }
}

/// Builds the session credentials for the target's auth kind.
///
/// Password accounts authenticate with HTTP Basic; OAuth2 accounts
/// authenticate with the bearer access token in the `Authorization` header
/// (RFC 7628 bearer usage), which `jmap-client` sends for `Bearer`
/// credentials.
fn jmap_credentials(
    auth: AuthKind,
    username: &str,
    secret: &str,
) -> Result<jmap_client::client::Credentials> {
    match auth {
        AuthKind::Password => Ok(jmap_client::client::Credentials::basic(username, secret)),
        AuthKind::OAuth2 => {
            // `jmap-client` unwraps the header it builds from the token, so
            // a token that is not a valid header value would panic there.
            if reqwest::header::HeaderValue::from_str(&format!("Bearer {secret}")).is_err() {
                bail!(
                    "the OAuth access token contains characters that cannot be sent to the \
                     server; sign in to the account again"
                );
            }
            Ok(jmap_client::client::Credentials::bearer(secret.to_owned()))
        }
        AuthKind::ProtonSession => bail!("Proton accounts do not sign in over JMAP"),
    }
}

/// Where RFC 8620 section 2.2 puts the session resource on a host.
const WELL_KNOWN_JMAP: &str = "/.well-known/jmap";
/// How many redirects from `/.well-known/jmap` are followed, as
/// `jmap-client` allows for the session itself.
const MAX_SESSION_REDIRECTS: usize = 5;
/// How long each request that follows those redirects may take, as
/// `jmap-client`'s default.
const SESSION_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The session URL for a stored endpoint.
///
/// Zmail stores three shapes: a bare origin (the live harness), a
/// `/.well-known/jmap` URL (typed in, or from the DNS SRV step) and the
/// session URL itself (the well-known step's URL after its redirects, or a
/// provider preset such as Fastmail's). A bare origin means its
/// `/.well-known/jmap`. That URL commonly redirects to the session on
/// another host (`example.com` to `api.example.com`), so its redirects are
/// followed here, without credentials and never from HTTPS to HTTP, as
/// autodiscovery does. Where they end is used only when it answers as a
/// JMAP server does without credentials (401, or the session as JSON), so
/// a redirect to a sign-in page is not mistaken for the session; the
/// credentials then go only to that host.
async fn session_url(endpoint: &str) -> Result<Url> {
    let mut url =
        Url::parse(endpoint).with_context(|| format!("invalid JMAP endpoint {endpoint}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("JMAP endpoints should be absolute HTTP or HTTPS URLs");
    }
    url.set_fragment(None);
    if url.query().is_none() && matches!(url.path().trim_end_matches('/'), "" | WELL_KNOWN_JMAP) {
        url.set_path(WELL_KNOWN_JMAP);
    }
    if url.path() != WELL_KNOWN_JMAP {
        return Ok(url);
    }

    let well_known = url.clone();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(SESSION_LOOKUP_TIMEOUT)
        .build()
        .context("failed to build the JMAP session lookup client")?;
    for _ in 0..=MAX_SESSION_REDIRECTS {
        let response = http
            .get(url.clone())
            .send()
            .await
            .with_context(|| format!("failed to connect to the JMAP endpoint {url}"))?;
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|location| location.to_str().ok());
        let Some(location) = location.filter(|_| response.status().is_redirection()) else {
            return Ok(if url == well_known || answers_as_jmap(&response) {
                url
            } else {
                well_known
            });
        };
        url = next_session_hop(&url, location)?;
    }
    bail!("the JMAP endpoint {endpoint} redirects too many times")
}

/// Whether a credential-less answer came from a JMAP session resource: it
/// asks for credentials, or returns the session (JSON).
fn answers_as_jmap(response: &reqwest::Response) -> bool {
    response.status() == reqwest::StatusCode::UNAUTHORIZED
        || (response.status().is_success()
            && response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.trim_start().starts_with("application/json")))
}

/// Where a redirect from `current` to `location` leads, refused when it
/// leaves HTTP(S) or goes from HTTPS to HTTP.
fn next_session_hop(current: &Url, location: &str) -> Result<Url> {
    let mut next = current
        .join(location)
        .with_context(|| format!("the JMAP endpoint {current} redirects to an invalid URL"))?;
    next.set_fragment(None);
    let allowed = match next.scheme() {
        "https" => true,
        "http" => current.scheme() == "http",
        _ => false,
    };
    if !allowed {
        bail!("the JMAP endpoint {current} redirects to {next}, which is not HTTPS");
    }
    Ok(next)
}

/// What `jmap-client` 0.4.1's `connect` is given to fetch `session`.
///
/// It always appends `/.well-known/jmap` to the URL it is given and offers
/// no way to supply a session fetched elsewhere, so a well-known URL loses
/// that suffix, and any other session URL gets an empty fragment: the
/// appended path then lands in the fragment, which is never sent, and
/// the request goes to `session` exactly.
fn jmap_client_url(session: &Url) -> String {
    let url = session.as_str();
    match url.strip_suffix(WELL_KNOWN_JMAP) {
        Some(base) if session.query().is_none() && session.fragment().is_none() => base.to_owned(),
        _ => format!("{url}#"),
    }
}

/// How long each API request may take, as `jmap-client`'s default was.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long one upload or download of a blob may take. A whole message
/// or an attachment of up to [`MAX_ATTACHMENT_BYTES`] does not fit the
/// ten seconds of an API request on a slow line, which is what
/// `jmap-client` gave it: the part was then skipped, or the send failed.
const BLOB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// The media type the Sent copy is uploaded as, as `jmap-client` sent
/// it. `Email/import` reads the blob as a message whatever its label.
const UPLOAD_CONTENT_TYPE: &str = "application/octet-stream";

/// One sign-in to a JMAP account, while Zmail moves from `jmap-client`
/// to its own JMAP library, `jmap-wire`, step by step (ADR 0008, #132).
///
/// It holds both clients over one session: `jmap-client` fetches it, and
/// the `jmap-wire` client is given the same session, with the same
/// credentials, the same-host redirect rule and the same timeout.
/// Every API call and every blob goes through `jmap-wire`: identities,
/// submission and destroying emails ([`Connection::send_first`]), the
/// reads, keyword and mailbox changes and `Email/import`
/// ([`Connection::call`]), the Sent copy's upload
/// ([`Connection::upload`]) and attachment downloads
/// ([`Connection::download`]). Only push still calls `jmap-client`,
/// which a `Connection` dereferences to, and the session fetch at
/// sign-in.
pub(crate) struct Connection {
    legacy: Client,
    wire: jmap_wire::Client<jmap_wire::ReqwestTransport>,
}

impl std::ops::Deref for Connection {
    type Target = Client;

    fn deref(&self) -> &Client {
        &self.legacy
    }
}

impl Connection {
    /// The account every call is made in: the one `jmap-client` uses, so
    /// that the calls on either client act on the same account.
    pub(crate) fn account_id(&self) -> &str {
        self.legacy.default_account_id()
    }

    /// The session as `jmap-wire` holds it, for the limits it states.
    pub(crate) fn wire_session(&self) -> &jmap_wire::Session {
        self.wire.session()
    }

    /// Sends one typed call in a request of its own and returns its
    /// answer. A method-level error is [`jmap_wire::Error::Method`].
    pub(crate) async fn call<M: jmap_wire::Method>(
        &self,
        method: M,
    ) -> jmap_wire::Result<M::Response> {
        self.wire.call(method).await
    }

    /// Uploads a message as a blob of the account (RFC 8620 section
    /// 6.1), within [`BLOB_TIMEOUT`]. One larger than the session's
    /// `maxSizeUpload` is refused before it is sent.
    pub(crate) async fn upload(
        &self,
        message: Vec<u8>,
    ) -> jmap_wire::Result<jmap_wire::UploadedBlob> {
        self.wire
            .upload(self.account_id(), UPLOAD_CONTENT_TYPE, message)
            .await
    }

    /// Downloads an attachment's blob (RFC 8620 section 6.2), within
    /// [`BLOB_TIMEOUT`] and never reading more than
    /// [`MAX_ATTACHMENT_BYTES`]: a larger answer is an error, not held
    /// in memory. The name and type in the URL are the placeholders
    /// `jmap-client` sent; they only label the server's answer.
    pub(crate) async fn download(&self, blob_id: &str) -> jmap_wire::Result<Vec<u8>> {
        let download = BlobDownload::new(self.account_id(), blob_id)
            .name("none")
            .max_bytes(MAX_ATTACHMENT_BYTES);
        Ok(self.wire.download(&download).await?.bytes)
    }

    /// Sends `request` and returns the first method response as JSON
    /// (`[name, arguments, call id]`), or `None` when the server gave
    /// none. A method-level error is such a response, named `error`.
    pub(crate) async fn send_first(
        &self,
        request: &jmap_wire::Request,
    ) -> jmap_wire::Result<Option<serde_json::Value>> {
        let response = self.wire.send(request).await?;
        Ok(response
            .method_responses
            .first()
            .and_then(|first| serde_json::to_value(first).ok()))
    }
}

/// The `jmap-wire` credentials for the target's auth kind: HTTP Basic
/// for a password, the bearer access token for OAuth2.
fn wire_credentials(
    auth: AuthKind,
    username: &str,
    secret: &str,
) -> Result<jmap_wire::Credentials> {
    match auth {
        AuthKind::Password => Ok(jmap_wire::Credentials::basic(username, secret)),
        AuthKind::OAuth2 => Ok(jmap_wire::Credentials::bearer(secret)),
        AuthKind::ProtonSession => bail!("Proton accounts do not sign in over JMAP"),
    }
}

pub(crate) async fn connect_client(
    endpoint: &str,
    username: &str,
    secret: &str,
    auth: AuthKind,
) -> Result<Connection> {
    let credentials = jmap_credentials(auth, username, secret)?;
    let wire_credentials = wire_credentials(auth, username, secret)?;
    let session = session_url(endpoint).await?;
    let mut client_builder = Client::new().credentials(credentials);
    // Same-host redirects only: a session or API URL that moves to another
    // host would take the credentials with it.
    let mut redirects = jmap_wire::RedirectPolicy::none();
    if let Some(host) = session.host_str() {
        client_builder = client_builder.follow_redirects([host.to_owned()]);
        redirects = jmap_wire::RedirectPolicy::hosts([host]);
    }

    let legacy = client_builder
        .connect(&jmap_client_url(&session))
        .await
        .with_context(|| format!("failed to connect to the JMAP endpoint {endpoint}"))?;
    // One session fetch for both clients: `jmap-wire` is handed the one
    // `jmap-client` just read.
    let wire = serde_json::to_value(&*legacy.session())
        .and_then(serde_json::from_value::<jmap_wire::Session>)
        .map_err(anyhow::Error::from)
        .and_then(|wire_session| {
            Ok(jmap_wire::Client::builder()
                .credentials(wire_credentials)
                .redirect_policy(redirects)
                .timeout(REQUEST_TIMEOUT)
                .blob_timeout(BLOB_TIMEOUT)
                .with_session(jmap_wire::ReqwestTransport::new()?, session, wire_session)?)
        })
        .with_context(|| format!("failed to connect to the JMAP endpoint {endpoint}"))?;
    Ok(Connection { legacy, wire })
}

/// Fetches the JMAP session at `endpoint` with the credentials: the
/// add-account flow's connection test, before anything is stored.
pub async fn check_sign_in(
    endpoint: &str,
    username: &str,
    secret: &str,
    auth: AuthKind,
) -> Result<()> {
    connect_client(endpoint, username, secret, auth).await?;
    Ok(())
}

async fn fetch_submission_identities(client: &Connection) -> Result<Vec<SubmissionIdentity>> {
    // Read as plain JSON: the identities' ids and addresses are all a
    // submission needs.
    let response = client
        .send_first(&identities::get_request(client.account_id())?)
        .await
        .map_err(|error| anyhow!("failed to fetch JMAP identities for submission: {error}"))?;
    submission_identities(response.as_ref())
}

/// The identities in an `Identity/get` method response
/// (`[name, arguments, call id]`), or why the server gave none.
fn submission_identities(
    method_response: Option<&serde_json::Value>,
) -> Result<Vec<SubmissionIdentity>> {
    Ok(identities::identity_list(method_response)?
        .iter()
        .filter_map(|identity| {
            Some(SubmissionIdentity {
                id: identity.get("id")?.as_str()?.to_owned(),
                email: identity.get("email")?.as_str()?.to_owned(),
            })
        })
        .collect())
}

async fn fetch_mailboxes(client: &Connection) -> Result<(Vec<JmapMailbox>, String)> {
    let response = client
        .call(mailboxes_get(client.account_id()))
        .await
        .map_err(wording::reported)
        .context("failed to fetch JMAP mailboxes")?;
    let state = response.state.unwrap_or_default();

    let list = response.list;
    let names = list
        .iter()
        .filter_map(|mailbox| Some((mailbox.id.as_deref()?, mailbox.name.as_deref()?)))
        .collect::<HashMap<_, _>>();
    let mut mailboxes = list
        .iter()
        .filter_map(|mailbox| {
            Some(JmapMailbox {
                id: mailbox.id.clone()?,
                name: mailbox.name.clone()?,
                role: mailbox_role(mailbox.role.as_ref()),
                sort_order: mailbox.sort_order.unwrap_or(0),
                parent: mailbox
                    .parent_id
                    .as_deref()
                    .and_then(|parent_id| names.get(parent_id))
                    .map(|name| (*name).to_owned()),
            })
        })
        .collect::<Vec<_>>();
    mailboxes.sort_by_key(|mailbox| (mailbox.sort_order, mailbox.name.clone()));
    Ok((mailboxes, state))
}

/// The `Mailbox/get` of every mailbox of the account.
fn mailboxes_get(account_id: &str) -> Get<Mailbox> {
    Get::<Mailbox>::new(account_id)
}

async fn sent_mailbox_ids(client: &Connection) -> Result<Vec<String>> {
    let response = client
        .call(mailboxes_get(client.account_id()))
        .await
        .map_err(wording::reported)
        .context("failed to fetch JMAP mailboxes for submission")?;
    Ok(response
        .list
        .into_iter()
        .filter(|mailbox| mailbox_role(mailbox.role.as_ref()).as_deref() == Some("sent"))
        .filter_map(|mailbox| mailbox.id)
        .collect())
}

async fn resolve_identity_id(client: &Connection, identity_email: &str) -> Result<Option<String>> {
    let identities = fetch_submission_identities(client).await?;
    Ok(identities
        .into_iter()
        .find(|identity| identity.email.eq_ignore_ascii_case(identity_email))
        .map(|identity| identity.id))
}

async fn fetch_email_query(client: &Connection) -> Result<JmapQueryResult> {
    let mut ids = Vec::new();
    let mut position = 0;
    let mut query_state = None;

    loop {
        let response = client
            .call(sync_query(client.account_id(), position))
            .await
            .map_err(wording::reported)
            .context("failed to query JMAP emails")?;
        if query_state.is_none() {
            query_state = Some(response.query_state.unwrap_or_default());
        }
        let batch = response.ids;
        if batch.is_empty() {
            break;
        }
        position += batch.len() as i64;
        ids.extend(batch);
    }

    Ok(JmapQueryResult {
        ids,
        query_state: query_state.unwrap_or_default(),
    })
}

/// The `Email/query` for one page of every email of the account, oldest
/// first.
fn sync_query(account_id: &str, position: i64) -> Query<Email> {
    Query::<Email>::new(account_id)
        .sort([Comparator::new("receivedAt").ascending()])
        .position(position)
        .limit(QUERY_PAGE_SIZE as u64)
}

/// Searches the account on the server with `Email/query` and returns the
/// ids of the newest `max_hits` matching emails.
///
/// This is the fallback for mail whose body the local index does not hold.
/// Every operator has a JMAP filter: free text (`text`), `from`, `to`,
/// `subject`, `hasAttachment`, the `$seen` and `$flagged` keywords,
/// `inMailbox` (resolved by mailbox name or role), and `before`/`after`
/// at the start of the day in `now`'s zone; negation wraps a condition in
/// a `NOT` operator. The store re-checks the non-text clauses locally.
pub async fn search_emails(
    target: &SyncTarget,
    secret: &str,
    query: &zmail_core::search::SearchQuery,
    now: &zmail_core::time::ZonedDateTime,
    max_hits: usize,
) -> Result<Vec<zmail_core::ServerHitRef>> {
    if target.protocol != ProtocolKind::Jmap {
        bail!("JMAP adapter received a non-JMAP sync target");
    }
    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;
    let (mailboxes, _) = fetch_mailboxes(&client).await?;
    let Some(filter) = search_filter(query, &mailboxes, now) else {
        return Ok(Vec::new());
    };
    let response = client
        .call(search_query(client.account_id(), filter, max_hits))
        .await
        .map_err(wording::reported)
        .context("failed to search JMAP emails")?;
    Ok(response
        .ids
        .into_iter()
        .map(|id| zmail_core::ServerHitRef {
            mailbox_name: None,
            remote_id: id,
        })
        .collect())
}

/// The `Email/query` for the newest `max_hits` emails `filter` matches.
fn search_query(
    account_id: &str,
    filter: Filter<EmailFilterCondition>,
    max_hits: usize,
) -> Query<Email> {
    Query::<Email>::new(account_id)
        .filter(filter)
        .sort([Comparator::new("receivedAt").descending()])
        .limit(max_hits as u64)
}

/// Builds the `Email/query` filter for a query (see [`search_emails`]).
/// Returns `None` when the query cannot match anything (an `in:` naming no
/// mailbox).
fn search_filter(
    query: &zmail_core::search::SearchQuery,
    mailboxes: &[JmapMailbox],
    now: &zmail_core::time::ZonedDateTime,
) -> Option<Filter<EmailFilterCondition>> {
    use zmail_core::search::{SearchTerm, day_start_unix, mailbox_matches};

    let condition = EmailFilterCondition::new;
    let today = zmail_core::time::Date::of(now);
    let zone = now.time_zone();
    let mut conditions: Vec<Filter<EmailFilterCondition>> = Vec::new();
    for clause in &query.clauses {
        let filter: Filter<EmailFilterCondition> = match &clause.term {
            SearchTerm::Text(text)
            | SearchTerm::From(text)
            | SearchTerm::To(text)
            | SearchTerm::Subject(text) => {
                if !text.is_searchable() {
                    continue;
                }
                let value = text.value.clone();
                match &clause.term {
                    SearchTerm::Text(_) => condition().text(value),
                    SearchTerm::From(_) => condition().from(value),
                    SearchTerm::To(_) => condition().to(value),
                    _ => condition().subject(value),
                }
                .into()
            }
            SearchTerm::HasAttachment => condition().has_attachment(true).into(),
            SearchTerm::Unread => condition().not_keyword("$seen").into(),
            SearchTerm::Read => condition().has_keyword("$seen").into(),
            SearchTerm::Flagged => condition().has_keyword("$flagged").into(),
            SearchTerm::Mailbox(value) => {
                let ids = mailboxes
                    .iter()
                    .filter(|mailbox| {
                        mailbox_matches(value, &mailbox.name, mailbox.role.as_deref())
                    })
                    .map(|mailbox| condition().in_mailbox(mailbox.id.clone()).into())
                    .collect::<Vec<_>>();
                if ids.is_empty() {
                    if clause.negated {
                        continue;
                    }
                    return None;
                }
                Filter::or(ids)
            }
            SearchTerm::Before(spec) | SearchTerm::After(spec) => {
                let Some(boundary) = spec
                    .resolve(today)
                    .and_then(|day| day_start_unix(day, zone))
                    .and_then(utc_date)
                else {
                    continue;
                };
                if matches!(clause.term, SearchTerm::Before(_)) {
                    condition().before(boundary).into()
                } else {
                    condition().after(boundary).into()
                }
            }
        };
        conditions.push(if clause.negated {
            Filter::not([filter])
        } else {
            filter
        });
    }
    Some(Filter::and(conditions))
}

/// Fetches email details, downloading attachment blobs into the store's
/// attachments directory.
///
/// JMAP delivers bodies with the snapshot (there is no lazy hydration path),
/// so attachments download during sync as well. Each part resolves through
/// the email's `attachments` property and downloads over the session's
/// binary download URL ([`Connection::download`]). Failures skip the part
/// rather than failing the sync, mirroring the IMAP adapter's tolerance.
pub(crate) async fn fetch_emails(
    client: &Connection,
    ids: &[String],
    blobs: &BlobStore,
) -> Result<Vec<JmapEmail>> {
    let mut emails = Vec::with_capacity(ids.len());

    let mut batches = batches::GetBatches::new(client, "email with its body");
    let mut pending = ids;
    while !pending.is_empty() {
        let response = batches
            .next(&mut pending, |batch| {
                client.call(emails_get(client.account_id(), batch))
            })
            .await
            .context("failed to fetch JMAP email details")?;

        for email in response.list {
            let thread_id = email
                .thread_id
                .clone()
                .or_else(|| email.id.clone())
                .unwrap_or_else(|| "unknown-thread".to_owned());
            let identity_email = email
                .from
                .as_ref()
                .and_then(|addresses| addresses.first())
                .map(|address| address.email.clone())
                .unwrap_or_default();
            let id = email.id.clone().unwrap_or_default();

            let (body_text, sanitized_html, has_remote_content, hydrated_preview) =
                extract_renderable_bodies(&email);
            let attachments = download_attachments(client, &email, &id, blobs).await;
            emails.push(JmapEmail {
                id,
                thread_id,
                mailbox_ids: members(email.mailbox_ids.as_ref()),
                from: format_jmap_addresses(email.from.as_deref()),
                reply_to: match format_jmap_addresses(email.reply_to.as_deref()) {
                    value if value == "Unknown" => None,
                    value => Some(value),
                },
                recipients: format_jmap_addresses(email.to.as_deref()),
                subject: email
                    .subject
                    .as_deref()
                    .unwrap_or("(no subject)")
                    .to_owned(),
                preview: hydrated_preview
                    .or_else(|| email.preview.clone())
                    .unwrap_or_else(|| "No preview available.".to_owned()),
                body_text,
                sanitized_html,
                raw_html: raw_html_body(&email),
                received_at: email
                    .received_at
                    .as_ref()
                    .and_then(received_seconds)
                    .map(timestamp_to_rfc3339)
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
                unread: !has_keyword(&email, "$seen"),
                flagged: has_keyword(&email, "$flagged"),
                has_attachments: email.has_attachment.unwrap_or(false)
                    || attachments.iter().any(|attachment| !attachment.inline_only),
                has_remote_content,
                identity_email,
                attachments,
                headers: email_headers(&email),
            });
        }
    }

    Ok(emails)
}

/// The `Email/get` of the emails `ids` with everything a sync stores:
/// the metadata, the header fields, the body parts and the text of every
/// text part.
fn emails_get(account_id: &str, ids: &[String]) -> Get<Email> {
    let mut properties = vec![
        EmailProperty::Id,
        EmailProperty::ThreadId,
        EmailProperty::MailboxIds,
        EmailProperty::Keywords,
        EmailProperty::From,
        EmailProperty::ReplyTo,
        EmailProperty::To,
        EmailProperty::Subject,
        EmailProperty::Preview,
        EmailProperty::HasAttachment,
        EmailProperty::Attachments,
        EmailProperty::BodyValues,
        EmailProperty::TextBody,
        EmailProperty::HtmlBody,
        EmailProperty::ReceivedAt,
    ];
    properties.extend(header_properties());
    Get::<Email>::new(account_id)
        .ids(ids)
        .properties(properties)
        .fetch_all_body_values(true)
}

/// The members of a JMAP set (ids or keywords mapped to `true`).
pub(crate) fn members(set: Option<&BTreeMap<String, bool>>) -> Vec<String> {
    set.into_iter()
        .flatten()
        .filter(|(_, member)| **member)
        .map(|(name, _)| name.clone())
        .collect()
}

/// Whether the email has `keyword`, written exactly so: servers return
/// keywords in lower case (RFC 8621 section 4.1.1).
pub(crate) fn has_keyword(email: &EmailRecord, keyword: &str) -> bool {
    email
        .keywords
        .as_ref()
        .and_then(|keywords| keywords.get(keyword))
        .copied()
        .unwrap_or(false)
}

/// A `receivedAt` as Unix seconds, when it is a date.
fn received_seconds(date: &UtcDate) -> Option<i64> {
    zmail_core::time::parse_timestamp(date.as_str()).map(zmail_core::time::Stamp::seconds)
}

/// The `Email/get` properties [`email_headers`] reads: the threading ids,
/// Cc, and the raw `Delivered-To` and `X-Original-To` header fields.
fn header_properties() -> Vec<EmailProperty> {
    vec![
        EmailProperty::MessageId,
        EmailProperty::InReplyTo,
        EmailProperty::References,
        EmailProperty::Cc,
        HeaderProperty::raw(DELIVERED_TO).all().into(),
        HeaderProperty::raw(ORIGINAL_TO).all().into(),
    ]
}

const DELIVERED_TO: &str = "Delivered-To";
const ORIGINAL_TO: &str = "X-Original-To";

/// The headers a reply is worked out from, from an email fetched with
/// [`header_properties`].
fn email_headers(email: &EmailRecord) -> MessageHeaders {
    let ids = |values: Option<&Vec<String>>| -> Vec<String> {
        values
            .into_iter()
            .flatten()
            .filter_map(|id| bare_message_id(id))
            .collect()
    };
    let mut delivered_to: Vec<String> = Vec::new();
    for name in [DELIVERED_TO, ORIGINAL_TO] {
        for value in email.header_all::<AsRaw>(name).unwrap_or_default() {
            let address = value.trim().trim_start_matches('<').trim_end_matches('>');
            if address.contains('@')
                && !address.contains(char::is_whitespace)
                && !delivered_to
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(address))
            {
                delivered_to.push(address.to_owned());
            }
        }
    }
    MessageHeaders {
        captured: true,
        message_id: ids(email.message_id.as_ref()).into_iter().next(),
        in_reply_to: ids(email.in_reply_to.as_ref()),
        references: ids(email.references.as_ref()),
        cc: match format_jmap_addresses(email.cc.as_deref()) {
            value if value == "Unknown" => String::new(),
            value => value,
        },
        delivered_to,
    }
}

/// Fetches the reply headers of emails stored before Zmail kept them
/// (see `LocalStore::messages_missing_bodies`): one `Email/get` of
/// [`header_properties`] per page of ids. Emails the server no longer has
/// are left out.
pub async fn fetch_message_headers(
    target: &SyncTarget,
    secret: &str,
    ids: &[String],
) -> Result<Vec<(String, MessageHeaders)>> {
    if target.protocol != ProtocolKind::Jmap {
        bail!("JMAP adapter received a non-JMAP sync target");
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;
    let mut headers = Vec::with_capacity(ids.len());
    let mut batches = batches::GetBatches::new(&client, "email's headers");
    let mut pending = ids;
    while !pending.is_empty() {
        let response = batches
            .next(&mut pending, |batch| {
                client.call(headers_get(client.account_id(), batch))
            })
            .await
            .context("failed to fetch JMAP email headers")?;
        for email in response.list {
            if let Some(id) = &email.id {
                headers.push((id.clone(), email_headers(&email)));
            }
        }
    }
    Ok(headers)
}

/// The `Email/get` of the reply headers of the emails `ids`.
fn headers_get(account_id: &str, ids: &[String]) -> Get<Email> {
    let mut properties = vec![EmailProperty::Id];
    properties.extend(header_properties());
    Get::<Email>::new(account_id)
        .ids(ids)
        .properties(properties)
}

/// Downloads one email's attachment parts into the blob directory.
///
/// Parts without a blob id or a name, oversized parts, and failed downloads
/// are skipped so a single bad part never fails the account sync.
async fn download_attachments(
    client: &Connection,
    email: &EmailRecord,
    email_id: &str,
    blobs: &BlobStore,
) -> Vec<MessageAttachment> {
    let mut attachments = Vec::new();
    let Some(parts) = email.attachments.as_deref() else {
        return attachments;
    };
    if parts.is_empty() {
        return attachments;
    }

    for (index, part) in parts.iter().enumerate() {
        if attachments.len() >= MAX_ATTACHMENTS_PER_MESSAGE {
            break;
        }
        let Some(blob_id) = part.blob_id.as_deref() else {
            continue;
        };
        if part.size.unwrap_or(0) > MAX_ATTACHMENT_BYTES as u64 {
            continue;
        }
        let content_id = part
            .cid
            .as_deref()
            .map(|content_id| {
                content_id
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .trim()
                    .to_owned()
            })
            .filter(|content_id| !content_id.is_empty());
        // A part without a name is kept only for its Content-ID, so HTML
        // can show it through `cid:` (ADR 0006); it is not listed.
        let named = part.name.clone().filter(|name| !name.trim().is_empty());
        let inline_only = named.is_none();
        let Some(filename) = named.or_else(|| content_id.clone()) else {
            continue;
        };
        // A blob the server does not have, refuses or sends more of
        // than a part may be is skipped.
        let Ok(contents) = client.download(blob_id).await else {
            continue;
        };
        let storage_name = format!(
            "incoming-jmap-{email_id}-{index}-{}",
            sanitize_component(&filename)
        );
        // The blob store encrypts the part (ADR 0005); a failed write skips
        // it rather than failing the sync.
        let Ok(storage_path) = blobs.write(&storage_name, &contents) else {
            continue;
        };
        attachments.push(MessageAttachment {
            filename,
            content_type: part
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_owned()),
            size_bytes: contents.len() as u64,
            storage_path,
            content_id,
            inline_only,
        });
    }

    attachments
}

/// Restricts a filename-derived path component to safe characters.
fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect()
}

fn group_mailboxes(
    mailbox_details: Vec<JmapMailbox>,
    emails: Vec<JmapEmail>,
    identities: &[NewIdentity],
    fallback_identity: &str,
) -> Vec<MailboxSnapshot> {
    let mut mailboxes = mailbox_details
        .into_iter()
        .map(|mailbox| {
            (
                mailbox.id.clone(),
                MailboxAccumulator {
                    name: mailbox.name,
                    role: mailbox.role,
                    parent: mailbox.parent,
                    sort_order: mailbox.sort_order,
                    threads: BTreeMap::new(),
                },
            )
        })
        .collect::<HashMap<_, _>>();

    let primary_identity = primary_identity(identities, fallback_identity);

    for email in emails {
        for mailbox_id in &email.mailbox_ids {
            let Some(mailbox) = mailboxes.get_mut(mailbox_id) else {
                continue;
            };
            let entry = mailbox
                .threads
                .entry(email.thread_id.clone())
                .or_insert_with(|| ThreadSnapshot {
                    remote_thread_id: email.thread_id.clone(),
                    subject: String::new(),
                    participants: String::new(),
                    preview: String::new(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 0,
                    latest_message_at: String::new(),
                    messages: Vec::new(),
                });
            entry.has_attachments |= email.has_attachments;
            entry
                .messages
                .push(message_snapshot(&email, primary_identity));
        }
    }

    let mut ordered = mailboxes
        .into_values()
        .map(|mailbox| {
            let mut threads = mailbox
                .threads
                .into_values()
                .map(|mut thread| {
                    changes::recompute_thread(&mut thread);
                    thread
                })
                .collect::<Vec<_>>();
            threads.sort_by(|left, right| right.latest_message_at.cmp(&left.latest_message_at));
            (
                mailbox.sort_order,
                MailboxSnapshot {
                    name: mailbox.name,
                    role: mailbox.role,
                    delimiter: None,
                    parent: mailbox.parent,
                    threads,
                },
            )
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.name.cmp(&right.1.name)));
    ordered.into_iter().map(|(_, mailbox)| mailbox).collect()
}

/// The identity address messages without a sender identity are filed
/// under: the primary identity, else the first, else the login.
fn primary_identity<'a>(identities: &'a [NewIdentity], fallback: &'a str) -> &'a str {
    identities
        .iter()
        .find(|identity| identity.is_primary)
        .or_else(|| identities.first())
        .map(|identity| identity.email_address.as_str())
        .unwrap_or(fallback)
}

/// Converts a fetched email into the stored message row.
pub(crate) fn message_snapshot(email: &JmapEmail, primary_identity: &str) -> MessageSnapshot {
    MessageSnapshot {
        remote_id: email.id.clone(),
        identity_email: if email.identity_email.is_empty() {
            primary_identity.to_owned()
        } else {
            email.identity_email.clone()
        },
        sender: email.from.clone(),
        reply_to: email.reply_to.clone(),
        recipients: email.recipients.clone(),
        subject: email.subject.clone(),
        body_preview: email.preview.clone(),
        body_text: email.body_text.clone(),
        sanitized_html: email.sanitized_html.clone(),
        raw_html: email.raw_html.clone(),
        has_remote_content: email.has_remote_content,
        is_read: !email.unread,
        is_flagged: email.flagged,
        received_at: email.received_at.clone(),
        attachments: email.attachments.clone(),
        headers: email.headers.clone(),
    }
}

#[derive(Debug)]
struct MailboxAccumulator {
    name: String,
    role: Option<String>,
    parent: Option<String>,
    sort_order: u32,
    threads: BTreeMap<String, ThreadSnapshot>,
}

/// Splits a JMAP email into the stored body fields.
///
/// RFC 8621 section 4.1.4 lists the `text/html` part in `textBody` when a
/// message has no `text/plain` alternative, and may list `text/plain` parts
/// in `htmlBody`. The list a part came from therefore says nothing about
/// its content: each part's `type` decides whether it is displayed as plain
/// text or goes through the sanitizer (issue #130, the JMAP twin of #112).
///
/// This mirrors the IMAP adapter's `hydrate_bodies`: a real `text/plain`
/// part is displayed when there is one, and for `multipart/alternative` the
/// HTML alternative still decides `has_remote_content`. Otherwise the HTML
/// part is stored sanitized and its remote references are flagged. HTML is
/// never stored as `body_text`, so it is never shown raw.
fn extract_renderable_bodies(
    email: &EmailRecord,
) -> (Option<String>, Option<String>, bool, Option<String>) {
    let text_parts = email.text_body.as_deref().unwrap_or_default();
    let html_parts = email.html_body.as_deref().unwrap_or_default();

    let text_body = body_value_of_type(text_parts.iter().chain(html_parts), email, "text/plain")
        .map(normalize_body_text)
        .filter(|value| !value.is_empty());
    let html_body = body_value_of_type(html_parts.iter().chain(text_parts), email, "text/html")
        .filter(|value| !value.trim().is_empty());

    if let Some(text_body) = text_body {
        // multipart/alternative: the plain-text alternative is displayed,
        // but the HTML alternative still decides whether the message
        // references remote content. Only the displayed alternative is
        // stored, matching the IMAP adapter.
        let has_remote_content = html_body
            .as_deref()
            .is_some_and(|html| sanitize_html_for_display(html).has_remote_content);
        let preview = excerpt(&text_body, 180);
        return (Some(text_body), None, has_remote_content, Some(preview));
    }

    if let Some(html_body) = html_body {
        let sanitized = sanitize_html_for_display(&html_body);
        let preview = excerpt(&sanitized.text, 180);
        return (
            None,
            Some(sanitized.html),
            sanitized.has_remote_content,
            Some(preview),
        );
    }

    (None, None, false, None)
}

/// The original `text/html` part, kept for HTML renderers (ADR 0006)
/// even when the message also has a plain-text part.
fn raw_html_body(email: &EmailRecord) -> Option<String> {
    let text_parts = email.text_body.as_deref().unwrap_or_default();
    let html_parts = email.html_body.as_deref().unwrap_or_default();
    body_value_of_type(html_parts.iter().chain(text_parts), email, "text/html")
        .filter(|value| !value.trim().is_empty())
}

/// Returns the fetched value of the first part whose media type is
/// `media_type`.
///
/// A part without a `type` is `text/plain`, the MIME default (RFC 2045
/// section 5.2). Parameters and letter case are ignored.
fn body_value_of_type<'a>(
    parts: impl IntoIterator<Item = &'a EmailBodyPart>,
    email: &EmailRecord,
    media_type: &str,
) -> Option<String> {
    parts
        .into_iter()
        .filter(|part| {
            let content_type = part.content_type.as_deref().unwrap_or("text/plain");
            content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case(media_type)
        })
        .find_map(|part| {
            let part_id = part.part_id.as_deref()?;
            email.body_value(part_id)?.value.clone()
        })
}

fn normalize_body_text(text: String) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn excerpt(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else if preview.is_empty() {
        "No preview available.".to_owned()
    } else {
        preview
    }
}

fn format_jmap_addresses(addresses: Option<&[EmailAddress]>) -> String {
    match addresses {
        Some(addresses) if !addresses.is_empty() => addresses
            .iter()
            .map(|address| match address.name.as_deref() {
                Some(name) if !name.trim().is_empty() => {
                    format!("{name} <{}>", address.email)
                }
                _ => address.email.clone(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => "Unknown".to_owned(),
    }
}

/// Every recipient's bare address, for the submission envelope.
fn all_recipients(message: &OutgoingMessage) -> Vec<String> {
    message
        .to
        .iter()
        .chain(message.cc.iter())
        .chain(message.bcc.iter())
        .map(|recipient| zmail_core::participants::envelope_address(recipient))
        .collect()
}

/// A mailbox's role as the mirror stores it: in lower case, whatever
/// case the server wrote it in.
fn mailbox_role(role: Option<&jmap_wire::mail::Role>) -> Option<String> {
    role.map(|role| role.as_str().to_ascii_lowercase())
}

fn timestamp_to_rfc3339(timestamp: i64) -> String {
    if let Some(stamp) = zmail_core::time::Stamp::from_seconds(timestamp) {
        stamp.format_rfc3339_utc()
    } else {
        "1970-01-01T00:00:00Z".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        JmapCheckpoint, JmapEmail, JmapMailbox, extract_renderable_bodies, format_jmap_addresses,
        group_mailboxes, jmap_credentials, raw_html_body, sanitize_component, timestamp_to_rfc3339,
    };
    use serde_json::json;
    use zmail_core::{AuthKind, threading::MessageHeaders};

    #[test]
    fn jmap_credentials_follow_the_auth_kind() {
        assert_eq!(
            jmap_credentials(AuthKind::Password, "user@example.test", "app-password")
                .expect("password credentials"),
            jmap_client::client::Credentials::basic("user@example.test", "app-password"),
            "password accounts authenticate with HTTP Basic"
        );
        assert_eq!(
            jmap_credentials(AuthKind::OAuth2, "user@example.test", "access-token")
                .expect("bearer credentials"),
            jmap_client::client::Credentials::bearer("access-token"),
            "OAuth2 accounts authenticate with a bearer token"
        );
        assert!(
            jmap_credentials(AuthKind::ProtonSession, "user@example.test", "").is_err(),
            "Proton sessions never reach JMAP"
        );
    }

    #[test]
    fn attachment_filenames_sanitize_to_safe_components() {
        assert_eq!(sanitize_component("report.txt"), "report.txt");
        assert_eq!(sanitize_component("a b/c:d.txt"), "a_b_c_d.txt");
        assert_eq!(sanitize_component("../../etc/passwd"), ".._.._etc_passwd");
    }

    #[test]
    fn search_filters_map_every_operator() {
        let mailboxes = vec![
            super::JmapMailbox {
                id: "mb-inbox".to_owned(),
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                sort_order: 0,
                parent: None,
            },
            super::JmapMailbox {
                id: "mb-junk".to_owned(),
                name: "Bulk".to_owned(),
                role: Some("junk".to_owned()),
                sort_order: 1,
                parent: None,
            },
        ];
        let now = zmail_core::time::clock_at("2026-06-15T12:00:00Z").expect("valid time");
        let filter = |query: &str| {
            super::search_filter(
                &zmail_core::search::SearchQuery::parse(query),
                &mailboxes,
                &now,
            )
            .map(|filter| serde_json::to_value(filter).expect("filters serialise"))
        };
        assert_eq!(
            filter(
                "budget from:alice -is:unread has:attachment -in:spam after:2026-06-01 \
                 is:flagged"
            ),
            Some(serde_json::json!({
                "operator": "AND",
                "conditions": [
                    {"text": "budget"},
                    {"from": "alice"},
                    {"operator": "NOT", "conditions": [{"notKeyword": "$seen"}]},
                    {"hasAttachment": true},
                    {"operator": "NOT", "conditions": [
                        {"operator": "OR", "conditions": [{"inMailbox": "mb-junk"}]}
                    ]},
                    {"after": "2026-06-01T00:00:00Z"},
                    {"hasKeyword": "$flagged"},
                ],
            }))
        );
        // `in:` naming no mailbox matches nothing; negated, it narrows nothing.
        assert_eq!(filter("budget in:nowhere"), None);
        assert_eq!(
            filter("budget -in:nowhere"),
            Some(serde_json::json!({
                "operator": "AND",
                "conditions": [{"text": "budget"}],
            }))
        );
    }

    #[test]
    fn checkpoint_round_trip_is_stable() {
        let checkpoint = JmapCheckpoint {
            mailbox_query_state: Some("m-1".to_owned()),
            email_query_state: Some("e-4".to_owned()),
            email_state: Some("s-9".to_owned()),
            mailbox_ids: Some(std::collections::BTreeMap::from([(
                "m1".to_owned(),
                "Inbox".to_owned(),
            )])),
        };
        let json = checkpoint.to_json().expect("checkpoint should serialize");
        let parsed = JmapCheckpoint::from_json(Some(&json)).expect("checkpoint should parse");
        assert_eq!(parsed.mailbox_query_state.as_deref(), Some("m-1"));
        assert_eq!(parsed.email_query_state.as_deref(), Some("e-4"));
        assert_eq!(parsed.email_state.as_deref(), Some("s-9"));
        assert_eq!(
            parsed.mailbox_ids.as_ref().and_then(|ids| ids.get("m1")),
            Some(&"Inbox".to_owned())
        );
    }

    #[test]
    fn checkpoints_from_before_email_changes_still_decode() {
        let parsed = JmapCheckpoint::from_json(Some(
            r#"{"mailbox_query_state":"m-1","email_query_state":"e-4"}"#,
        ))
        .expect("legacy checkpoint should parse");
        assert_eq!(
            parsed.email_state, None,
            "a legacy checkpoint runs a full sync"
        );
        assert_eq!(parsed.mailbox_ids, None);
    }

    #[test]
    fn timestamps_render_to_rfc3339() {
        assert!(timestamp_to_rfc3339(1_700_000_000).contains('T'));
        // Characterisation (issue #489): `received_at` as it was written
        // before the workspace moved off chrono.
        let cases: &[(i64, &str)] = &[
            (0, "1970-01-01T00:00:00+00:00"),
            (-1, "1969-12-31T23:59:59+00:00"),
            (951_782_400, "2000-02-29T00:00:00+00:00"),
            (1_700_000_000, "2023-11-14T22:13:20+00:00"),
            (1_792_060_920, "2026-10-15T10:42:00+00:00"),
            (253_402_300_799, "9999-12-31T23:59:59+00:00"),
            (253_402_300_800, "+10000-01-01T00:00:00+00:00"),
            (-62_167_219_200, "0000-01-01T00:00:00+00:00"),
            (-62_167_219_201, "-0001-12-31T23:59:59+00:00"),
            (8_210_266_876_799, "+262142-12-31T23:59:59+00:00"),
            (8_210_266_876_800, "1970-01-01T00:00:00Z"),
            (i64::MAX, "1970-01-01T00:00:00Z"),
            (i64::MIN, "1970-01-01T00:00:00Z"),
        ];
        for (seconds, expected) in cases {
            assert_eq!(timestamp_to_rfc3339(*seconds), *expected, "{seconds}");
        }
    }

    /// Same newsletter as the IMAP adapter's HTML-only fixture, so both
    /// adapters are held to the same stored fields.
    const NEWSLETTER_HTML: &str = concat!(
        "<html><body>",
        "<h1>Your new dashboard</h1>",
        "<p>Faster exports</p>",
        "<img src=\"https://tracker.example.test/open.gif\">",
        "<table><tr><td>Plan</td><td>Price</td></tr>",
        "<tr><td>Starter</td><td>EUR 9</td></tr>",
        "<tr><td>Team</td><td>EUR 29</td></tr></table>",
        "</body></html>",
    );

    /// Builds an Email/get result the way RFC 8621 servers shape it.
    fn email(
        text_body: serde_json::Value,
        html_body: serde_json::Value,
        body_values: serde_json::Value,
    ) -> jmap_wire::mail::EmailRecord {
        serde_json::from_value(json!({
            "id": "M1",
            "textBody": text_body,
            "htmlBody": html_body,
            "bodyValues": body_values,
        }))
        .expect("the Email/get fixture should deserialize")
    }

    #[test]
    fn submission_identities_are_read_from_plain_json() {
        // Shaped like Apache James's answer, whose extra and null members
        // jmap-client's typed response refused.
        let response = json!(["Identity/get", {
            "accountId": "a1",
            "state": null,
            "notFound": null,
            "list": [
                {"id": "i1", "name": "", "email": "jmap@localhost", "replyTo": null,
                 "bcc": null, "textSignature": "", "htmlSignature": "", "mayDelete": false,
                 "sortOrder": 100},
                {"id": "i2"}
            ]
        }, "s0"]);
        let identities = super::submission_identities(Some(&response)).expect("identities");
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].id, "i1");
        assert_eq!(identities[0].email, "jmap@localhost");

        let refused = json!(["error", {"type": "unknownMethod"}, "s0"]);
        let error = super::submission_identities(Some(&refused)).expect_err("refused");
        assert!(error.to_string().contains("unknownMethod"), "{error}");
        assert!(super::submission_identities(None).is_err());
    }

    #[test]
    fn reply_headers_come_from_the_email_properties() {
        let email: jmap_wire::mail::EmailRecord = serde_json::from_value(json!({
            "id": "M2",
            "messageId": ["reply-2@example.test"],
            "inReplyTo": ["reply-1@example.test"],
            "references": ["root@example.test", "reply-1@example.test"],
            "cc": [
                {"name": "Ada", "email": "ada@example.test"},
                {"name": null, "email": "bo@example.test"}
            ],
            "header:Delivered-To:all": ["me+lists@work.example", " <ME+lists@work.example>"],
            "header:X-Original-To:all": ["alias@work.example"],
        }))
        .expect("the Email/get fixture should deserialize");
        let headers = super::email_headers(&email);
        assert!(headers.captured);
        assert_eq!(headers.message_id.as_deref(), Some("reply-2@example.test"));
        assert_eq!(headers.in_reply_to, ["reply-1@example.test"]);
        assert_eq!(
            headers.references,
            ["root@example.test", "reply-1@example.test"]
        );
        assert_eq!(headers.cc, "Ada <ada@example.test>, bo@example.test");
        assert_eq!(
            headers.delivered_to,
            ["me+lists@work.example", "alias@work.example"]
        );

        // An email with none of them: read, and empty.
        let bare = super::email_headers(
            &serde_json::from_value(json!({"id": "M3", "messageId": null, "cc": null}))
                .expect("deserializes"),
        );
        assert!(bare.captured);
        assert_eq!(bare.message_id, None);
        assert!(bare.cc.is_empty() && bare.delivered_to.is_empty());

        // The property names sent to the server.
        let names = super::header_properties()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "messageId",
                "inReplyTo",
                "references",
                "cc",
                "header:Delivered-To:all",
                "header:X-Original-To:all"
            ]
        );
    }

    fn part(part_id: &str, content_type: &str) -> serde_json::Value {
        json!({ "partId": part_id, "type": content_type, "charset": "utf-8" })
    }

    fn value(text: &str) -> serde_json::Value {
        json!({ "value": text, "isEncodingProblem": false, "isTruncated": false })
    }

    #[test]
    fn html_only_bodies_listed_in_text_body_are_sanitized_and_flag_remote_content() {
        // RFC 8621 4.1.4: with no text/plain alternative the HTML part is
        // listed in textBody as well as htmlBody.
        let email = email(
            json!([part("1", "text/html")]),
            json!([part("1", "text/html")]),
            json!({ "1": value(NEWSLETTER_HTML) }),
        );
        let (body_text, sanitized_html, has_remote_content, preview) =
            extract_renderable_bodies(&email);

        assert!(
            body_text.is_none(),
            "HTML must never be stored as plain text and shown raw"
        );
        let sanitized_html = sanitized_html.expect("the HTML body should be stored sanitized");
        assert!(sanitized_html.contains("<img src=\"zmail-blocked:0\">"));
        assert!(!sanitized_html.contains("tracker.example.test"));
        assert!(has_remote_content, "the remote image must be reported");
        assert_eq!(
            preview.as_deref(),
            Some("Your new dashboard Faster exports Plan Price Starter EUR 9 Team EUR 29")
        );
    }

    #[test]
    fn html_only_bodies_without_remote_references_are_not_flagged() {
        let email = email(
            json!([part("1", "text/html")]),
            json!([part("1", "text/html")]),
            json!({ "1": value("<p>Hello</p><p>No remote images here.</p>") }),
        );
        let (body_text, sanitized_html, has_remote_content, preview) =
            extract_renderable_bodies(&email);

        assert!(body_text.is_none());
        assert!(sanitized_html.is_some());
        assert!(!has_remote_content);
        assert_eq!(preview.as_deref(), Some("Hello No remote images here."));
    }

    #[test]
    fn html_only_bodies_render_as_clean_text() {
        let email = email(
            json!([part("1", "TEXT/HTML; charset=utf-8")]),
            json!([part("1", "TEXT/HTML; charset=utf-8")]),
            json!({
                "1": value(concat!(
                    "<div><b>Fish</b> &amp; <i>chips</i> &lt;today&gt;</div>",
                    "<p>Caf&#233; &quot;open&quot;</p>",
                )),
            }),
        );
        let (body_text, sanitized_html, has_remote_content, preview) =
            extract_renderable_bodies(&email);

        assert!(body_text.is_none());
        assert!(sanitized_html.is_some());
        assert!(!has_remote_content);
        let preview = preview.expect("an HTML body has a preview");
        assert_eq!(preview, "Fish & chips <today> Café \"open\"");
        assert!(!preview.contains("<div>") && !preview.contains("&amp;"));
    }

    #[test]
    fn the_original_html_part_is_kept_beside_the_text() {
        let alternative = email(
            json!([part("1", "text/plain")]),
            json!([part("2", "text/html")]),
            json!({
                "1": value("Hello in text."),
                "2": value(NEWSLETTER_HTML),
            }),
        );
        assert_eq!(
            raw_html_body(&alternative).as_deref(),
            Some(NEWSLETTER_HTML),
            "the HTML alternative is stored for HTML renderers"
        );
        let html_only = email(
            json!([part("1", "text/html")]),
            json!([part("1", "text/html")]),
            json!({ "1": value(NEWSLETTER_HTML) }),
        );
        assert_eq!(raw_html_body(&html_only).as_deref(), Some(NEWSLETTER_HTML));
        let text_only = email(
            json!([part("1", "text/plain")]),
            json!([part("1", "text/plain")]),
            json!({ "1": value("Only text") }),
        );
        assert_eq!(raw_html_body(&text_only), None);
    }

    #[test]
    fn multipart_alternative_keeps_plain_text_and_flags_remote_html() {
        let email = email(
            json!([part("1", "text/plain")]),
            json!([part("2", "text/html")]),
            json!({
                "1": value("Your new dashboard   \r\nFaster exports\r\n"),
                "2": value(NEWSLETTER_HTML),
            }),
        );
        let (body_text, sanitized_html, has_remote_content, preview) =
            extract_renderable_bodies(&email);

        assert_eq!(
            body_text.as_deref(),
            Some("Your new dashboard\nFaster exports")
        );
        assert!(
            sanitized_html.is_none(),
            "only the displayed alternative is stored, matching the IMAP adapter"
        );
        assert!(
            has_remote_content,
            "the HTML alternative's remote image must still be reported"
        );
        assert_eq!(
            preview.as_deref(),
            Some("Your new dashboard Faster exports")
        );
    }

    #[test]
    fn plain_text_only_bodies_keep_their_text() {
        // RFC 8621 4.1.4: with no HTML alternative the text/plain part is
        // listed in htmlBody too. Markup inside plain text stays literal.
        let email = email(
            json!([part("1", "text/plain")]),
            json!([part("1", "text/plain")]),
            json!({ "1": value("Line one\r\nVisit https://example.test/ for <b>details</b>.\r\n") }),
        );
        let (body_text, sanitized_html, has_remote_content, preview) =
            extract_renderable_bodies(&email);

        assert_eq!(
            body_text.as_deref(),
            Some("Line one\nVisit https://example.test/ for <b>details</b>.")
        );
        assert!(sanitized_html.is_none());
        assert!(!has_remote_content);
        assert_eq!(
            preview.as_deref(),
            Some("Line one Visit https://example.test/ for <b>details</b>.")
        );
    }

    #[test]
    fn parts_without_a_type_default_to_plain_text() {
        let email = email(
            json!([{ "partId": "1" }]),
            json!([{ "partId": "1" }]),
            json!({ "1": value("Untyped body") }),
        );
        let (body_text, sanitized_html, has_remote_content, _) = extract_renderable_bodies(&email);

        assert_eq!(body_text.as_deref(), Some("Untyped body"));
        assert!(sanitized_html.is_none());
        assert!(!has_remote_content);
    }

    #[test]
    fn bodies_without_values_stay_unhydrated() {
        let email = email(json!([]), json!([]), json!({}));
        assert_eq!(extract_renderable_bodies(&email), (None, None, false, None));
    }

    #[test]
    fn empty_address_lists_render_cleanly() {
        assert_eq!(format_jmap_addresses(None), "Unknown");
    }

    /// An email in thread T1 of the inbox, received at `received_at`.
    fn threaded_email(id: &str, subject: &str, received_at: &str, parents: &[&str]) -> JmapEmail {
        JmapEmail {
            id: id.to_owned(),
            thread_id: "T1".to_owned(),
            mailbox_ids: vec!["inbox".to_owned()],
            from: "Ada <ada@example.test>".to_owned(),
            reply_to: None,
            recipients: "me@example.test".to_owned(),
            subject: subject.to_owned(),
            preview: format!("Preview {id}"),
            body_text: None,
            sanitized_html: None,
            raw_html: None,
            received_at: received_at.to_owned(),
            unread: true,
            flagged: false,
            has_attachments: false,
            has_remote_content: false,
            identity_email: String::new(),
            attachments: Vec::new(),
            headers: MessageHeaders {
                captured: true,
                message_id: Some(format!("{id}@example.test")),
                in_reply_to: parents
                    .last()
                    .map(|id| (*id).to_owned())
                    .into_iter()
                    .collect(),
                references: parents.iter().map(|id| (*id).to_owned()).collect(),
                ..MessageHeaders::default()
            },
        }
    }

    #[test]
    fn a_reply_received_in_its_parents_second_names_the_thread() {
        // receivedAt is to the second, and the server lists a tie in
        // either order.
        let at = "2026-11-03T16:15:05Z";
        for reply_first in [false, true] {
            let parent = threaded_email("e1", "Plans", at, &["e0@example.test"]);
            let reply = threaded_email(
                "e2",
                "Re: Plans",
                at,
                &["e0@example.test", "e1@example.test"],
            );
            let emails = if reply_first {
                vec![reply, parent]
            } else {
                vec![parent, reply]
            };
            let mailboxes = group_mailboxes(
                vec![JmapMailbox {
                    id: "inbox".to_owned(),
                    name: "Inbox".to_owned(),
                    role: Some("inbox".to_owned()),
                    sort_order: 0,
                    parent: None,
                }],
                emails,
                &[],
                "me@example.test",
            );

            let thread = &mailboxes[0].threads[0];
            assert_eq!(
                thread.subject, "Re: Plans",
                "reply listed first: {reply_first}"
            );
            assert_eq!(thread.preview, "Preview e2");
            let order = thread
                .messages
                .iter()
                .map(|message| message.remote_id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(order, ["e1", "e2"], "the reply is the newest message");
            assert_eq!(thread.unread_count, 2);
        }
    }
}

#[cfg(test)]
mod submission_tests;

#[cfg(test)]
mod session_tests;

#[cfg(test)]
mod wire_parity_tests;

#[cfg(test)]
mod read_characterisation_tests;

#[cfg(test)]
mod write_characterisation_tests;
