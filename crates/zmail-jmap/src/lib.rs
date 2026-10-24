//! Real JMAP adapter for Phase 2.

mod changes;
mod push;

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use jmap_client::{
    client::Client,
    core::{
        query::QueryResponse,
        response::{EmailGetResponse, IdentityGetResponse, MailboxGetResponse},
    },
    email::{self, Property},
    mailbox::Role,
};
use serde::{Deserialize, Serialize};
use url::Url;
use zmail_core::{
    AccountSyncSnapshot, AuthKind, BlobStore, MailboxSnapshot, MessageAttachment, MessageSnapshot,
    NewIdentity, OpApplyReport, OutgoingMessage, PendingOp, PendingOpKind, ProtocolKind,
    SyncTarget, ThreadSnapshot,
};
use zmail_security::sanitize_html_for_display;
use zmail_smtp::render_rfc822_message;

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

/// Connects to a live JMAP account and returns the mailbox snapshot written into the local store.
pub async fn discover_and_sync(
    target: &SyncTarget,
    secret: &str,
    existing_mailboxes: &[MailboxSnapshot],
) -> Result<AccountSyncSnapshot> {
    if target.protocol != ProtocolKind::Jmap {
        bail!("JMAP adapter received a non-JMAP sync target");
    }
    if !(target.endpoint.starts_with("https://") || target.endpoint.starts_with("http://")) {
        bail!("JMAP endpoints should be absolute HTTP or HTTPS URLs");
    }

    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;

    let checkpoint = JmapCheckpoint::from_json(target.checkpoint.as_deref())?;
    let mut identities = fetch_identities(&client).await.unwrap_or_default();
    if identities.is_empty() {
        identities = target
            .identities
            .iter()
            .enumerate()
            .map(|(index, identity)| NewIdentity {
                display_name: identity.display_name.clone(),
                email_address: identity.email_address.clone(),
                signature: String::new(),
                is_primary: identity.is_primary || index == 0,
            })
            .collect();
    }
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
            return Ok(AccountSyncSnapshot {
                checkpoint: JmapCheckpoint {
                    mailbox_query_state: Some(mailbox_state),
                    email_query_state: None,
                    email_state: Some(incremental.email_state),
                    mailbox_ids: Some(mailbox_ids),
                }
                .to_json()?,
                detail,
                identities: Some(identities),
                mailboxes: incremental.mailboxes,
            });
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
        return Ok(AccountSyncSnapshot {
            checkpoint: JmapCheckpoint {
                mailbox_query_state: Some(mailbox_state),
                email_query_state: Some(email_query.query_state),
                email_state: Some(email_state),
                mailbox_ids: Some(mailbox_ids),
            }
            .to_json()?,
            detail: format!("JMAP mailbox state unchanged for {}", target.display_name),
            identities: Some(identities),
            mailboxes: existing_mailboxes.to_vec(),
        });
    }

    let emails = fetch_emails(&client, &email_query.ids, &target.blobs).await?;
    let mailboxes = group_mailboxes(mailbox_details, emails, &identities, &target.username);

    Ok(AccountSyncSnapshot {
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
        identities: Some(identities),
        mailboxes,
    })
}

/// Applies queued mailbox operations to the live JMAP account.
///
/// Read and flag operations update the `$seen` and `$flagged` keywords via
/// `Email/set`, move operations replace the mailbox ids so the message is
/// moved rather than copied, and destroy operations rely on the server's
/// trash handling through `Email/set` destroy.
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

    let needs_mailbox_map = ops.iter().any(|op| matches!(op.kind, PendingOpKind::Move));
    let mailbox_ids: HashMap<String, String> = if needs_mailbox_map {
        fetch_mailboxes(&client)
            .await?
            .0
            .into_iter()
            .map(|mailbox| (mailbox.name, mailbox.id))
            .collect()
    } else {
        HashMap::new()
    };

    for op in ops {
        let result = apply_single_jmap_op(&client, op, &mailbox_ids).await;
        match result {
            Ok(()) => report.applied.push(op.id),
            Err(error) => report.failed.push((op.id, format!("{error:#}"))),
        }
    }

    Ok(report)
}

async fn apply_single_jmap_op(
    client: &Client,
    op: &PendingOp,
    mailbox_ids: &HashMap<String, String>,
) -> Result<()> {
    let email_id = op.remote_id.as_str();
    match op.kind {
        PendingOpKind::MarkRead => client
            .email_set_keyword(email_id, "$seen", true)
            .await
            .map(|_| ())
            .with_context(|| format!("failed to mark JMAP email {email_id} as read")),
        PendingOpKind::MarkUnread => client
            .email_set_keyword(email_id, "$seen", false)
            .await
            .map(|_| ())
            .with_context(|| format!("failed to mark JMAP email {email_id} as unread")),
        PendingOpKind::SetFlagged => client
            .email_set_keyword(email_id, "$flagged", true)
            .await
            .map(|_| ())
            .with_context(|| format!("failed to flag JMAP email {email_id}")),
        PendingOpKind::ClearFlagged => client
            .email_set_keyword(email_id, "$flagged", false)
            .await
            .map(|_| ())
            .with_context(|| format!("failed to unflag JMAP email {email_id}")),
        PendingOpKind::Destroy => client
            .email_destroy(email_id)
            .await
            .with_context(|| format!("failed to destroy JMAP email {email_id}")),
        PendingOpKind::Move => {
            let Some(target_mailbox) = op.target_mailbox.as_deref() else {
                bail!("move operation without a target mailbox");
            };
            let Some(target_id) = mailbox_ids.get(target_mailbox) else {
                bail!("the JMAP account has no mailbox named {target_mailbox}");
            };
            // A move replaces the mailbox membership instead of adding a
            // second mailbox, matching IMAP MOVE semantics.
            client
                .email_set_mailboxes(email_id, [target_id.to_owned()])
                .await
                .map(|_| ())
                .with_context(|| {
                    format!("failed to move JMAP email {email_id} to {target_mailbox}")
                })
        }
    }
}

/// Submits a composed outgoing message through JMAP `EmailSubmission`.
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

    let client = connect_client(endpoint, username, secret, auth).await?;
    let identity_id = resolve_identity_id(&client, identity_email)
        .await?
        .with_context(|| format!("no JMAP identity matches {identity_email}"))?;
    let sent_mailbox_ids = sent_mailbox_ids(&client).await?;
    let raw_message = render_rfc822_message(message)?;
    let imported = client
        .email_import_account(
            client.default_account_id(),
            raw_message,
            sent_mailbox_ids,
            Option::<Vec<String>>::None,
            None,
        )
        .await
        .context("failed to import the outgoing message into JMAP")?;
    let email_id = imported
        .id()
        .map(str::to_owned)
        .with_context(|| "JMAP import did not return an email id")?;

    client
        .email_submission_create_envelope(
            email_id,
            identity_id,
            message.from_email.as_str(),
            all_recipients(message),
        )
        .await
        .context("failed to create the JMAP EmailSubmission")?;

    Ok(())
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
    received_at: String,
    unread: bool,
    flagged: bool,
    has_attachments: bool,
    has_remote_content: bool,
    identity_email: String,
    attachments: Vec<MessageAttachment>,
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
        AuthKind::OAuth2 => Ok(jmap_client::client::Credentials::bearer(secret.to_owned())),
        AuthKind::ProtonSession => bail!("Proton accounts do not sign in over JMAP"),
    }
}

pub(crate) async fn connect_client(
    endpoint: &str,
    username: &str,
    secret: &str,
    auth: AuthKind,
) -> Result<Client> {
    let mut client_builder = Client::new().credentials(jmap_credentials(auth, username, secret)?);
    if let Some(host) = Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
    {
        client_builder = client_builder.follow_redirects([host]);
    }

    client_builder
        .connect(endpoint)
        .await
        .with_context(|| format!("failed to connect to the JMAP endpoint {endpoint}"))
}

async fn fetch_identities(client: &Client) -> Result<Vec<NewIdentity>> {
    let request = client.build();
    let mut response: IdentityGetResponse = request
        .send_get_identity()
        .await
        .context("failed to fetch JMAP identities")?;
    let identities = response
        .take_list()
        .into_iter()
        .enumerate()
        .filter_map(|(index, identity)| {
            Some(NewIdentity {
                display_name: identity.name()?.to_owned(),
                email_address: identity.email()?.to_owned(),
                signature: identity.text_signature().unwrap_or_default().to_owned(),
                is_primary: index == 0,
            })
        })
        .collect::<Vec<_>>();

    Ok(identities)
}

async fn fetch_submission_identities(client: &Client) -> Result<Vec<SubmissionIdentity>> {
    let request = client.build();
    let mut response: IdentityGetResponse = request
        .send_get_identity()
        .await
        .context("failed to fetch JMAP identities for submission")?;
    Ok(response
        .take_list()
        .into_iter()
        .filter_map(|identity| {
            Some(SubmissionIdentity {
                id: identity.id()?.to_owned(),
                email: identity.email()?.to_owned(),
            })
        })
        .collect())
}

async fn fetch_mailboxes(client: &Client) -> Result<(Vec<JmapMailbox>, String)> {
    let mut request = client.build();
    request.get_mailbox();
    let mut response: MailboxGetResponse = request
        .send_get_mailbox()
        .await
        .context("failed to fetch JMAP mailboxes")?;
    let state = response.take_state();

    let list = response.take_list();
    let names = list
        .iter()
        .filter_map(|mailbox| Some((mailbox.id()?, mailbox.name()?)))
        .collect::<HashMap<_, _>>();
    let mut mailboxes = list
        .iter()
        .filter_map(|mailbox| {
            Some(JmapMailbox {
                id: mailbox.id()?.to_owned(),
                name: mailbox.name()?.to_owned(),
                role: mailbox_role(mailbox.role()),
                sort_order: mailbox.sort_order(),
                parent: mailbox
                    .parent_id()
                    .and_then(|parent_id| names.get(parent_id))
                    .map(|name| (*name).to_owned()),
            })
        })
        .collect::<Vec<_>>();
    mailboxes.sort_by_key(|mailbox| (mailbox.sort_order, mailbox.name.clone()));
    Ok((mailboxes, state))
}

async fn sent_mailbox_ids(client: &Client) -> Result<Vec<String>> {
    let mut request = client.build();
    request.get_mailbox();
    let mut response: MailboxGetResponse = request
        .send_get_mailbox()
        .await
        .context("failed to fetch JMAP mailboxes for submission")?;
    Ok(response
        .take_list()
        .into_iter()
        .filter(|mailbox| matches!(mailbox.role(), Role::Sent))
        .filter_map(|mailbox| mailbox.id().map(str::to_owned))
        .collect())
}

async fn resolve_identity_id(client: &Client, identity_email: &str) -> Result<Option<String>> {
    let identities = fetch_submission_identities(client).await?;
    Ok(identities
        .into_iter()
        .find(|identity| identity.email.eq_ignore_ascii_case(identity_email))
        .map(|identity| identity.id))
}

async fn fetch_email_query(client: &Client) -> Result<JmapQueryResult> {
    let mut ids = Vec::new();
    let mut position = 0;
    let mut query_state = None;

    loop {
        let mut request = client.build();
        request
            .query_email()
            .sort([email::query::Comparator::received_at()])
            .position(position)
            .limit(QUERY_PAGE_SIZE);
        let mut response: QueryResponse = request
            .send_query_email()
            .await
            .context("failed to query JMAP emails")?;
        if query_state.is_none() {
            query_state = Some(response.take_query_state());
        }
        let batch = response.take_ids();
        if batch.is_empty() {
            break;
        }
        position += batch.len() as i32;
        ids.extend(batch);
    }

    Ok(JmapQueryResult {
        ids,
        query_state: query_state.unwrap_or_default(),
    })
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
pub async fn search_emails<Tz: chrono::TimeZone>(
    target: &SyncTarget,
    secret: &str,
    query: &zmail_core::search::SearchQuery,
    now: &DateTime<Tz>,
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
    let mut request = client.build();
    request
        .query_email()
        .filter(filter)
        .sort([email::query::Comparator::received_at().descending()])
        .limit(max_hits);
    let mut response: QueryResponse = request
        .send_query_email()
        .await
        .context("failed to search JMAP emails")?;
    Ok(response
        .take_ids()
        .into_iter()
        .map(|id| zmail_core::ServerHitRef {
            mailbox_name: None,
            remote_id: id,
        })
        .collect())
}

/// Builds the `Email/query` filter for a query (see [`search_emails`]).
/// Returns `None` when the query cannot match anything (an `in:` naming no
/// mailbox).
fn search_filter<Tz: chrono::TimeZone>(
    query: &zmail_core::search::SearchQuery,
    mailboxes: &[JmapMailbox],
    now: &DateTime<Tz>,
) -> Option<jmap_client::core::query::Filter<email::query::Filter>> {
    use email::query::Filter;
    use jmap_client::core::query::Filter as Operator;
    use zmail_core::search::{SearchTerm, day_start_unix, mailbox_matches};

    let today = now.date_naive();
    let zone = now.timezone();
    let mut conditions: Vec<Operator<Filter>> = Vec::new();
    for clause in &query.clauses {
        let condition: Operator<Filter> = match &clause.term {
            SearchTerm::Text(text)
            | SearchTerm::From(text)
            | SearchTerm::To(text)
            | SearchTerm::Subject(text) => {
                if !text.is_searchable() {
                    continue;
                }
                let value = text.value.clone();
                match &clause.term {
                    SearchTerm::Text(_) => Filter::text(value),
                    SearchTerm::From(_) => Filter::from(value),
                    SearchTerm::To(_) => Filter::to(value),
                    _ => Filter::subject(value),
                }
                .into()
            }
            SearchTerm::HasAttachment => Filter::has_attachment(true).into(),
            SearchTerm::Unread => Filter::not_keyword("$seen").into(),
            SearchTerm::Read => Filter::has_keyword("$seen").into(),
            SearchTerm::Flagged => Filter::has_keyword("$flagged").into(),
            SearchTerm::Mailbox(value) => {
                let ids = mailboxes
                    .iter()
                    .filter(|mailbox| {
                        mailbox_matches(value, &mailbox.name, mailbox.role.as_deref())
                    })
                    .map(|mailbox| Filter::in_mailbox(mailbox.id.clone()))
                    .collect::<Vec<_>>();
                if ids.is_empty() {
                    if clause.negated {
                        continue;
                    }
                    return None;
                }
                Operator::or(ids)
            }
            SearchTerm::Before(spec) | SearchTerm::After(spec) => {
                let Some(boundary) = spec
                    .resolve(today)
                    .and_then(|day| day_start_unix(day, &zone))
                else {
                    continue;
                };
                if matches!(clause.term, SearchTerm::Before(_)) {
                    Filter::before(boundary).into()
                } else {
                    Filter::after(boundary).into()
                }
            }
        };
        conditions.push(if clause.negated {
            Operator::not([condition])
        } else {
            condition
        });
    }
    Some(Operator::and(conditions))
}

/// Fetches email details, downloading attachment blobs into the store's
/// attachments directory.
///
/// JMAP delivers bodies with the snapshot (there is no lazy hydration path),
/// so attachments download during sync as well. Each part resolves through
/// the email's `attachments` property and downloads over the session's
/// binary download URL; `jmap-client` 0.4.0 has no `Blob/get` request helper,
/// but the download endpoint returns the same bytes. Failures skip the part
/// rather than failing the sync, mirroring the IMAP adapter's tolerance.
pub(crate) async fn fetch_emails(
    client: &Client,
    ids: &[String],
    blobs: &BlobStore,
) -> Result<Vec<JmapEmail>> {
    let mut emails = Vec::with_capacity(ids.len());
    let properties = vec![
        Property::Id,
        Property::ThreadId,
        Property::MailboxIds,
        Property::Keywords,
        Property::From,
        Property::ReplyTo,
        Property::To,
        Property::Subject,
        Property::Preview,
        Property::HasAttachment,
        Property::Attachments,
        Property::BodyValues,
        Property::TextBody,
        Property::HtmlBody,
        Property::ReceivedAt,
    ];

    for chunk in ids.chunks(QUERY_PAGE_SIZE) {
        let mut request = client.build();
        let get_request = request.get_email();
        get_request.ids(chunk.iter()).properties(properties.clone());
        get_request.arguments().fetch_all_body_values(true);
        let mut response: EmailGetResponse = request
            .send_get_email()
            .await
            .context("failed to fetch JMAP email details")?;

        for email in response.take_list() {
            let thread_id = email
                .thread_id()
                .map(str::to_owned)
                .or_else(|| email.id().map(str::to_owned))
                .unwrap_or_else(|| "unknown-thread".to_owned());
            let identity_email = email
                .from()
                .and_then(|addresses| addresses.first())
                .map(|address| address.email().to_owned())
                .unwrap_or_default();
            let id = email.id().map(str::to_owned).unwrap_or_default();

            let (body_text, sanitized_html, has_remote_content, hydrated_preview) =
                extract_renderable_bodies(&email);
            let attachments = download_attachments(client, &email, &id, blobs).await;
            emails.push(JmapEmail {
                id,
                thread_id,
                mailbox_ids: email.mailbox_ids().into_iter().map(str::to_owned).collect(),
                from: format_jmap_addresses(email.from()),
                reply_to: match format_jmap_addresses(email.reply_to()) {
                    value if value == "Unknown" => None,
                    value => Some(value),
                },
                recipients: format_jmap_addresses(email.to()),
                subject: email.subject().unwrap_or("(no subject)").to_owned(),
                preview: hydrated_preview
                    .or_else(|| email.preview().map(str::to_owned))
                    .unwrap_or_else(|| "No preview available.".to_owned()),
                body_text,
                sanitized_html,
                received_at: email
                    .received_at()
                    .map(timestamp_to_rfc3339)
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
                unread: !email.keywords().contains(&"$seen"),
                flagged: email.keywords().contains(&"$flagged"),
                has_attachments: email.has_attachment() || !attachments.is_empty(),
                has_remote_content,
                identity_email,
                attachments,
            });
        }
    }

    Ok(emails)
}

/// Downloads one email's attachment parts into the blob directory.
///
/// Parts without a blob id or a name, oversized parts, and failed downloads
/// are skipped so a single bad part never fails the account sync.
async fn download_attachments(
    client: &Client,
    email: &jmap_client::email::Email,
    email_id: &str,
    blobs: &BlobStore,
) -> Vec<MessageAttachment> {
    let mut attachments = Vec::new();
    let Some(parts) = email.attachments() else {
        return attachments;
    };
    if parts.is_empty() {
        return attachments;
    }

    for (index, part) in parts.iter().enumerate() {
        if attachments.len() >= MAX_ATTACHMENTS_PER_MESSAGE {
            break;
        }
        let Some(blob_id) = part.blob_id() else {
            continue;
        };
        if part.size() > MAX_ATTACHMENT_BYTES {
            continue;
        }
        let Some(filename) = part.name().map(str::to_owned) else {
            continue;
        };
        if filename.trim().is_empty() {
            continue;
        }
        let Ok(contents) = client.download(blob_id).await else {
            continue;
        };
        if contents.len() > MAX_ATTACHMENT_BYTES {
            continue;
        }
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
                .content_type()
                .map(str::to_owned)
                .unwrap_or_else(|| "application/octet-stream".to_owned()),
            size_bytes: contents.len() as u64,
            storage_path,
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
                .or_insert_with(|| ThreadAccumulator {
                    remote_thread_id: email.thread_id.clone(),
                    subject: email.subject.clone(),
                    preview: email.preview.clone(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 0,
                    latest_message_at: email.received_at.clone(),
                    messages: Vec::new(),
                });
            if email.received_at > entry.latest_message_at {
                entry.latest_message_at = email.received_at.clone();
                entry.preview = email.preview.clone();
                entry.subject = email.subject.clone();
            }
            entry.is_flagged |= email.flagged;
            entry.has_attachments |= email.has_attachments;
            entry.unread_count += usize::from(email.unread);
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
                    thread
                        .messages
                        .sort_by(|left, right| left.received_at.cmp(&right.received_at));
                    ThreadSnapshot {
                        remote_thread_id: thread.remote_thread_id,
                        subject: thread.subject,
                        participants: zmail_core::participants::thread_participants(
                            &thread.messages,
                        ),
                        preview: thread.preview,
                        is_flagged: thread.is_flagged,
                        has_attachments: thread.has_attachments,
                        unread_count: thread.unread_count,
                        latest_message_at: thread.latest_message_at,
                        messages: thread.messages,
                    }
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
        has_remote_content: email.has_remote_content,
        is_read: !email.unread,
        is_flagged: email.flagged,
        received_at: email.received_at.clone(),
        attachments: email.attachments.clone(),
    }
}

#[derive(Debug)]
struct MailboxAccumulator {
    name: String,
    role: Option<String>,
    parent: Option<String>,
    sort_order: u32,
    threads: BTreeMap<String, ThreadAccumulator>,
}

#[derive(Debug)]
struct ThreadAccumulator {
    remote_thread_id: String,
    subject: String,
    preview: String,
    is_flagged: bool,
    has_attachments: bool,
    unread_count: usize,
    latest_message_at: String,
    messages: Vec<MessageSnapshot>,
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
    email: &jmap_client::email::Email,
) -> (Option<String>, Option<String>, bool, Option<String>) {
    let text_parts = email.text_body().unwrap_or_default();
    let html_parts = email.html_body().unwrap_or_default();

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

/// Returns the fetched value of the first part whose media type is
/// `media_type`.
///
/// A part without a `type` is `text/plain`, the MIME default (RFC 2045
/// section 5.2). Parameters and letter case are ignored.
fn body_value_of_type<'a>(
    parts: impl IntoIterator<Item = &'a jmap_client::email::EmailBodyPart>,
    email: &jmap_client::email::Email,
    media_type: &str,
) -> Option<String> {
    parts
        .into_iter()
        .filter(|part| {
            let content_type = part.content_type().unwrap_or("text/plain");
            content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case(media_type)
        })
        .find_map(|part| part.part_id().and_then(|part_id| email.body_value(part_id)))
        .map(|value| value.value().to_owned())
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

fn format_jmap_addresses(addresses: Option<&[jmap_client::email::EmailAddress]>) -> String {
    match addresses {
        Some(addresses) if !addresses.is_empty() => addresses
            .iter()
            .map(|address| match address.name() {
                Some(name) if !name.trim().is_empty() => {
                    format!("{name} <{}>", address.email())
                }
                _ => address.email().to_owned(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => "Unknown".to_owned(),
    }
}

fn all_recipients(message: &OutgoingMessage) -> Vec<String> {
    message
        .to
        .iter()
        .chain(message.cc.iter())
        .chain(message.bcc.iter())
        .cloned()
        .collect()
}

fn mailbox_role(role: jmap_client::mailbox::Role) -> Option<String> {
    match role {
        jmap_client::mailbox::Role::Archive => Some("archive".to_owned()),
        jmap_client::mailbox::Role::Drafts => Some("drafts".to_owned()),
        jmap_client::mailbox::Role::Important => Some("important".to_owned()),
        jmap_client::mailbox::Role::Inbox => Some("inbox".to_owned()),
        jmap_client::mailbox::Role::Junk => Some("junk".to_owned()),
        jmap_client::mailbox::Role::Sent => Some("sent".to_owned()),
        jmap_client::mailbox::Role::Trash => Some("trash".to_owned()),
        jmap_client::mailbox::Role::Other(value) => Some(value.to_ascii_lowercase()),
        jmap_client::mailbox::Role::None => None,
    }
}

fn timestamp_to_rfc3339(timestamp: i64) -> String {
    if let Some(datetime) = DateTime::<Utc>::from_timestamp(timestamp, 0) {
        datetime.to_rfc3339()
    } else {
        "1970-01-01T00:00:00Z".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        JmapCheckpoint, extract_renderable_bodies, format_jmap_addresses, jmap_credentials,
        sanitize_component, timestamp_to_rfc3339,
    };
    use serde_json::json;
    use zmail_core::AuthKind;

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
        use chrono::TimeZone;
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
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 6, 15, 12, 0, 0)
            .single()
            .expect("valid time");
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
    ) -> jmap_client::email::Email {
        serde_json::from_value(json!({
            "id": "M1",
            "textBody": text_body,
            "htmlBody": html_body,
            "bodyValues": body_values,
        }))
        .expect("the Email/get fixture should deserialize")
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
}
