//! Real JMAP adapter for Phase 2.

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
    AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, NewIdentity, OutgoingMessage,
    ProtocolKind, SyncTarget, ThreadSnapshot,
};
use zmail_security::sanitize_html_for_display;
use zmail_smtp::render_rfc822_message;

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

const QUERY_PAGE_SIZE: usize = 250;

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

    let client = connect_client(&target.endpoint, &target.username, secret).await?;

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
    let email_query = fetch_email_query(&client).await?;

    if checkpoint.mailbox_query_state.as_deref() == Some(mailbox_state.as_str())
        && checkpoint.email_query_state.as_deref() == Some(email_query.query_state.as_str())
    {
        return Ok(AccountSyncSnapshot {
            checkpoint: JmapCheckpoint {
                mailbox_query_state: Some(mailbox_state),
                email_query_state: Some(email_query.query_state),
            }
            .to_json()?,
            detail: format!("JMAP mailbox state unchanged for {}", target.display_name),
            identities: Some(identities),
            mailboxes: existing_mailboxes.to_vec(),
        });
    }

    let emails = fetch_emails(&client, &email_query.ids).await?;
    let mailboxes = group_mailboxes(mailbox_details, emails, &identities, &target.username);

    Ok(AccountSyncSnapshot {
        checkpoint: JmapCheckpoint {
            mailbox_query_state: Some(mailbox_state),
            email_query_state: Some(email_query.query_state),
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

/// Submits a composed outgoing message through JMAP `EmailSubmission`.
pub async fn submit_outgoing_message(
    endpoint: &str,
    username: &str,
    secret: &str,
    identity_email: &str,
    message: &OutgoingMessage,
) -> Result<()> {
    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        bail!("JMAP submission requires at least one recipient");
    }

    let client = connect_client(endpoint, username, secret).await?;
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
}

#[derive(Debug)]
struct JmapQueryResult {
    ids: Vec<String>,
    query_state: String,
}

#[derive(Debug)]
struct JmapMailbox {
    id: String,
    name: String,
    role: Option<String>,
    sort_order: u32,
}

#[derive(Debug)]
struct JmapEmail {
    thread_id: String,
    mailbox_ids: Vec<String>,
    from: String,
    recipients: String,
    subject: String,
    preview: String,
    body_text: Option<String>,
    sanitized_html: Option<String>,
    received_at: String,
    unread: bool,
    flagged: bool,
    has_remote_content: bool,
    identity_email: String,
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

async fn connect_client(endpoint: &str, username: &str, secret: &str) -> Result<Client> {
    let mut client_builder = Client::new().credentials((username, secret));
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

    let mut mailboxes = response
        .take_list()
        .into_iter()
        .filter_map(|mailbox| {
            Some(JmapMailbox {
                id: mailbox.id()?.to_owned(),
                name: mailbox.name()?.to_owned(),
                role: mailbox_role(mailbox.role()),
                sort_order: mailbox.sort_order(),
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

async fn fetch_emails(client: &Client, ids: &[String]) -> Result<Vec<JmapEmail>> {
    let mut emails = Vec::with_capacity(ids.len());
    let properties = vec![
        Property::Id,
        Property::ThreadId,
        Property::MailboxIds,
        Property::Keywords,
        Property::From,
        Property::To,
        Property::Subject,
        Property::Preview,
        Property::BodyValues,
        Property::TextBody,
        Property::HtmlBody,
        Property::ReceivedAt,
    ];

    for chunk in ids.chunks(QUERY_PAGE_SIZE) {
        let mut request = client.build();
        let get_request = request.get_email();
        get_request
            .ids(chunk.iter())
            .properties(properties.clone());
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

            let (body_text, sanitized_html, has_remote_content, hydrated_preview) =
                extract_renderable_bodies(&email);
            emails.push(JmapEmail {
                thread_id,
                mailbox_ids: email.mailbox_ids().into_iter().map(str::to_owned).collect(),
                from: format_jmap_addresses(email.from()),
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
                has_remote_content,
                identity_email,
            });
        }
    }

    Ok(emails)
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
                    sort_order: mailbox.sort_order,
                    threads: BTreeMap::new(),
                },
            )
        })
        .collect::<HashMap<_, _>>();

    let primary_identity = identities
        .iter()
        .find(|identity| identity.is_primary)
        .or_else(|| identities.first())
        .map(|identity| identity.email_address.as_str())
        .unwrap_or(fallback_identity);

    for email in emails {
        let identity_email = if email.identity_email.is_empty() {
            primary_identity.to_owned()
        } else {
            email.identity_email.clone()
        };
        for mailbox_id in &email.mailbox_ids {
            let Some(mailbox) = mailboxes.get_mut(mailbox_id) else {
                continue;
            };
            let entry = mailbox
                .threads
                .entry(email.thread_id.clone())
                .or_insert_with(|| ThreadAccumulator {
                    subject: email.subject.clone(),
                    participants: format!("{} -> {}", email.from, email.recipients),
                    preview: email.preview.clone(),
                    is_flagged: false,
                    unread_count: 0,
                    latest_message_at: email.received_at.clone(),
                    messages: Vec::new(),
                });
            if email.received_at > entry.latest_message_at {
                entry.latest_message_at = email.received_at.clone();
                entry.preview = email.preview.clone();
                entry.subject = email.subject.clone();
                entry.participants = format!("{} -> {}", email.from, email.recipients);
            }
            entry.is_flagged |= email.flagged;
            entry.unread_count += usize::from(email.unread);
            entry.messages.push(MessageSnapshot {
                identity_email: identity_email.clone(),
                sender: email.from.clone(),
                recipients: email.recipients.clone(),
                subject: email.subject.clone(),
                body_preview: email.preview.clone(),
                body_text: email.body_text.clone(),
                sanitized_html: email.sanitized_html.clone(),
                has_remote_content: email.has_remote_content,
                is_read: !email.unread,
                is_flagged: email.flagged,
                received_at: email.received_at.clone(),
            });
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
                        subject: thread.subject,
                        participants: thread.participants,
                        preview: thread.preview,
                        is_flagged: thread.is_flagged,
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
                    threads,
                },
            )
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.name.cmp(&right.1.name)));
    ordered.into_iter().map(|(_, mailbox)| mailbox).collect()
}

#[derive(Debug)]
struct MailboxAccumulator {
    name: String,
    role: Option<String>,
    sort_order: u32,
    threads: BTreeMap<String, ThreadAccumulator>,
}

#[derive(Debug)]
struct ThreadAccumulator {
    subject: String,
    participants: String,
    preview: String,
    is_flagged: bool,
    unread_count: usize,
    latest_message_at: String,
    messages: Vec<MessageSnapshot>,
}

fn extract_renderable_bodies(
    email: &jmap_client::email::Email,
) -> (Option<String>, Option<String>, bool, Option<String>) {
    let text_body = body_value(email.text_body(), email)
        .map(normalize_body_text)
        .filter(|value| !value.is_empty());
    if let Some(text_body) = text_body {
        let preview = excerpt(&text_body, 180);
        return (Some(text_body), None, false, Some(preview));
    }

    let html_body = body_value(email.html_body(), email).filter(|value| !value.is_empty());
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

fn body_value(
    parts: Option<&[jmap_client::email::EmailBodyPart]>,
    email: &jmap_client::email::Email,
) -> Option<String> {
    parts
        .into_iter()
        .flatten()
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
    use super::{JmapCheckpoint, format_jmap_addresses, timestamp_to_rfc3339};

    #[test]
    fn checkpoint_round_trip_is_stable() {
        let checkpoint = JmapCheckpoint {
            mailbox_query_state: Some("m-1".to_owned()),
            email_query_state: Some("e-4".to_owned()),
        };
        let json = checkpoint.to_json().expect("checkpoint should serialize");
        let parsed = JmapCheckpoint::from_json(Some(&json)).expect("checkpoint should parse");
        assert_eq!(parsed.mailbox_query_state.as_deref(), Some("m-1"));
        assert_eq!(parsed.email_query_state.as_deref(), Some("e-4"));
    }

    #[test]
    fn timestamps_render_to_rfc3339() {
        assert!(timestamp_to_rfc3339(1_700_000_000).contains('T'));
    }

    #[test]
    fn empty_address_lists_render_cleanly() {
        assert_eq!(format_jmap_addresses(None), "Unknown");
    }
}
