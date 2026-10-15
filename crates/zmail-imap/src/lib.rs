//! Real IMAP adapter for Phase 2.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{Arc, Once},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use async_imap::{
    Authenticator, Client, Session,
    types::{Fetch, Flag, Mailbox, Name, NameAttribute},
};
use futures_util::TryStreamExt;
use mail_parser::{Address, MessageParser, MimeHeaders};
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
    AccountSyncSnapshot, AuthKind, HydratedBody, MailboxSnapshot, MessageAttachment,
    MessageSnapshot, OpApplyReport, PendingOp, PendingOpKind, ProtocolKind, SyncTarget,
    ThreadSnapshot,
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
// Lazy sync: headers only. Bodies (and attachment blobs) are fetched on
// demand by `fetch_message_bodies` when a message is opened.
const FETCH_QUERY: &str = "(UID INTERNALDATE FLAGS BODY.PEEK[HEADER])";
/// Full-message fetch used by the on-demand hydration path.
const HYDRATE_QUERY: &str = "(UID INTERNALDATE FLAGS BODY.PEEK[])";
/// Maximum number of attachments persisted per message.
const MAX_ATTACHMENTS_PER_MESSAGE: usize = 25;
/// Maximum size of a single persisted attachment blob.
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

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
    let attachments_dir = std::path::PathBuf::from(&target.attachments_dir);
    let identity_email = target
        .identities
        .iter()
        .find(|identity| identity.is_primary)
        .or_else(|| target.identities.first())
        .map(|identity| identity.email_address.as_str())
        .unwrap_or(target.username.as_str());
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    let condstore = session
        .capabilities()
        .await
        .map(|capabilities| capabilities.has_str("CONDSTORE"))
        .unwrap_or(false);
    let mailbox_names = list_mailboxes(&mut session).await?;
    let checkpoint = ImapCheckpoint::from_json(target.checkpoint.as_deref())?;
    let existing = existing_mailbox_map(existing_mailboxes);

    let mut next_checkpoint = BTreeMap::new();
    let mut mailboxes = Vec::with_capacity(mailbox_names.len());
    let mut refreshed = 0usize;
    let mut incremental = 0usize;
    let mut condstore_skipped = 0usize;

    for mailbox_name in mailbox_names {
        let name = mailbox_name.name().to_owned();
        // Selecting with (CONDSTORE) makes the server report the highest
        // mod-sequence, which drives the incremental flag path; servers
        // without the extension keep the plain read-only EXAMINE.
        let status = if condstore {
            session
                .select_condstore(&name)
                .await
                .with_context(|| format!("failed to select IMAP mailbox {name}"))?
        } else {
            session
                .examine(&name)
                .await
                .with_context(|| format!("failed to examine IMAP mailbox {name}"))?
        };

        let previous = checkpoint.mailboxes.get(&name).cloned();
        let mut mailbox_checkpoint = ImapMailboxCheckpoint::from_status(&status);
        next_checkpoint.insert(name.clone(), mailbox_checkpoint.clone());

        // While UIDVALIDITY is stable the existing mirror can be reused;
        // only flag changes and new mail need to be fetched.
        if previous.as_ref().and_then(|state| state.uid_validity) == status.uid_validity
            && previous.is_some()
            && let Some(existing_mailbox) = existing.get(name.as_str()).cloned()
        {
            let last_seen_uid = previous.as_ref().and_then(|state| state.last_seen_uid);
            let new_uids = new_mailbox_uids(&mut session, last_seen_uid).await?;
            let previous_exists = previous.as_ref().map(|state| state.exists).unwrap_or(0);
            let previous_modseq = previous.as_ref().and_then(|state| state.highest_modseq);

            // CONDSTORE fast path: an unchanged mod-sequence with the same
            // message count proves the mailbox state is identical, so no
            // flag data is fetched at all. When the message count is stable
            // but the mod-sequence advanced, only the changed messages are
            // fetched and merged into the previous flag map.
            let flag_map = if let (true, Some(previous_modseq)) = (condstore, previous_modseq) {
                if status.exists == previous_exists
                    && status.highest_modseq == Some(previous_modseq)
                {
                    mailboxes.push(existing_mailbox);
                    mailbox_checkpoint.last_seen_uid = last_seen_uid;
                    mailbox_checkpoint.uid_flags = previous
                        .as_ref()
                        .map(|state| state.uid_flags.clone())
                        .unwrap_or_default();
                    next_checkpoint.insert(name, mailbox_checkpoint);
                    condstore_skipped += 1;
                    continue;
                }
                if status.exists == previous_exists {
                    let changed = fetch_changed_flags(&mut session, previous_modseq).await?;
                    let mut merged = previous
                        .as_ref()
                        .map(|state| state.uid_flags.clone())
                        .unwrap_or_default();
                    for (uid, flags) in changed {
                        merged.insert(uid, flags);
                    }
                    merged
                } else {
                    // Expunges or a mod-sequence reset mean the merged map
                    // cannot be trusted; a full scan is the safe fallback.
                    fetch_flag_map(&mut session).await?
                }
            } else {
                fetch_flag_map(&mut session).await?
            };
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
                append_new_threads(
                    &mut session,
                    &mut merged,
                    &new_uids,
                    identity_email,
                    &attachments_dir,
                )
                .await?;
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
        let (snapshot, uid_flags) = fetch_mailbox_snapshot(
            &mut session,
            &name,
            guess_role(&name),
            identity_email,
            &attachments_dir,
        )
        .await?;
        mailbox_checkpoint.last_seen_uid = uid_flags.keys().last().copied();
        mailbox_checkpoint.uid_flags = uid_flags;
        next_checkpoint.insert(name, mailbox_checkpoint);
        mailboxes.push(snapshot);
    }

    let _ = session.logout().await;

    let detail = if refreshed == 0 && incremental == 0 && condstore_skipped > 0 {
        format!(
            "IMAP mailbox state unchanged for {} via CONDSTORE",
            target.display_name
        )
    } else if refreshed == 0 && incremental == 0 {
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
/// Fetches the full bodies (and attachments) of specific messages on demand.
///
/// UIDs must belong to `mailbox_name`. Returns one [`HydratedBody`] per UID
/// found, in the order given; missing UIDs are skipped.
pub async fn fetch_message_bodies(
    target: &SyncTarget,
    secret: &str,
    mailbox_name: &str,
    uids: &[u32],
) -> Result<Vec<(u32, HydratedBody)>> {
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }
    if uids.is_empty() {
        return Ok(Vec::new());
    }

    let endpoint = parse_endpoint(&target.endpoint)?;
    let attachments_dir = std::path::PathBuf::from(&target.attachments_dir);
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    session
        .examine(mailbox_name)
        .await
        .with_context(|| format!("failed to open IMAP mailbox {mailbox_name} for hydration"))?;

    let uid_set = uids
        .iter()
        .map(|uid| uid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let fetches = session
        .uid_fetch(&uid_set, HYDRATE_QUERY)
        .await
        .with_context(|| format!("failed to fetch bodies for UID set {uid_set}"))?
        .try_collect::<Vec<_>>()
        .await
        .context("failed to stream the hydration fetches")?;

    let mut bodies = Vec::with_capacity(fetches.len());
    for fetch in fetches {
        let Some(uid) = fetch.uid else { continue };
        let Some(raw) = fetch.body() else { continue };
        let Some(message) = MessageParser::default().parse(raw) else {
            continue;
        };
        let (body_text, sanitized_html, has_remote_content, preview_source) =
            hydrate_bodies(&message);
        let attachments =
            persist_attachments(&message, mailbox_name, &uid.to_string(), &attachments_dir);
        bodies.push((
            uid,
            HydratedBody {
                body_text,
                sanitized_html,
                has_remote_content,
                body_preview: excerpt(&preview_source, 180),
                attachments,
            },
        ));
    }

    Ok(bodies)
}

/// Waits until the mail server reports a change or the bounded wait
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
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
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

/// Empties the server's trash mailbox, expunging every message in it.
///
/// Deletion normally moves messages to trash and marks destroyed
/// messages `\Deleted` without expunging, so storage would grow without
/// bound. This is the explicit, user-initiated expunge: it selects the
/// trash mailbox read-write, marks every message `\Deleted`, and issues
/// `EXPUNGE`, which affects only the selected mailbox. Returns the
/// number of messages expunged.
pub async fn empty_trash(
    target: &SyncTarget,
    secret: &str,
    trash_mailbox_name: &str,
) -> Result<u32> {
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }

    let endpoint = parse_endpoint(&target.endpoint)?;
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    let status = session
        .select(trash_mailbox_name)
        .await
        .with_context(|| format!("failed to select the trash mailbox {trash_mailbox_name}"))?;

    if status.exists > 0 {
        store_flags(
            &mut session,
            "1:*".to_owned(),
            "+FLAGS (\\Deleted)".to_owned(),
        )
        .await?;
        let expunged = session
            .expunge()
            .await
            .context("failed to expunge the trash mailbox")?
            .try_collect::<Vec<_>>()
            .await
            .context("failed to stream the trash expunge")?;
        let _ = session.logout().await;
        Ok(expunged.len() as u32)
    } else {
        let _ = session.logout().await;
        Ok(0)
    }
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
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
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
    /// Highest mod-sequence seen at the last sync (RFC 7162 CONDSTORE).
    #[serde(default)]
    highest_modseq: Option<u64>,
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
            highest_modseq: status.highest_modseq,
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

/// SASL XOAUTH2 string for IMAP bearer-token authentication.
///
/// The value is the clear-text mechanism response; the IMAP client base64-encodes
/// it before sending it to the server.
fn xoauth2_response(username: &str, token: &str) -> Vec<u8> {
    format!("user={username}\x01auth=Bearer {token}\x01\x01").into_bytes()
}

/// [`Authenticator`] that answers the XOAUTH2 challenge with a bearer token.
struct Xoauth2Authenticator<'a> {
    username: &'a str,
    token: &'a str,
}

impl Authenticator for Xoauth2Authenticator<'_> {
    type Response = Vec<u8>;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        xoauth2_response(self.username, self.token)
    }
}

async fn connect_session(
    endpoint: &ImapEndpoint,
    username: &str,
    secret: &str,
    auth: AuthKind,
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

    match auth {
        AuthKind::Password => client
            .login(username, secret)
            .await
            .map_err(|(error, _)| anyhow!(error))
            .context("IMAP authentication failed"),
        AuthKind::OAuth2 => {
            let authenticator = Xoauth2Authenticator {
                username,
                token: secret,
            };
            client
                .authenticate("XOAUTH2", authenticator)
                .await
                .map_err(|(error, _)| anyhow!(error))
                .context("IMAP XOAUTH2 authentication failed")
        }
    }
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
    attachments_dir: &Path,
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

    let mut conversations: BTreeMap<String, ImapThreadAccumulator> = BTreeMap::new();
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
            accumulate_fetch(
                &mut conversations,
                fetch,
                mailbox_name,
                identity_email,
                attachments_dir,
            );
        }
    }

    let mut threads: Vec<ThreadSnapshot> = conversations
        .into_values()
        .map(ImapThreadAccumulator::into_thread_snapshot)
        .collect();
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
/// Fetches only the flags of messages whose mod-sequence exceeds `since`
/// (RFC 7162 CONDSTORE). Implicitly enables CONDSTORE for the session.
async fn fetch_changed_flags(
    session: &mut Session<DynImapStream>,
    since: u64,
) -> Result<BTreeMap<u32, Vec<String>>> {
    let fetches = session
        .uid_fetch("1:*", &format!("(UID FLAGS) (CHANGEDSINCE {since})"))
        .await
        .context("failed to fetch the changed message flags")?
        .try_collect::<Vec<_>>()
        .await
        .context("failed to stream the changed message flags")?;

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
    attachments_dir: &Path,
) -> Result<()> {
    let mailbox_name = mailbox.name.clone();
    let mut conversations: BTreeMap<String, ImapThreadAccumulator> = BTreeMap::new();
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
            accumulate_fetch(
                &mut conversations,
                fetch,
                &mailbox_name,
                identity_email,
                attachments_dir,
            );
        }
    }

    for new_thread in conversations
        .into_values()
        .map(ImapThreadAccumulator::into_thread_snapshot)
    {
        // Replies join the conversation already present in the mirror
        // instead of opening a duplicate thread row.
        let existing = mailbox
            .threads
            .iter_mut()
            .find(|thread| thread.remote_thread_id == new_thread.remote_thread_id);
        match existing {
            Some(existing) => {
                existing.messages.extend(new_thread.messages);
                existing
                    .messages
                    .sort_by(|left, right| left.received_at.cmp(&right.received_at));
                recompute_thread_aggregates(existing);
            }
            None => mailbox.threads.push(new_thread),
        }
    }
    mailbox
        .threads
        .sort_by(|left, right| right.latest_message_at.cmp(&left.latest_message_at));
    Ok(())
}

/// Recomputes the aggregate thread fields from its messages.
///
/// The messages are expected to be ordered by `received_at`, so the last
/// message carries the newest display fields.
fn recompute_thread_aggregates(thread: &mut ThreadSnapshot) {
    if let Some(newest) = thread.messages.last() {
        thread.latest_message_at = newest.received_at.clone();
        thread.subject = newest.subject.clone();
        thread.preview = newest.body_preview.clone();
        thread.participants = format!("{} -> {}", newest.sender, newest.recipients);
    }
    thread.unread_count = thread
        .messages
        .iter()
        .filter(|message| !message.is_read)
        .count();
    thread.is_flagged = thread.messages.iter().any(|message| message.is_flagged);
}

/// Accumulates the messages of one IMAP conversation.
#[derive(Debug)]
struct ImapThreadAccumulator {
    remote_thread_id: String,
    subject: String,
    participants: String,
    preview: String,
    is_flagged: bool,
    has_attachments: bool,
    unread_count: usize,
    latest_message_at: String,
    messages: Vec<MessageSnapshot>,
}

impl ImapThreadAccumulator {
    /// Adds one message to the conversation, following the latest message
    /// for the display fields, like the JMAP accumulator does.
    fn push_message(&mut self, message: MessageSnapshot) {
        if message.received_at > self.latest_message_at {
            self.latest_message_at = message.received_at.clone();
            self.subject = message.subject.clone();
            self.preview = message.body_preview.clone();
            self.participants = format!("{} -> {}", message.sender, message.recipients);
        }
        self.is_flagged |= message.is_flagged;
        self.unread_count += usize::from(!message.is_read);
        self.messages.push(message);
    }
}

/// Derives the conversation key of a message from its threading headers.
///
/// The first `References` entry is the root of the chain; replies without
/// a `References` header fall back to `In-Reply-To`, then to the
/// message's own `Message-ID`. Messages with no threading headers at
/// all stay standalone, keyed by their UID (the pre-conversation
/// behavior).
fn conversation_key(message: &mail_parser::Message<'_>, uid: Option<u32>) -> String {
    if let Some(references) = message.references().as_text_list()
        && let Some(root) = references.first()
        && !root.trim().is_empty()
    {
        return root.trim().to_owned();
    }
    if let Some(in_reply_to) = message.in_reply_to().as_text_list()
        && let Some(parent) = in_reply_to.first()
        && !parent.trim().is_empty()
    {
        return parent.trim().to_owned();
    }
    if let Some(in_reply_to) = message.in_reply_to().as_text()
        && !in_reply_to.trim().is_empty()
    {
        return in_reply_to.trim().to_owned();
    }
    if let Some(message_id) = message.message_id().filter(|id| !id.trim().is_empty()) {
        return message_id.trim().to_owned();
    }
    format!("uid:{}", uid.map(|uid| uid.to_string()).unwrap_or_default())
}

/// Accumulates one fetched message into its conversation bucket.
fn accumulate_fetch(
    threads: &mut BTreeMap<String, ImapThreadAccumulator>,
    fetch: Fetch,
    mailbox_name: &str,
    identity_email: &str,
    attachments_dir: &Path,
) {
    // Header-only syncs carry the message in the BODY[HEADER] section;
    // full fetches (hydration) carry it in BODY[].
    let raw = match fetch.body().or_else(|| fetch.header()) {
        Some(raw) => raw,
        None => return,
    };
    let message = match MessageParser::default().parse(raw) {
        Some(message) => message,
        None => return,
    };
    let subject = message
        .subject()
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "(no subject)".to_owned());
    let sender = first_address(message.from()).unwrap_or_else(|| "Unknown sender".to_owned());
    let reply_to = first_address(message.reply_to());
    let recipients = format_recipients(message.to(), identity_email);
    let (body_text, sanitized_html, has_remote_content, preview_source) = hydrate_bodies(&message);
    let preview = excerpt(&preview_source, 180);
    let has_attachments = message.attachment_count() > 0;
    let remote_stub = fetch.uid.map(|uid| uid.to_string()).unwrap_or_default();
    let attachments = persist_attachments(&message, mailbox_name, &remote_stub, attachments_dir);
    let received_at = fetch
        .internal_date()
        .map(|date| date.to_rfc3339())
        .or_else(|| message.date().map(|date| date.to_rfc3339()))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
    let is_read = fetch.flags().any(|flag| matches!(flag, Flag::Seen));
    let is_flagged = fetch.flags().any(|flag| matches!(flag, Flag::Flagged));
    let uid = fetch.uid;
    let remote_id = uid.map(|uid| uid.to_string()).unwrap_or_default();
    let message_snapshot = MessageSnapshot {
        remote_id,
        identity_email: identity_email.to_owned(),
        sender,
        reply_to,
        recipients,
        subject,
        body_preview: preview,
        body_text,
        sanitized_html,
        has_remote_content,
        is_read,
        is_flagged,
        received_at: received_at.clone(),
        attachments,
    };
    let key = conversation_key(&message, uid);

    let entry = match threads.entry(key.clone()) {
        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::btree_map::Entry::Vacant(entry) => entry.insert(ImapThreadAccumulator {
            remote_thread_id: key,
            subject: message_snapshot.subject.clone(),
            participants: format!(
                "{} -> {}",
                message_snapshot.sender, message_snapshot.recipients
            ),
            preview: message_snapshot.body_preview.clone(),
            is_flagged: false,
            has_attachments: false,
            unread_count: 0,
            latest_message_at: received_at,
            messages: Vec::new(),
        }),
    };
    entry.has_attachments |= has_attachments;
    entry.push_message(message_snapshot);
}

/// Persists the attachment parts of a parsed message into the store's
/// attachments directory and returns their metadata.
///
/// Blobs go straight to disk; only the raw fetched message is held in memory,
/// so attachment bytes never round-trip through the store or the database.
/// Parts beyond the per-message count or single-part size caps are skipped
/// rather than half-persisted.
fn persist_attachments(
    message: &mail_parser::Message<'_>,
    mailbox_name: &str,
    remote_id: &str,
    attachments_dir: &Path,
) -> Vec<MessageAttachment> {
    let mut attachments = Vec::new();
    if message.attachment_count() == 0 {
        return attachments;
    }

    if !attachments_dir.is_dir() && fs::create_dir_all(attachments_dir).is_err() {
        // Without a writable blob directory the sync must not fail: the
        // message still stores, the attachment parts are simply not kept.
        return attachments;
    }

    for (index, part) in message.attachments().enumerate() {
        if attachments.len() >= MAX_ATTACHMENTS_PER_MESSAGE {
            break;
        }
        let contents = part.contents();
        if contents.len() > MAX_ATTACHMENT_BYTES {
            continue;
        }
        let Some(filename) = part.attachment_name().map(str::to_owned) else {
            continue;
        };
        if filename.trim().is_empty() {
            continue;
        }
        let content_type = part
            .content_type()
            .map(|content_type| match content_type.subtype() {
                Some(subtype) => format!("{}/{}", content_type.ctype(), subtype),
                None => content_type.ctype().to_owned(),
            })
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let storage_name = format!(
            "incoming-{}-{remote_id}-{index}-{}",
            sanitize_component(mailbox_name),
            sanitize_component(&filename),
        );
        let storage_path = attachments_dir.join(&storage_name);
        if fs::write(&storage_path, contents).is_err() {
            continue;
        }
        attachments.push(MessageAttachment {
            filename,
            content_type,
            size_bytes: contents.len() as u64,
            storage_path: storage_name,
        });
    }

    attachments
}

/// Restricts a filename- or mailbox-derived path component to safe characters.
fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect()
}

impl ImapThreadAccumulator {
    /// Converts the accumulator into a store snapshot.
    fn into_thread_snapshot(mut self) -> ThreadSnapshot {
        self.messages
            .sort_by(|left, right| left.received_at.cmp(&right.received_at));
        ThreadSnapshot {
            remote_thread_id: self.remote_thread_id,
            subject: self.subject,
            participants: self.participants,
            preview: self.preview,
            is_flagged: self.is_flagged,
            has_attachments: self.has_attachments,
            unread_count: self.unread_count,
            latest_message_at: self.latest_message_at,
            messages: self.messages,
        }
    }
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
    let body_text = message
        .body_text(0)
        .map(|value| value.into_owned())
        .filter(|value| !value.trim().is_empty());
    // Header-only parses get a synthesized "<html><body></body></html>"
    // shell from mail-parser; it carries no visible text and must surface as
    // an un-hydrated message rather than as an (empty) HTML body.
    let html = message
        .body_html(0)
        .map(|value| value.into_owned())
        .filter(|value| html_has_visible_text(value));

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

    (None, None, false, "Body not downloaded.".to_owned())
}

/// Returns whether an HTML fragment carries text outside its tags.
fn html_has_visible_text(html: &str) -> bool {
    let mut text = String::new();
    let mut inside_tag = false;
    for character in html.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => text.push(character),
            _ => {}
        }
    }
    !text.trim().is_empty()
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
        ImapCheckpoint, ImapMailboxCheckpoint, ImapSecurity, conversation_key, excerpt, guess_role,
        hydrate_bodies, parse_endpoint, persist_attachments, recompute_thread_aggregates,
        xoauth2_response,
    };
    use zmail_core::{MessageSnapshot, ThreadSnapshot};

    #[test]
    fn xoauth2_response_matches_the_sasl_mechanism_format() {
        let response = xoauth2_response("user@example.test", "token-1234");
        assert_eq!(
            String::from_utf8(response).expect("the XOAUTH2 response is UTF-8"),
            "user=user@example.test\x01auth=Bearer token-1234\x01\x01"
        );
    }

    #[test]
    fn persists_attachment_parts_from_a_parsed_message() {
        let raw = concat!(
            "From: sender@example.test\r\n",
            "Subject: with attachment\r\n",
            "To: rcpt@example.test\r\n",
            "Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n",
            "\r\n",
            "--BOUND\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "Body text.\r\n",
            "--BOUND\r\n",
            "Content-Type: text/plain; name=\"notes.txt\"\r\n",
            "Content-Disposition: attachment; filename=\"notes.txt\"\r\n",
            "\r\n",
            "attachment payload\r\n",
            "--BOUND--\r\n",
        );
        let message = mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .expect("the message should parse");

        let dir = std::env::temp_dir().join(format!("zmail-imap-attach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let attachments = persist_attachments(&message, "Inbox", "42", &dir);
        assert_eq!(attachments.len(), 1, "one attachment should persist");
        let attachment = &attachments[0];
        assert_eq!(attachment.filename, "notes.txt");
        assert_eq!(attachment.content_type, "text/plain");
        assert_eq!(attachment.size_bytes, "attachment payload".len() as u64);
        assert_eq!(attachment.storage_path, "incoming-Inbox-42-0-notes.txt");

        let blob_path = dir.join(&attachment.storage_path);
        let blob = std::fs::read(&blob_path).expect("the blob should be readable");
        assert_eq!(blob, b"attachment payload");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_header_only_messages_for_lazy_sync() {
        let raw = concat!(
            "From: sender@example.test\r\n",
            "Subject: header only\r\n",
            "To: rcpt@example.test\r\n",
            "Message-ID: <header-only@example.test>\r\n",
            "\r\n",
        );
        let message = mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .expect("a header-only message should parse");
        assert_eq!(message.subject(), Some("header only"));

        // A header-only parse must surface as an un-hydrated message: no
        // body fields and the lazy-sync placeholder preview.
        let (body_text, sanitized_html, has_remote_content, preview) = hydrate_bodies(&message);
        assert!(body_text.is_none());
        assert!(sanitized_html.is_none());
        assert!(!has_remote_content);
        assert_eq!(preview, "Body not downloaded.");
    }
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
                    highest_modseq: Some(7001),
                },
            )]
            .into_iter()
            .collect(),
        };
        let json = serde_json::to_string(&checkpoint).expect("checkpoint should serialize");
        let parsed = ImapCheckpoint::from_json(Some(&json)).expect("checkpoint should parse");
        assert_eq!(parsed, checkpoint);
    }

    /// Parses a raw RFC822 message for the conversation tests.
    fn parse_raw(raw: &[u8]) -> mail_parser::Message<'_> {
        mail_parser::MessageParser::default()
            .parse(raw)
            .expect("the raw message should parse")
    }

    #[test]
    fn conversation_key_follows_the_references_root() {
        let raw = b"From: alice@example.com\r\n\r\nBody\r\n";
        let root = conversation_key(&parse_raw(raw), Some(1));
        assert_eq!(
            root, "uid:1",
            "a message without threading headers stays standalone"
        );

        let raw = b"Message-ID: <root@example.com>\r\n\r\nBody\r\n";
        let key = conversation_key(&parse_raw(raw), Some(1));
        assert_eq!(key, "root@example.com");

        let raw =
            b"Message-ID: <reply@example.com>\r\nIn-Reply-To: <root@example.com>\r\n\r\nBody\r\n";
        let key = conversation_key(&parse_raw(raw), Some(2));
        assert_eq!(
            key, "root@example.com",
            "In-Reply-To joins the parent conversation"
        );

        let raw = b"Message-ID: <reply2@example.com>\r\nReferences: <root@example.com> <reply@example.com>\r\n\r\nBody\r\n";
        let key = conversation_key(&parse_raw(raw), Some(3));
        assert_eq!(
            key, "root@example.com",
            "References keys by the root of the chain"
        );
    }

    #[test]
    fn conversation_key_falls_back_to_the_uid() {
        let raw = b"From: bob@example.com\r\nSubject: no headers\r\n\r\nBody\r\n";
        let key = conversation_key(&parse_raw(raw), Some(42));
        assert_eq!(key, "uid:42");
    }

    #[test]
    fn recompute_thread_aggregates_follows_the_newest_message() {
        let older = MessageSnapshot {
            remote_id: "1".to_owned(),
            identity_email: "me@example.com".to_owned(),
            sender: "Older Sender".to_owned(),
            reply_to: None,
            recipients: "me@example.com".to_owned(),
            subject: "Older subject".to_owned(),
            body_preview: "older preview".to_owned(),
            body_text: None,
            sanitized_html: None,
            has_remote_content: false,
            is_read: false,
            is_flagged: false,
            received_at: "2026-01-01T00:00:00Z".to_owned(),
            attachments: Vec::new(),
        };
        let newer = MessageSnapshot {
            remote_id: "2".to_owned(),
            identity_email: "me@example.com".to_owned(),
            sender: "Newer Sender".to_owned(),
            reply_to: None,
            recipients: "me@example.com".to_owned(),
            subject: "Newer subject".to_owned(),
            body_preview: "newer preview".to_owned(),
            body_text: None,
            sanitized_html: None,
            has_remote_content: false,
            is_read: true,
            is_flagged: true,
            received_at: "2026-02-01T00:00:00Z".to_owned(),
            attachments: Vec::new(),
        };
        let mut thread = ThreadSnapshot {
            remote_thread_id: "root@example.com".to_owned(),
            subject: String::new(),
            participants: String::new(),
            preview: String::new(),
            is_flagged: false,
            has_attachments: false,
            unread_count: 0,
            latest_message_at: String::new(),
            messages: vec![older, newer],
        };
        recompute_thread_aggregates(&mut thread);
        assert_eq!(thread.subject, "Newer subject");
        assert_eq!(thread.preview, "newer preview");
        assert_eq!(thread.latest_message_at, "2026-02-01T00:00:00Z");
        assert_eq!(thread.unread_count, 1, "the older message stays unread");
        assert!(thread.is_flagged);
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
