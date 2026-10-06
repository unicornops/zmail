//! Real IMAP adapter for Phase 2.

use std::{
    collections::BTreeMap,
    sync::{Arc, Once},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use async_imap::{
    Client, Session,
    types::{Fetch, Flag, Mailbox, Name, NameAttribute},
};
use futures_util::TryStreamExt;
use mail_parser::{Address, MessageParser};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};
use url::Url;
use zmail_core::{
    AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, OpApplyReport, PendingOp, PendingOpKind,
    ProtocolKind, SyncTarget, ThreadSnapshot,
};
use zmail_security::sanitize_html_for_display;

/// IMAP extensions prioritized by the plan.
pub const PRIORITY_CAPABILITIES: &[&str] = &[
    "STARTTLS",
    "IDLE",
    "UIDPLUS",
    "MOVE",
    "CONDSTORE",
    "QRESYNC",
    "LIST-STATUS",
    "SPECIAL-USE",
    "NAMESPACE",
];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const FETCH_BATCH_SIZE: usize = 50;
const FETCH_QUERY: &str = "(UID INTERNALDATE FLAGS BODY.PEEK[])";

fn ensure_rustls_provider() {
    static RUSTLS_PROVIDER: Once = Once::new();

    RUSTLS_PROVIDER.call_once(|| {
        let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Connects to a live IMAP account and returns the mailbox snapshot written into the local store.
pub async fn discover_and_sync(
    target: &SyncTarget,
    secret: &str,
    existing_mailboxes: &[MailboxSnapshot],
) -> Result<AccountSyncSnapshot> {
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }

    let endpoint = parse_endpoint(&target.endpoint)?;
    let identity_email = target
        .identities
        .iter()
        .find(|identity| identity.is_primary)
        .or_else(|| target.identities.first())
        .map(|identity| identity.email_address.as_str())
        .unwrap_or(target.username.as_str());
    let mut session = connect_session(&endpoint, &target.username, secret).await?;
    let mailbox_names = list_mailboxes(&mut session).await?;
    let checkpoint = ImapCheckpoint::from_json(target.checkpoint.as_deref())?;
    let existing = existing_mailbox_map(existing_mailboxes);

    let mut next_checkpoint = BTreeMap::new();
    let mut mailboxes = Vec::with_capacity(mailbox_names.len());
    let mut refreshed = 0usize;
    let mut incremental = 0usize;

    for mailbox_name in mailbox_names {
        let name = mailbox_name.name().to_owned();
        let status = session
            .examine(&name)
            .await
            .with_context(|| format!("failed to examine IMAP mailbox {name}"))?;

        let previous = checkpoint.mailboxes.get(&name).cloned();
        let mut mailbox_checkpoint = ImapMailboxCheckpoint::from_status(&status);
        next_checkpoint.insert(name.clone(), mailbox_checkpoint.clone());

        // While UIDVALIDITY is stable the existing mirror can be reused;
        // only flag changes and new mail need to be fetched.
        if previous.as_ref().and_then(|state| state.uid_validity) == status.uid_validity
            && previous.is_some()
            && let Some(existing_mailbox) = existing.get(name.as_str()).cloned()
        {
            let flag_map = fetch_flag_map(&mut session).await?;
            let last_seen_uid = previous.as_ref().and_then(|state| state.last_seen_uid);
            let new_uids = new_mailbox_uids(&mut session, last_seen_uid).await?;
            let flags_unchanged =
                previous.as_ref().map(|state| &state.uid_flags) == Some(&flag_map);

            if new_uids.is_empty() && flags_unchanged && !has_expunged(&flag_map, &existing_mailbox)
            {
                // Nothing changed server-side; the mirror is reused as is.
                mailboxes.push(existing_mailbox);
                mailbox_checkpoint.last_seen_uid = last_seen_uid;
                mailbox_checkpoint.uid_flags = flag_map;
                next_checkpoint.insert(name, mailbox_checkpoint);
                continue;
            }

            incremental += 1;
            let mut merged = existing_mailbox;
            merge_flag_changes(&mut merged, &flag_map);
            if !new_uids.is_empty() {
                append_new_threads(&mut session, &mut merged, &new_uids, identity_email).await?;
            }
            mailbox_checkpoint.last_seen_uid = Some(
                new_uids
                    .last()
                    .copied()
                    .unwrap_or(0)
                    .max(last_seen_uid.unwrap_or(0)),
            );
            mailbox_checkpoint.uid_flags = flag_map;
            next_checkpoint.insert(name, mailbox_checkpoint);
            mailboxes.push(merged);
            continue;
        }

        refreshed += 1;
        let (snapshot, uid_flags) =
            fetch_mailbox_snapshot(&mut session, &name, guess_role(&name), identity_email).await?;
        mailbox_checkpoint.last_seen_uid = uid_flags.keys().last().copied();
        mailbox_checkpoint.uid_flags = uid_flags;
        next_checkpoint.insert(name, mailbox_checkpoint);
        mailboxes.push(snapshot);
    }

    let _ = session.logout().await;

    let detail = if refreshed == 0 && incremental == 0 {
        format!("IMAP mailbox state unchanged for {}", target.display_name)
    } else {
        format!(
            "IMAP sync complete against {} with {refreshed} full and {incremental} incremental mailbox refreshes",
            endpoint.host
        )
    };

    Ok(AccountSyncSnapshot {
        checkpoint: serde_json::to_string(&ImapCheckpoint {
            mailboxes: next_checkpoint,
        })
        .context("failed to serialize the IMAP sync checkpoint")?,
        detail,
        identities: None,
        mailboxes,
    })
}

#[derive(Debug)]
struct ImapEndpoint {
    host: String,
    port: u16,
    security: ImapSecurity,
}

/// Waits until the IMAP server reports a mailbox change or the timeout
/// elapses, whichever comes first.
///
/// The wait uses one IDLE connection on INBOX, re-issued at most once per
/// call so the caller's interval always stays below the RFC 2177 guidance.
/// Servers that do not advertise IDLE degrade to a plain sleep, so the
/// caller's polling interval keeps governing latency.
pub async fn wait_for_mail_changes(
    target: &SyncTarget,
    secret: &str,
    wait: Duration,
) -> Result<()> {
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }

    let endpoint = parse_endpoint(&target.endpoint)?;
    let mut session = connect_session(&endpoint, &target.username, secret).await?;
    session
        .select("INBOX")
        .await
        .context("failed to select INBOX for the IDLE watcher")?;

    let bounded_wait = wait.min(Duration::from_secs(29 * 60));
    let mut idle = session.idle();
    if idle.init().await.is_err() {
        // The server does not advertise IDLE; fall back to the caller's
        // poll interval so latency stays bounded. The half-initialized
        // handle is dropped, which closes the connection.
        tokio::time::sleep(bounded_wait).await;
        return Ok(());
    }

    let (idle_wait, _stop) = idle.wait_with_timeout(bounded_wait);
    let _ = idle_wait.await;
    if let Ok(mut session) = idle.done().await {
        let _ = session.logout().await;
    }
    Ok(())
}

/// Applies queued mailbox operations to the live IMAP account.
///
/// Read and flag operations translate to `UID STORE`, move operations to
/// `UID MOVE` (falling back to `UID COPY` plus a deleted flag), and destroy
/// operations mark the message deleted without expunging, so an accidental
/// destroy never removes mail before the user can intervene.
pub async fn apply_pending_ops(
    target: &SyncTarget,
    secret: &str,
    ops: &[PendingOp],
) -> Result<OpApplyReport> {
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }

    let endpoint = parse_endpoint(&target.endpoint)?;
    let mut session = connect_session(&endpoint, &target.username, secret).await?;
    let mut report = OpApplyReport::default();

    let mut by_mailbox: BTreeMap<&str, Vec<&PendingOp>> = BTreeMap::new();
    for op in ops {
        by_mailbox
            .entry(op.source_mailbox.as_str())
            .or_default()
            .push(op);
    }

    for (mailbox, mailbox_ops) in by_mailbox {
        // Write-back needs read-write mailbox access; `examine` would make
        // the server reject every STORE.
        if let Err(error) = session.select(mailbox).await {
            for op in mailbox_ops {
                report.failed.push((
                    op.id,
                    format!("mailbox {mailbox} is not selectable: {error:#}"),
                ));
            }
            continue;
        }

        for op in mailbox_ops {
            let Some(uid) = op.remote_id.parse::<u32>().ok() else {
                report
                    .failed
                    .push((op.id, format!("invalid IMAP uid {}", op.remote_id)));
                continue;
            };
            let result = apply_single_op(&mut session, uid, op).await;
            match result {
                Ok(()) => report.applied.push(op.id),
                Err(error) => report.failed.push((op.id, format!("{error:#}"))),
            }
        }
    }

    let _ = session.logout().await;
    Ok(report)
}

async fn apply_single_op(
    session: &mut Session<DynImapStream>,
    uid: u32,
    op: &PendingOp,
) -> Result<()> {
    let uid_set = uid.to_string();
    match op.kind {
        PendingOpKind::MarkRead => {
            store_flags(session, uid_set, "+FLAGS (\\Seen)".to_owned()).await
        }
        PendingOpKind::MarkUnread => {
            store_flags(session, uid_set, "-FLAGS (\\Seen)".to_owned()).await
        }
        PendingOpKind::SetFlagged => {
            store_flags(session, uid_set, "+FLAGS (\\Flagged)".to_owned()).await
        }
        PendingOpKind::ClearFlagged => {
            store_flags(session, uid_set, "-FLAGS (\\Flagged)".to_owned()).await
        }
        PendingOpKind::Destroy => {
            // Marking deleted is safe and reversible; expunging is not run
            // until a dedicated, confirmed destroy flow exists.
            store_flags(session, uid_set, "+FLAGS (\\Deleted)".to_owned()).await
        }
        PendingOpKind::Move => {
            let Some(target_mailbox) = op.target_mailbox.as_deref().filter(|name| !name.is_empty())
            else {
                bail!("move operation without a target mailbox");
            };
            match session
                .uid_mv(uid_set.clone(), target_mailbox.to_owned())
                .await
            {
                Ok(()) => Ok(()),
                Err(move_error) => {
                    // Servers without the MOVE extension get the copy-plus-
                    // delete fallback, which keeps the message available.
                    session
                        .uid_copy(uid_set.clone(), target_mailbox.to_owned())
                        .await
                        .with_context(|| {
                            format!(
                                "failed to copy uid {uid} to {target_mailbox} while moving: {move_error}"
                            )
                        })?;
                    store_flags(session, uid_set, "+FLAGS (\\Deleted)".to_owned()).await
                }
            }
        }
    }
}

async fn store_flags(
    session: &mut Session<DynImapStream>,
    uid_set: String,
    query: String,
) -> Result<()> {
    let stream = session
        .uid_store(uid_set, query)
        .await
        .context("failed to run the IMAP STORE command")?;
    stream
        .try_collect::<Vec<_>>()
        .await
        .map(|_| ())
        .context("failed to apply the IMAP flag operation")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImapSecurity {
    Plain,
    ImplicitTls,
}

trait ImapIo: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}

impl<T> ImapIo for T where T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}

type DynImapStream = Box<dyn ImapIo>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ImapCheckpoint {
    mailboxes: BTreeMap<String, ImapMailboxCheckpoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ImapMailboxCheckpoint {
    uid_validity: Option<u32>,
    uid_next: Option<u32>,
    exists: u32,
    #[serde(default)]
    last_seen_uid: Option<u32>,
    #[serde(default)]
    uid_flags: BTreeMap<u32, Vec<String>>,
}

impl ImapCheckpoint {
    fn from_json(value: Option<&str>) -> Result<Self> {
        match value {
            Some(value) if !value.trim().is_empty() => serde_json::from_str(value)
                .with_context(|| "failed to decode the IMAP sync checkpoint"),
            _ => Ok(Self::default()),
        }
    }
}

impl ImapMailboxCheckpoint {
    fn from_status(status: &Mailbox) -> Self {
        Self {
            uid_validity: status.uid_validity,
            uid_next: status.uid_next,
            exists: status.exists,
            last_seen_uid: None,
            uid_flags: BTreeMap::new(),
        }
    }
}

fn parse_endpoint(endpoint: &str) -> Result<ImapEndpoint> {
    if let Ok(url) = Url::parse(endpoint) {
        let host = url
            .host_str()
            .context("IMAP endpoint must include a host")?
            .to_owned();
        let (port, security) = match url.scheme() {
            "imap" => (url.port().unwrap_or(143), ImapSecurity::Plain),
            "imaps" => (url.port().unwrap_or(993), ImapSecurity::ImplicitTls),
            scheme => bail!("unsupported IMAP endpoint scheme {scheme}"),
        };
        return Ok(ImapEndpoint {
            host,
            port,
            security,
        });
    }

    let (host, port) = if let Some((host, port)) = endpoint.rsplit_once(':') {
        (
            host.to_owned(),
            port.parse::<u16>()
                .with_context(|| format!("invalid IMAP port in endpoint {endpoint}"))?,
        )
    } else {
        (endpoint.to_owned(), 993)
    };

    if host.trim().is_empty() {
        bail!("IMAP endpoint must not be empty");
    }

    Ok(ImapEndpoint {
        host,
        port,
        security: ImapSecurity::ImplicitTls,
    })
}

async fn connect_session(
    endpoint: &ImapEndpoint,
    username: &str,
    secret: &str,
) -> Result<Session<DynImapStream>> {
    ensure_rustls_provider();

    let tcp = timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
    )
    .await
    .context("timed out while connecting to the IMAP server")?
    .with_context(|| format!("failed to connect to {}:{}", endpoint.host, endpoint.port))?;

    let stream: DynImapStream = match endpoint.security {
        ImapSecurity::Plain => Box::new(tcp),
        ImapSecurity::ImplicitTls => {
            let mut roots = RootCertStore::empty();
            let native = rustls_native_certs::load_native_certs();
            for cert in native.certs {
                roots
                    .add(cert)
                    .map_err(|error| anyhow!("failed to add a native root certificate: {error}"))?;
            }
            if roots.is_empty() {
                bail!("no TLS root certificates were loaded for the IMAP connection");
            }

            let config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector = TlsConnector::from(Arc::new(config));
            let server_name = ServerName::try_from(endpoint.host.clone())
                .with_context(|| format!("invalid IMAP server name {}", endpoint.host))?;
            let tls_stream = timeout(CONNECT_TIMEOUT, connector.connect(server_name, tcp))
                .await
                .context("timed out while establishing IMAP TLS")?
                .with_context(|| format!("failed to negotiate TLS with {}", endpoint.host))?;
            Box::new(tls_stream)
        }
    };

    let mut client = Client::new(stream);
    client
        .read_response()
        .await
        .context("failed to read the IMAP greeting")?
        .context("the IMAP server closed the connection before greeting")?;

    client
        .login(username, secret)
        .await
        .map_err(|(error, _)| anyhow!(error))
        .context("IMAP authentication failed")
}

async fn list_mailboxes(session: &mut Session<DynImapStream>) -> Result<Vec<Name>> {
    let mailboxes = session
        .list(None, Some("*"))
        .await
        .context("failed to list IMAP mailboxes")?
        .try_collect::<Vec<_>>()
        .await
        .context("failed to stream the IMAP mailbox list")?;

    Ok(mailboxes
        .into_iter()
        .filter(|mailbox| {
            !mailbox
                .attributes()
                .iter()
                .any(|attribute| matches!(attribute, NameAttribute::NoSelect))
        })
        .collect())
}

async fn fetch_mailbox_snapshot(
    session: &mut Session<DynImapStream>,
    mailbox_name: &str,
    role: Option<String>,
    identity_email: &str,
) -> Result<(MailboxSnapshot, BTreeMap<u32, Vec<String>>)> {
    session
        .examine(mailbox_name)
        .await
        .with_context(|| format!("failed to re-open IMAP mailbox {mailbox_name}"))?;

    let mut uids = session
        .uid_search("ALL")
        .await
        .with_context(|| format!("failed to query message ids for {mailbox_name}"))?
        .into_iter()
        .collect::<Vec<_>>();
    uids.sort_unstable();
    uids.reverse();

    let mut threads = Vec::with_capacity(uids.len());
    let mut uid_flags = BTreeMap::new();
    for chunk in uids.chunks(FETCH_BATCH_SIZE) {
        let uid_set = chunk
            .iter()
            .map(|uid| uid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let fetches = session
            .uid_fetch(&uid_set, FETCH_QUERY)
            .await
            .with_context(|| {
                format!("failed to fetch metadata for mailbox {mailbox_name} and UID set {uid_set}")
            })?
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| format!("failed to stream message metadata for {mailbox_name}"))?;

        for fetch in fetches {
            if let Some(uid) = fetch.uid {
                uid_flags.insert(
                    uid,
                    fetch
                        .flags()
                        .map(|flag| flag_name(&flag))
                        .collect::<Vec<_>>(),
                );
            }
            if let Some(thread) = fetch_to_thread_snapshot(fetch, identity_email) {
                threads.push(thread);
            }
        }
    }

    threads.sort_by(|left, right| right.latest_message_at.cmp(&left.latest_message_at));

    let snapshot = MailboxSnapshot {
        name: mailbox_name.to_owned(),
        role,
        threads,
    };
    Ok((snapshot, uid_flags))
}

/// Formats an IMAP flag for the checkpoint comparison map.
fn flag_name(flag: &Flag<'_>) -> String {
    match flag {
        Flag::Seen => "\\Seen".to_owned(),
        Flag::Answered => "\\Answered".to_owned(),
        Flag::Flagged => "\\Flagged".to_owned(),
        Flag::Deleted => "\\Deleted".to_owned(),
        Flag::Draft => "\\Draft".to_owned(),
        Flag::Recent => "\\Recent".to_owned(),
        Flag::MayCreate => "\\*".to_owned(),
        Flag::Custom(name) => name.to_string(),
    }
}

/// Fetches the current flags of every message without touching bodies.
///
/// The returned map doubles as the set of messages still present on the
/// server: UIDs missing from it have been expunged.
async fn fetch_flag_map(
    session: &mut Session<DynImapStream>,
) -> Result<BTreeMap<u32, Vec<String>>> {
    let fetches = session
        .uid_fetch("1:*", "(UID FLAGS)")
        .await
        .context("failed to fetch the message flags")?
        .try_collect::<Vec<_>>()
        .await
        .context("failed to stream the message flags")?;

    let mut flags = BTreeMap::new();
    for fetch in fetches {
        if let Some(uid) = fetch.uid {
            flags.insert(
                uid,
                fetch
                    .flags()
                    .map(|flag| flag_name(&flag))
                    .collect::<Vec<_>>(),
            );
        }
    }
    Ok(flags)
}

/// Returns the UIDs that arrived after the last seen one.
async fn new_mailbox_uids(
    session: &mut Session<DynImapStream>,
    last_seen_uid: Option<u32>,
) -> Result<Vec<u32>> {
    let Some(last_seen) = last_seen_uid else {
        return Ok(Vec::new());
    };

    let mut uids = session
        .uid_search(&format!("UID {}:*", last_seen.saturating_add(1)))
        .await
        .context("failed to search for new message ids")?
        .into_iter()
        .collect::<Vec<_>>();
    // The `*:n` range semantics can return the highest UID even when it
    // is below the search start, so filter explicitly.
    uids.retain(|uid| *uid > last_seen);
    uids.sort_unstable();
    Ok(uids)
}

/// Returns `true` when a message in the existing mirror is missing from
/// the server flag map, which means it was expunged.
fn has_expunged(flag_map: &BTreeMap<u32, Vec<String>>, mailbox: &MailboxSnapshot) -> bool {
    mailbox.threads.iter().any(|thread| {
        thread.messages.iter().any(|message| {
            message
                .remote_id
                .parse::<u32>()
                .is_ok_and(|uid| !flag_map.contains_key(&uid))
        })
    })
}

/// Applies the fetched server flags to the existing mirror and drops
/// threads whose messages were expunged.
fn merge_flag_changes(mailbox: &mut MailboxSnapshot, flag_map: &BTreeMap<u32, Vec<String>>) {
    mailbox.threads.retain_mut(|thread| {
        thread.messages.retain_mut(|message| {
            let Some(uid) = message.remote_id.parse::<u32>().ok() else {
                return true;
            };
            let Some(flags) = flag_map.get(&uid) else {
                return false;
            };
            message.is_read = flags.iter().any(|flag| flag.contains("Seen"));
            message.is_flagged = flags.iter().any(|flag| flag.contains("Flagged"));
            true
        });

        if thread.messages.is_empty() {
            return false;
        }

        thread.unread_count = thread
            .messages
            .iter()
            .filter(|message| !message.is_read)
            .count();
        thread.is_flagged = thread.messages.iter().any(|message| message.is_flagged);
        true
    });

    mailbox
        .threads
        .sort_by(|left, right| right.latest_message_at.cmp(&left.latest_message_at));
}

/// Fetches the given new UIDs and appends their threads to the mirror.
async fn append_new_threads(
    session: &mut Session<DynImapStream>,
    mailbox: &mut MailboxSnapshot,
    new_uids: &[u32],
    identity_email: &str,
) -> Result<()> {
    let mut new_threads = Vec::with_capacity(new_uids.len());
    for chunk in new_uids.chunks(FETCH_BATCH_SIZE) {
        let uid_set = chunk
            .iter()
            .map(|uid| uid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let fetches = session
            .uid_fetch(&uid_set, FETCH_QUERY)
            .await
            .with_context(|| format!("failed to fetch new messages for UID set {uid_set}"))?
            .try_collect::<Vec<_>>()
            .await
            .context("failed to stream new messages")?;

        for fetch in fetches {
            if let Some(thread) = fetch_to_thread_snapshot(fetch, identity_email) {
                new_threads.push(thread);
            }
        }
    }

    mailbox.threads.extend(new_threads);
    mailbox
        .threads
        .sort_by(|left, right| right.latest_message_at.cmp(&left.latest_message_at));
    Ok(())
}

fn fetch_to_thread_snapshot(fetch: Fetch, identity_email: &str) -> Option<ThreadSnapshot> {
    let raw = fetch.body()?;
    let message = MessageParser::default().parse(raw)?;
    let subject = message
        .subject()
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "(no subject)".to_owned());
    let sender = first_address(message.from()).unwrap_or_else(|| "Unknown sender".to_owned());
    let recipients = format_recipients(message.to(), identity_email);
    let participants = format!("{sender} -> {recipients}");
    let (body_text, sanitized_html, has_remote_content, preview_source) = hydrate_bodies(&message);
    let preview = excerpt(&preview_source, 180);
    let has_attachments = message.attachment_count() > 0;
    let latest_message_at = fetch
        .internal_date()
        .map(|date| date.to_rfc3339())
        .or_else(|| message.date().map(|date| date.to_rfc3339()))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
    let is_read = fetch.flags().any(|flag| matches!(flag, Flag::Seen));
    let is_flagged = fetch.flags().any(|flag| matches!(flag, Flag::Flagged));
    let unread_count = usize::from(!is_read);
    let remote_id = fetch.uid.map(|uid| uid.to_string()).unwrap_or_default();

    Some(ThreadSnapshot {
        remote_thread_id: remote_id.clone(),
        subject: subject.clone(),
        participants,
        preview: preview.clone(),
        is_flagged,
        has_attachments,
        unread_count,
        latest_message_at: latest_message_at.clone(),
        messages: vec![MessageSnapshot {
            remote_id,
            identity_email: identity_email.to_owned(),
            sender,
            recipients,
            subject,
            body_preview: preview,
            body_text,
            sanitized_html,
            has_remote_content,
            is_read,
            is_flagged,
            received_at: latest_message_at,
        }],
    })
}

fn first_address(addresses: Option<&Address<'_>>) -> Option<String> {
    collect_addresses(addresses)
        .into_iter()
        .next()
        .filter(|value| !value.trim().is_empty())
}

fn format_recipients(addresses: Option<&Address<'_>>, fallback: &str) -> String {
    let recipients = collect_addresses(addresses);
    if recipients.is_empty() {
        fallback.to_owned()
    } else {
        recipients.join(", ")
    }
}

fn collect_addresses(addresses: Option<&Address<'_>>) -> Vec<String> {
    match addresses {
        Some(Address::List(items)) => items
            .iter()
            .map(format_addr)
            .filter(|value| !value.trim().is_empty())
            .collect(),
        Some(Address::Group(groups)) => groups
            .iter()
            .flat_map(|group| group.addresses.iter())
            .map(format_addr)
            .filter(|value| !value.trim().is_empty())
            .collect(),
        None => Vec::new(),
    }
}

fn format_addr(address: &mail_parser::Addr<'_>) -> String {
    match (address.name.as_deref(), address.address.as_deref()) {
        (Some(name), Some(email)) if !name.trim().is_empty() => format!("{name} <{email}>"),
        (_, Some(email)) => email.to_owned(),
        (Some(name), None) => name.to_owned(),
        (None, None) => String::new(),
    }
}

fn hydrate_bodies(
    message: &mail_parser::Message<'_>,
) -> (Option<String>, Option<String>, bool, String) {
    let body_text = message.body_text(0).map(|value| value.into_owned());
    let html = message.body_html(0).map(|value| value.into_owned());

    if let Some(body_text) = body_text {
        let normalized = normalize_body_text(&body_text);
        return (Some(normalized.clone()), None, false, normalized);
    }

    if let Some(html) = html {
        let sanitized = sanitize_html_for_display(&html);
        return (
            None,
            Some(sanitized.html),
            sanitized.has_remote_content,
            sanitized.text,
        );
    }

    (None, None, false, "No preview available.".to_owned())
}

fn normalize_body_text(text: &str) -> String {
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

fn guess_role(mailbox_name: &str) -> Option<String> {
    match mailbox_name.to_ascii_lowercase().as_str() {
        "inbox" => Some("inbox".to_owned()),
        "archive" | "all mail" => Some("archive".to_owned()),
        "sent" | "sent items" | "sent mail" => Some("sent".to_owned()),
        "drafts" => Some("drafts".to_owned()),
        "trash" | "deleted items" => Some("trash".to_owned()),
        "junk" | "spam" => Some("junk".to_owned()),
        _ => None,
    }
}

fn existing_mailbox_map(mailboxes: &[MailboxSnapshot]) -> BTreeMap<&str, MailboxSnapshot> {
    mailboxes
        .iter()
        .map(|mailbox| (mailbox.name.as_str(), mailbox.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        ImapCheckpoint, ImapMailboxCheckpoint, ImapSecurity, excerpt, guess_role, parse_endpoint,
    };

    #[test]
    fn parses_imap_endpoints() {
        let endpoint = parse_endpoint("imaps://mail.example.test:993").expect("valid endpoint");
        assert_eq!(endpoint.host, "mail.example.test");
        assert_eq!(endpoint.port, 993);
        assert_eq!(endpoint.security, ImapSecurity::ImplicitTls);

        let endpoint = parse_endpoint("imap://mail.example.test:3143").expect("valid endpoint");
        assert_eq!(endpoint.host, "mail.example.test");
        assert_eq!(endpoint.port, 3143);
        assert_eq!(endpoint.security, ImapSecurity::Plain);
    }

    #[test]
    fn checkpoint_round_trip_is_stable() {
        let checkpoint = ImapCheckpoint {
            mailboxes: [(
                "Inbox".to_owned(),
                ImapMailboxCheckpoint {
                    uid_validity: Some(9),
                    uid_next: Some(34),
                    exists: 33,
                    last_seen_uid: Some(33),
                    uid_flags: BTreeMap::new(),
                },
            )]
            .into_iter()
            .collect(),
        };
        let json = serde_json::to_string(&checkpoint).expect("checkpoint should serialize");
        let parsed = ImapCheckpoint::from_json(Some(&json)).expect("checkpoint should parse");
        assert_eq!(parsed, checkpoint);
    }

    #[test]
    fn mailbox_roles_follow_common_names() {
        assert_eq!(guess_role("Inbox").as_deref(), Some("inbox"));
        assert_eq!(guess_role("Sent Items").as_deref(), Some("sent"));
        assert_eq!(guess_role("Projects"), None);
    }

    #[test]
    fn preview_excerpt_is_trimmed() {
        let preview = excerpt("one two three four five six seven", 10);
        assert!(preview.starts_with("one two th"));
    }
}
