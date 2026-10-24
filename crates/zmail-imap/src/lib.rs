//! Real IMAP adapter for Phase 2.

mod qresync;
mod watch;

use std::{
    collections::BTreeMap,
    sync::{Arc, Once},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use async_imap::{
    Authenticator, Client, Session,
    types::{Fetch, Flag, Mailbox, Name, NameAttribute},
};
use futures_util::TryStreamExt;
use mail_parser::{Address, MessageParser, MimeHeaders, PartType};
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
    AccountSyncSnapshot, AuthKind, BlobStore, HydratedBody, MailboxSnapshot, MessageAttachment,
    MessageSnapshot, OpApplyReport, PendingOp, PendingOpKind, ProtocolKind, SyncTarget,
    ThreadSnapshot,
};
use zmail_security::sanitize_html_for_display;

pub use watch::{
    IDLE_RENEW_INTERVAL, MAX_WATCH_CONNECTIONS, STATUS_POLL_INTERVAL, WatchEnd, WatchEvent,
    WatchPlan, plan_watch, poll_status, watch_idle, watch_notify,
};

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
    let blobs = target.blobs.clone();
    let identity_email = target
        .identities
        .iter()
        .find(|identity| identity.is_primary)
        .or_else(|| target.identities.first())
        .map(|identity| identity.email_address.as_str())
        .unwrap_or(target.username.as_str());
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    let (condstore, qresync_offered) = session
        .capabilities()
        .await
        .map(|capabilities| {
            (
                capabilities.has_str("CONDSTORE"),
                capabilities.has_str("QRESYNC"),
            )
        })
        .unwrap_or((false, false));
    // QRESYNC must be enabled before the first SELECT (RFC 7162 section
    // 3.2.3); a refusal keeps the CONDSTORE path.
    let qresync = qresync_offered && qresync::enable(&mut session).await;
    let mailbox_names = list_mailboxes(&mut session).await?;
    // Roles, delimiters and parents come from the listing, so they are
    // current even for a mailbox whose mirror is reused unchanged below.
    let listings = mailbox_names
        .iter()
        .map(|name| name.name().to_owned())
        .zip(mailbox_listings(&mailbox_names))
        .collect::<BTreeMap<_, _>>();
    let checkpoint = ImapCheckpoint::from_json(target.checkpoint.as_deref())?;
    let existing = existing_mailbox_map(existing_mailboxes);

    let mut next_checkpoint = BTreeMap::new();
    let mut mailboxes = Vec::with_capacity(mailbox_names.len());
    let mut refreshed = 0usize;
    let mut incremental = 0usize;
    let mut condstore_skipped = 0usize;
    let mut qresync_opened = 0usize;

    for mailbox_name in mailbox_names {
        let name = mailbox_name.name().to_owned();
        let previous = checkpoint.mailboxes.get(&name).cloned();
        // A mailbox the mirror already holds, with a stored UIDVALIDITY and
        // mod-sequence, opens with QRESYNC: the server reports what was
        // expunged and what changed since in the same round trip.
        let qresync_state = previous
            .as_ref()
            .filter(|_| qresync && existing.contains_key(name.as_str()))
            .and_then(|state| Some((state.uid_validity?, state.highest_modseq?, state)));
        let mut qresync_delta = None;
        // Selecting with (CONDSTORE) makes the server report the highest
        // mod-sequence, which drives the incremental flag path; servers
        // without the extension keep the plain read-only EXAMINE.
        let status = if let Some((uid_validity, modseq, state)) = qresync_state {
            let known_uids = qresync::uid_set(state.uid_flags.keys().copied());
            let delta =
                qresync::examine(&mut session, &name, uid_validity, modseq, Some(&known_uids))
                    .await?;
            let status = delta.status.clone();
            // A changed UIDVALIDITY voids the stored state; the server
            // reported nothing and the mailbox refreshes in full below.
            if status.uid_validity == Some(uid_validity) {
                qresync_opened += 1;
                qresync_delta = Some(delta);
            }
            status
        } else if condstore {
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

        let mut mailbox_checkpoint = ImapMailboxCheckpoint::from_status(&status);
        next_checkpoint.insert(name.clone(), mailbox_checkpoint.clone());

        // While UIDVALIDITY is stable the existing mirror can be reused;
        // only flag changes and new mail need to be fetched.
        if previous.as_ref().and_then(|state| state.uid_validity) == status.uid_validity
            && previous.is_some()
            && let Some(existing_mailbox) = existing.get(name.as_str()).cloned()
        {
            let last_seen_uid = resume_uid(previous.as_ref(), &existing_mailbox);
            let new_uids = new_mailbox_uids(&mut session, last_seen_uid).await?;
            let previous_exists = previous.as_ref().map(|state| state.exists).unwrap_or(0);
            let previous_modseq = previous.as_ref().and_then(|state| state.highest_modseq);

            // CONDSTORE fast path: an unchanged mod-sequence with the same
            // message count proves the mailbox state is identical, so no
            // flag data is fetched at all. When the message count is stable
            // but the mod-sequence advanced, only the changed messages are
            // fetched and merged into the previous flag map.
            let flag_map = if let Some(delta) = &qresync_delta {
                // QRESYNC already reported every expunge and flag change,
                // so the stored map is patched without fetching anything.
                let merged = qresync::apply_delta(
                    &previous
                        .as_ref()
                        .map(|state| state.uid_flags.clone())
                        .unwrap_or_default(),
                    &delta.vanished,
                    &delta.changed,
                );
                if delta.vanished.is_empty()
                    && delta.changed.is_empty()
                    && new_uids.is_empty()
                    && status.exists == previous_exists
                {
                    mailboxes.push(existing_mailbox);
                    mailbox_checkpoint.last_seen_uid = Some(last_seen_uid);
                    mailbox_checkpoint.uid_flags = merged;
                    next_checkpoint.insert(name, mailbox_checkpoint);
                    condstore_skipped += 1;
                    continue;
                }
                merged
            } else if let (true, Some(previous_modseq)) = (condstore, previous_modseq) {
                if status.exists == previous_exists
                    && status.highest_modseq == Some(previous_modseq)
                {
                    mailboxes.push(existing_mailbox);
                    mailbox_checkpoint.last_seen_uid = Some(last_seen_uid);
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
                mailbox_checkpoint.last_seen_uid = Some(last_seen_uid);
                mailbox_checkpoint.uid_flags = flag_map;
                next_checkpoint.insert(name, mailbox_checkpoint);
                continue;
            }

            incremental += 1;
            let mut merged = existing_mailbox;
            merge_flag_changes(&mut merged, &flag_map);
            if !new_uids.is_empty() {
                append_new_threads(&mut session, &mut merged, &new_uids, identity_email, &blobs)
                    .await?;
            }
            mailbox_checkpoint.last_seen_uid =
                Some(new_uids.last().copied().unwrap_or(0).max(last_seen_uid));
            mailbox_checkpoint.uid_flags = flag_map;
            next_checkpoint.insert(name, mailbox_checkpoint);
            mailboxes.push(merged);
            continue;
        }

        refreshed += 1;
        let (snapshot, uid_flags) =
            fetch_mailbox_snapshot(&mut session, &name, identity_email, &blobs).await?;
        mailbox_checkpoint.last_seen_uid = uid_flags.keys().last().copied();
        mailbox_checkpoint.uid_flags = uid_flags;
        next_checkpoint.insert(name, mailbox_checkpoint);
        mailboxes.push(snapshot);
    }

    let _ = session.logout().await;

    for mailbox in &mut mailboxes {
        if let Some(listing) = listings.get(&mailbox.name) {
            mailbox.role = listing.role.clone();
            mailbox.delimiter = listing.delimiter.clone();
            mailbox.parent = listing.parent.clone();
        }
    }

    let detail = if refreshed == 0 && incremental == 0 && condstore_skipped > 0 {
        format!(
            "IMAP mailbox state unchanged for {} via {}",
            target.display_name,
            if qresync_opened > 0 {
                "QRESYNC"
            } else {
                "CONDSTORE"
            }
        )
    } else if refreshed == 0 && incremental == 0 {
        format!("IMAP mailbox state unchanged for {}", target.display_name)
    } else {
        format!(
            "IMAP sync complete against {} with {refreshed} full and {incremental} incremental mailbox refreshes{}",
            endpoint.host,
            if qresync_opened > 0 {
                format!(" ({qresync_opened} opened with QRESYNC)")
            } else {
                String::new()
            }
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
    let blobs = target.blobs.clone();
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

    Ok(fetches
        .iter()
        .filter_map(|fetch| hydrated_body_from_fetch(fetch, mailbox_name, &blobs))
        .collect())
}

/// Parses one full-message fetch into the stored body fields, persisting
/// its attachment blobs. Returns `None` for fetches without a UID or body.
fn hydrated_body_from_fetch(
    fetch: &Fetch,
    mailbox_name: &str,
    blobs: &BlobStore,
) -> Option<(u32, HydratedBody)> {
    let uid = fetch.uid?;
    let body = hydrated_body_from_rfc822(fetch.body()?, mailbox_name, &uid.to_string(), blobs)?;
    Some((uid, body))
}

/// Parses a full RFC 822 message into the stored body fields, through the
/// same sanitizer and attachment path IMAP hydration uses, persisting its
/// attachment blobs through `blobs`, which encrypts them (ADR 0005). Adapters that rebuild RFC 822 themselves (Proton) use
/// it so every protocol renders and stores mail the same way. Returns
/// `None` when the bytes do not parse as a message.
pub fn hydrated_body_from_rfc822(
    raw: &[u8],
    mailbox_name: &str,
    remote_id: &str,
    blobs: &BlobStore,
) -> Option<HydratedBody> {
    let message = MessageParser::default().parse(raw)?;
    let (body_text, sanitized_html, has_remote_content, preview_source) = hydrate_bodies(&message);
    let attachments = persist_attachments(&message, mailbox_name, remote_id, blobs);
    Some(HydratedBody {
        body_text,
        sanitized_html,
        has_remote_content,
        body_preview: excerpt(&preview_source, 180),
        attachments,
    })
}

/// Bodies fetched for background indexing from one mailbox.
#[derive(Debug, Default)]
pub struct IndexedBodies {
    /// Hydrated bodies by UID.
    pub bodies: Vec<(u32, HydratedBody)>,
    /// UIDs skipped because the message is larger than the size cap; they
    /// stay lazy and hydrate when opened.
    pub oversized: Vec<u32>,
}

/// One IMAP connection reused across the batches of a background indexing
/// pass, so indexing a large mailbox does not log in once per batch.
pub struct BodyIndexSession {
    session: Session<DynImapStream>,
    blobs: BlobStore,
    examined: Option<String>,
}

impl std::fmt::Debug for BodyIndexSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BodyIndexSession")
            .field("examined", &self.examined)
            .finish_non_exhaustive()
    }
}

impl BodyIndexSession {
    /// Connects and authenticates for background body indexing.
    pub async fn connect(target: &SyncTarget, secret: &str) -> Result<Self> {
        if target.protocol != ProtocolKind::Imap {
            bail!("IMAP adapter received a non-IMAP sync target");
        }
        let endpoint = parse_endpoint(&target.endpoint)?;
        let session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
        Ok(Self {
            session,
            blobs: target.blobs.clone(),
            examined: None,
        })
    }

    /// Fetches the bodies of `uids` in `mailbox_name`, skipping messages
    /// whose `RFC822.SIZE` exceeds `max_message_bytes`.
    ///
    /// Sizes are fetched first so oversized messages (usually large
    /// attachments) never download in the background. The mailbox is
    /// opened read-only with `EXAMINE` and bodies use `BODY.PEEK[]`, so
    /// indexing never marks mail as read. UIDs the server no longer has
    /// appear in neither list.
    pub async fn fetch(
        &mut self,
        mailbox_name: &str,
        uids: &[u32],
        max_message_bytes: u32,
    ) -> Result<IndexedBodies> {
        if uids.is_empty() {
            return Ok(IndexedBodies::default());
        }
        if self.examined.as_deref() != Some(mailbox_name) {
            self.examined = None;
            self.session.examine(mailbox_name).await.with_context(|| {
                format!("failed to open IMAP mailbox {mailbox_name} for indexing")
            })?;
            self.examined = Some(mailbox_name.to_owned());
        }

        let sizes = self
            .session
            .uid_fetch(uid_set(uids), "(UID RFC822.SIZE)")
            .await
            .with_context(|| format!("failed to fetch message sizes in {mailbox_name}"))?
            .try_collect::<Vec<_>>()
            .await
            .context("failed to stream the message sizes")?;
        let mut within = Vec::new();
        let mut result = IndexedBodies::default();
        for fetch in sizes {
            let Some(uid) = fetch.uid else { continue };
            if !uids.contains(&uid) {
                continue;
            }
            match fetch.size {
                Some(size) if size > max_message_bytes => result.oversized.push(uid),
                _ => within.push(uid),
            }
        }
        if within.is_empty() {
            return Ok(result);
        }

        let fetches = self
            .session
            .uid_fetch(uid_set(&within), HYDRATE_QUERY)
            .await
            .with_context(|| format!("failed to fetch bodies in {mailbox_name} for indexing"))?
            .try_collect::<Vec<_>>()
            .await
            .context("failed to stream the indexing fetches")?;
        result.bodies = fetches
            .iter()
            .filter(|fetch| fetch.uid.is_some_and(|uid| within.contains(&uid)))
            .filter_map(|fetch| hydrated_body_from_fetch(fetch, mailbox_name, &self.blobs))
            .collect();
        Ok(result)
    }

    /// Logs out, ignoring errors: the pass is over either way.
    pub async fn logout(mut self) {
        let _ = self.session.logout().await;
    }
}

/// Searches the given mailboxes on the server for `query` and returns the
/// newest `max_hits_per_mailbox` matching UIDs of each.
///
/// This is the fallback for mail whose body the local index does not hold
/// yet. Free text and the header and flag operators map onto `UID SEARCH`
/// keys (`TEXT`, `FROM`, `TO`, `SUBJECT`, `SEEN`/`UNSEEN`, `FLAGGED`, `NOT`).
/// `has:attachment` and dates have no exact IMAP equivalent (IMAP dates are
/// whole days in the server's zone), so they are left out here and the
/// store re-checks every non-text clause locally. `in:` is applied by the
/// caller's choice of mailboxes. Mailboxes are opened read-only.
///
/// Plain `UID SEARCH` is used even when the server advertises `ESEARCH`:
/// async-imap cannot parse `ESEARCH` responses (an unknown untagged
/// response fails the stream), and every `ESEARCH` server still answers
/// plain `SEARCH`. Non-ASCII text is sent as a UTF-8 literal, which needs
/// non-synchronizing literals (`LITERAL+` or `LITERAL-`); without them the
/// search fails rather than send text the server could misread.
pub async fn search_messages(
    target: &SyncTarget,
    secret: &str,
    query: &zmail_core::search::SearchQuery,
    mailboxes: &[String],
    max_hits_per_mailbox: usize,
) -> Result<Vec<zmail_core::ServerHitRef>> {
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }
    let endpoint = parse_endpoint(&target.endpoint)?;
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    let literals = session
        .capabilities()
        .await
        .map(|capabilities| capabilities.has_str("LITERAL+") || capabilities.has_str("LITERAL-"))
        .unwrap_or(false);
    let criteria = search_criteria(query, literals)
        .context("the search text needs UTF-8 literals, which this IMAP server does not offer")?;

    let mut hits = Vec::new();
    for mailbox in mailboxes {
        session
            .examine(mailbox)
            .await
            .with_context(|| format!("failed to open IMAP mailbox {mailbox} for search"))?;
        let mut uids = session
            .uid_search(&criteria)
            .await
            .with_context(|| format!("failed to search IMAP mailbox {mailbox}"))?
            .into_iter()
            .collect::<Vec<_>>();
        // Higher UIDs arrived later: keep the newest.
        uids.sort_unstable_by(|left, right| right.cmp(left));
        uids.truncate(max_hits_per_mailbox);
        hits.extend(uids.into_iter().map(|uid| zmail_core::ServerHitRef {
            mailbox_name: Some(mailbox.clone()),
            remote_id: uid.to_string(),
        }));
    }
    let _ = session.logout().await;
    Ok(hits)
}

/// Builds the `UID SEARCH` criteria for a query (see [`search_messages`]).
/// Returns `None` when the text needs literals the server does not offer.
fn search_criteria(query: &zmail_core::search::SearchQuery, literals: bool) -> Option<String> {
    use zmail_core::search::SearchTerm;

    let mut keys = Vec::new();
    let mut utf8 = false;
    for clause in &query.clauses {
        let key = match &clause.term {
            SearchTerm::Text(text)
            | SearchTerm::From(text)
            | SearchTerm::To(text)
            | SearchTerm::Subject(text) => {
                if !text.is_searchable() {
                    continue;
                }
                let name = match &clause.term {
                    SearchTerm::Text(_) => "TEXT",
                    SearchTerm::From(_) => "FROM",
                    SearchTerm::To(_) => "TO",
                    _ => "SUBJECT",
                };
                let (value, needs_utf8) = search_string(&text.value, literals)?;
                utf8 |= needs_utf8;
                format!("{name} {value}")
            }
            SearchTerm::Unread => "UNSEEN".to_owned(),
            SearchTerm::Read => "SEEN".to_owned(),
            SearchTerm::Flagged => "FLAGGED".to_owned(),
            SearchTerm::HasAttachment
            | SearchTerm::Mailbox(_)
            | SearchTerm::Before(_)
            | SearchTerm::After(_) => continue,
        };
        keys.push(if clause.negated {
            format!("NOT {key}")
        } else {
            key
        });
    }
    if keys.is_empty() {
        keys.push("ALL".to_owned());
    }
    let criteria = keys.join(" ");
    Some(if utf8 {
        format!("CHARSET UTF-8 {criteria}")
    } else {
        criteria
    })
}

/// Encodes search text as an IMAP string: a quoted string for ASCII, or a
/// non-synchronizing UTF-8 literal (`{n+}`) otherwise. Control characters,
/// which IMAP strings cannot carry, become spaces. Returns the encoded
/// string and whether it needs `CHARSET UTF-8`, or `None` when it needs a
/// literal and `literals` is false.
fn search_string(value: &str, literals: bool) -> Option<(String, bool)> {
    let cleaned = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    if cleaned.is_ascii() {
        let escaped = cleaned.replace('\\', "\\\\").replace('"', "\\\"");
        return Some((format!("\"{escaped}\""), false));
    }
    literals.then(|| (format!("{{{}+}}\r\n{cleaned}", cleaned.len()), true))
}

/// Formats UIDs as an IMAP sequence set (`1,5,9`).
fn uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
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
        AuthKind::ProtonSession => bail!("Proton accounts do not sign in over IMAP"),
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
    identity_email: &str,
    blobs: &BlobStore,
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
                blobs,
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
        threads,
        // The caller fills in the role and hierarchy from the listing.
        ..Default::default()
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
    last_seen: u32,
) -> Result<Vec<u32>> {
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

/// The highest UID the mirror already holds, above which new mail starts.
///
/// A mailbox that was empty at its last sync has no recorded UID; it then
/// resumes from 0 so its first messages are fetched by the very next sync,
/// which matters now that a single pushed change triggers a single sync.
/// Checkpoints that predate `last_seen_uid` fall back to the highest UID in
/// the flag map or the mirror, so nothing already stored is fetched twice.
fn resume_uid(previous: Option<&ImapMailboxCheckpoint>, mailbox: &MailboxSnapshot) -> u32 {
    previous
        .and_then(|state| {
            state
                .last_seen_uid
                .or_else(|| state.uid_flags.keys().last().copied())
        })
        .or_else(|| {
            mailbox
                .threads
                .iter()
                .flat_map(|thread| &thread.messages)
                .filter_map(|message| message.remote_id.parse::<u32>().ok())
                .max()
        })
        .unwrap_or(0)
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
    blobs: &BlobStore,
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
                blobs,
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
    }
    thread.participants = zmail_core::participants::thread_participants(&thread.messages);
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
    blobs: &BlobStore,
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
    let attachments = persist_attachments(&message, mailbox_name, &remote_stub, blobs);
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
/// encrypted attachment files and returns their metadata.
///
/// Blobs go straight to `blobs`, which encrypts them (ADR 0005); the
/// storage name it returns is opaque. Only the raw fetched message is held
/// in memory,
/// so attachment bytes never round-trip through the store or the database.
/// Parts beyond the per-message count or single-part size caps are skipped
/// rather than half-persisted.
fn persist_attachments(
    message: &mail_parser::Message<'_>,
    mailbox_name: &str,
    remote_id: &str,
    blobs: &BlobStore,
) -> Vec<MessageAttachment> {
    let mut attachments = Vec::new();
    if message.attachment_count() == 0 {
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
        // Without a writable blob store the sync must not fail: the
        // message still stores, the attachment part is simply not kept.
        let Ok(storage_path) = blobs.write(&storage_name, contents) else {
            continue;
        };
        attachments.push(MessageAttachment {
            filename,
            content_type,
            size_bytes: contents.len() as u64,
            storage_path,
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
            participants: zmail_core::participants::thread_participants(&self.messages),
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

/// Splits a parsed message into the stored body fields.
///
/// mail-parser fills `text_body` and `html_body` for every message: when
/// one alternative is missing it points at the other one and converts it
/// on the fly in `body_text` / `body_html`. Only parts that really are
/// `text/plain` or `text/html` are used here, so an HTML-only message goes
/// through the sanitizer (and remote-content detection) instead of being
/// shown as mail-parser's flattened text conversion (issue #112).
fn hydrate_bodies(
    message: &mail_parser::Message<'_>,
) -> (Option<String>, Option<String>, bool, String) {
    let body_text = match message.text_part(0).map(|part| &part.body) {
        Some(PartType::Text(text)) => Some(text.as_ref()),
        _ => None,
    }
    .filter(|value| !value.trim().is_empty());
    // Header-only parses get a synthesized "<html><body></body></html>"
    // shell from mail-parser; it carries no visible text and must surface as
    // an un-hydrated message rather than as an (empty) HTML body.
    let html = match message.html_part(0).map(|part| &part.body) {
        Some(PartType::Html(html)) => Some(html.as_ref()),
        _ => None,
    }
    .filter(|value| html_has_visible_text(value));

    if let Some(body_text) = body_text {
        let normalized = normalize_body_text(body_text);
        // multipart/alternative: the plain-text alternative is displayed,
        // but the HTML alternative still decides whether the message
        // references remote content. Like the JMAP adapter, only the
        // displayed alternative is stored.
        let has_remote_content =
            html.is_some_and(|html| sanitize_html_for_display(html).has_remote_content);
        return (
            Some(normalized.clone()),
            None,
            has_remote_content,
            normalized,
        );
    }

    if let Some(html) = html {
        let sanitized = sanitize_html_for_display(html);
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

/// What a sync records about a listed mailbox besides its name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MailboxListing {
    /// The mailbox's role (see [`mailbox_listings`]).
    pub(crate) role: Option<String>,
    /// The hierarchy delimiter the server listed it with.
    pub(crate) delimiter: Option<String>,
    /// The name of the mailbox one level up, whether or not it is listed.
    pub(crate) parent: Option<String>,
}

/// Roles, delimiters and parents for listed mailboxes, in listing order.
pub(crate) fn mailbox_listings(names: &[Name]) -> Vec<MailboxListing> {
    let roles = mailbox_roles(
        &names
            .iter()
            .map(|name| (name.name(), name.attributes()))
            .collect::<Vec<_>>(),
    );
    names
        .iter()
        .zip(roles)
        .map(|(name, role)| {
            let delimiter = name
                .delimiter()
                .filter(|delimiter| !delimiter.is_empty())
                .map(str::to_owned);
            MailboxListing {
                role,
                parent: delimiter
                    .as_deref()
                    .and_then(|delimiter| parent_name(name.name(), delimiter)),
                delimiter,
            }
        })
        .collect()
}

/// The name one level up from `name`, if it is nested.
fn parent_name(name: &str, delimiter: &str) -> Option<String> {
    name.rsplit_once(delimiter)
        .map(|(parent, _)| parent)
        .filter(|parent| !parent.is_empty())
        .map(str::to_owned)
}

/// Each mailbox's role, in order.
///
/// SPECIAL-USE attributes (RFC 6154) decide first. A role is only guessed
/// from a name when no mailbox claims it with an attribute, so a folder
/// the user happens to call "Archive" does not compete with the server's
/// `\Archive` mailbox. `\All` (Gmail's All Mail) is the archive only when
/// nothing is marked `\Archive`.
fn mailbox_roles(mailboxes: &[(&str, &[NameAttribute<'_>])]) -> Vec<Option<String>> {
    let has_archive = mailboxes.iter().any(|(_, attributes)| {
        attributes
            .iter()
            .any(|attribute| matches!(attribute, NameAttribute::Archive))
    });
    let special = mailboxes
        .iter()
        .map(|(_, attributes)| {
            special_use_role(attributes).filter(|role| {
                *role != "archive"
                    || !has_archive
                    || attributes
                        .iter()
                        .any(|attribute| matches!(attribute, NameAttribute::Archive))
            })
        })
        .collect::<Vec<_>>();
    mailboxes
        .iter()
        .zip(&special)
        .map(|((name, _), special_role)| match special_role {
            Some(role) => Some((*role).to_owned()),
            None => guess_role(name).filter(|guessed| !special.contains(&Some(guessed.as_str()))),
        })
        .collect()
}

/// The role a SPECIAL-USE (RFC 6154) attribute names.
fn special_use_role(attributes: &[NameAttribute<'_>]) -> Option<&'static str> {
    attributes.iter().find_map(|attribute| match attribute {
        NameAttribute::Sent => Some("sent"),
        NameAttribute::Drafts => Some("drafts"),
        NameAttribute::Archive | NameAttribute::All => Some("archive"),
        NameAttribute::Junk => Some("junk"),
        NameAttribute::Trash => Some("trash"),
        _ => None,
    })
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

    use async_imap::types::NameAttribute;

    use super::{
        ImapCheckpoint, ImapMailboxCheckpoint, ImapSecurity, conversation_key, excerpt, guess_role,
        hydrate_bodies, hydrated_body_from_rfc822, mailbox_roles, parent_name, parse_endpoint,
        persist_attachments, recompute_thread_aggregates, resume_uid, special_use_role,
        xoauth2_response,
    };
    use zmail_core::{MailboxSnapshot, MessageSnapshot, ThreadSnapshot};

    #[test]
    fn search_criteria_map_operators_onto_search_keys() {
        use zmail_core::search::SearchQuery;
        let criteria = |query: &str| super::search_criteria(&SearchQuery::parse(query), false);
        assert_eq!(
            criteria("budget \"q3 plan\" from:alice -to:bob subject:x is:unread -is:flagged"),
            Some(
                "TEXT \"budget\" TEXT \"q3 plan\" FROM \"alice\" NOT TO \"bob\" SUBJECT \"x\" \
                 UNSEEN NOT FLAGGED"
                    .to_owned()
            )
        );
        // Re-checked locally instead.
        assert_eq!(
            criteria("report has:attachment in:inbox after:7d before:2026-01-01"),
            Some("TEXT \"report\"".to_owned())
        );
        assert_eq!(criteria("is:read"), Some("SEEN".to_owned()));
        assert_eq!(criteria("!!"), Some("ALL".to_owned()));
    }

    #[test]
    fn search_strings_are_escaped_and_never_break_the_command() {
        use super::search_string;
        assert_eq!(
            search_string(r#"say "hi" \ bye"#, false),
            Some((r#""say \"hi\" \\ bye""#.to_owned(), false))
        );
        // CR/LF would end the command early: they become spaces.
        assert_eq!(
            search_string("a\r\nb NOOP", false),
            Some(("\"a  b NOOP\"".to_owned(), false))
        );
        // Non-ASCII text needs a UTF-8 literal.
        assert_eq!(search_string("Zoë", false), None);
        assert_eq!(
            search_string("Zoë", true),
            Some(("{4+}\r\nZoë".to_owned(), true))
        );
        assert_eq!(
            super::search_criteria(&zmail_core::search::SearchQuery::parse("from:Zoë"), true),
            Some("CHARSET UTF-8 FROM {4+}\r\nZoë".to_owned())
        );
    }

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
        let vault = zmail_security::vault::BlobVault::new(
            &dir,
            &zmail_security::vault::StoreKey::generate(),
        );
        let blobs = zmail_core::BlobStore::new(std::sync::Arc::new(vault.clone()));

        let attachments = persist_attachments(&message, "Inbox", "42", &blobs);
        assert_eq!(attachments.len(), 1, "one attachment should persist");
        let attachment = &attachments[0];
        assert_eq!(attachment.filename, "notes.txt");
        assert_eq!(attachment.content_type, "text/plain");
        assert_eq!(attachment.size_bytes, "attachment payload".len() as u64);
        // The name on disk is opaque: neither the mailbox nor the filename.
        assert_eq!(
            attachment.storage_path,
            vault.storage_name("incoming-Inbox-42-0-notes.txt")
        );

        let blob_path = dir.join(&attachment.storage_path);
        let stored = std::fs::read(&blob_path).expect("the blob should be readable");
        assert!(
            !stored
                .windows(b"attachment payload".len())
                .any(|window| window == b"attachment payload"),
            "the blob is encrypted on disk"
        );
        assert_eq!(
            vault.read_file(&blob_path).expect("the blob decrypts"),
            b"attachment payload"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The path Proton hydration takes (`zmail-proton` rebuilds RFC 822 and
    /// hands it here): the attachment lands encrypted under an opaque name,
    /// and neither its bytes nor its name nor the mailbox appear on disk.
    #[test]
    fn rfc822_hydration_writes_attachments_encrypted() {
        let raw = concat!(
            "From: sender@example.test\r\n",
            "Subject: hydrated\r\n",
            "To: rcpt@example.test\r\n",
            "Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n",
            "\r\n",
            "--BOUND\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "Body text.\r\n",
            "--BOUND\r\n",
            "Content-Type: text/csv; name=\"ledger-zebra.csv\"\r\n",
            "Content-Disposition: attachment; filename=\"ledger-zebra.csv\"\r\n",
            "\r\n",
            "quarter,total,pangolin\r\n",
            "--BOUND--\r\n",
        );
        let dir = std::env::temp_dir().join(format!(
            "zmail-imap-rfc822-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let vault = zmail_security::vault::BlobVault::new(
            &dir,
            &zmail_security::vault::StoreKey::generate(),
        );
        let blobs = zmail_core::BlobStore::new(std::sync::Arc::new(vault.clone()));

        let body =
            hydrated_body_from_rfc822(raw.as_bytes(), "Labels/Secret-folder", "msg-1", &blobs)
                .expect("the message parses");
        assert_eq!(body.attachments.len(), 1);
        let attachment = &body.attachments[0];
        assert_eq!(attachment.filename, "ledger-zebra.csv");

        for entry in std::fs::read_dir(&dir).expect("the blob dir exists") {
            let path = entry.expect("entry").path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            assert!(
                !name.contains("ledger") && !name.contains("Secret"),
                "{name} reveals the attachment or mailbox"
            );
            let stored = std::fs::read(&path).expect("readable");
            assert!(
                !stored
                    .windows(b"pangolin".len())
                    .any(|window| window == b"pangolin"),
                "{name} holds the attachment in plaintext"
            );
        }
        let decrypted = vault
            .read_file(&dir.join(&attachment.storage_path))
            .expect("the blob decrypts");
        assert!(String::from_utf8_lossy(&decrypted).contains("quarter,total,pangolin"));
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

    fn hydrate_raw(raw: &str) -> (Option<String>, Option<String>, bool, String) {
        let message = mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .expect("the message should parse");
        hydrate_bodies(&message)
    }

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

    #[test]
    fn html_only_messages_are_sanitized_and_flag_remote_content() {
        let raw = format!(
            concat!(
                "From: sender@example.test\r\n",
                "To: rcpt@example.test\r\n",
                "Subject: html only\r\n",
                "Content-Type: text/html; charset=utf-8\r\n",
                "\r\n",
                "{}\r\n",
            ),
            NEWSLETTER_HTML
        );
        let (body_text, sanitized_html, has_remote_content, preview) = hydrate_raw(&raw);

        assert!(
            body_text.is_none(),
            "an HTML-only message must not store mail-parser's text conversion"
        );
        let sanitized_html = sanitized_html.expect("the HTML body should be stored sanitized");
        assert!(sanitized_html.contains("<img src=\"zmail-blocked:0\">"));
        assert!(!sanitized_html.contains("tracker.example.test"));
        assert!(has_remote_content);
        assert_eq!(
            preview,
            "Your new dashboard\n\nFaster exports\n\nPlan  Price\nStarter  EUR 9\nTeam  EUR 29"
        );
    }

    #[test]
    fn html_only_messages_without_remote_references_are_not_flagged() {
        let raw = concat!(
            "From: sender@example.test\r\n",
            "To: rcpt@example.test\r\n",
            "Subject: html only, local\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<p>Hello</p><p>No remote images here.</p>\r\n",
        );
        let (body_text, sanitized_html, has_remote_content, preview) = hydrate_raw(raw);

        assert!(body_text.is_none());
        assert!(sanitized_html.is_some());
        assert!(!has_remote_content);
        assert_eq!(preview, "Hello\n\nNo remote images here.");
    }

    #[test]
    fn multipart_alternative_keeps_plain_text_and_flags_remote_html() {
        let raw = format!(
            concat!(
                "From: sender@example.test\r\n",
                "To: rcpt@example.test\r\n",
                "Subject: alternative\r\n",
                "Content-Type: multipart/alternative; boundary=\"ALT\"\r\n",
                "\r\n",
                "--ALT\r\n",
                "Content-Type: text/plain; charset=utf-8\r\n",
                "\r\n",
                "Your new dashboard   \r\n",
                "Faster exports\r\n",
                "--ALT\r\n",
                "Content-Type: text/html; charset=utf-8\r\n",
                "\r\n",
                "{}\r\n",
                "--ALT--\r\n",
            ),
            NEWSLETTER_HTML
        );
        let (body_text, sanitized_html, has_remote_content, preview) = hydrate_raw(&raw);

        assert_eq!(
            body_text.as_deref(),
            Some("Your new dashboard\nFaster exports")
        );
        assert!(
            sanitized_html.is_none(),
            "only the displayed alternative is stored, matching the JMAP adapter"
        );
        assert!(
            has_remote_content,
            "the HTML alternative's remote image must still be reported"
        );
        assert_eq!(preview, "Your new dashboard\nFaster exports");
    }

    #[test]
    fn plain_text_only_messages_keep_their_text_body() {
        let raw = concat!(
            "From: sender@example.test\r\n",
            "To: rcpt@example.test\r\n",
            "Subject: plain\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Line one\r\n",
            "Visit https://example.test/ for <b>details</b>.\r\n",
        );
        let (body_text, sanitized_html, has_remote_content, preview) = hydrate_raw(raw);

        assert_eq!(
            body_text.as_deref(),
            Some("Line one\nVisit https://example.test/ for <b>details</b>.")
        );
        assert!(sanitized_html.is_none());
        assert!(!has_remote_content);
        assert_eq!(
            preview,
            "Line one\nVisit https://example.test/ for <b>details</b>."
        );
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
        assert_eq!(
            thread.participants, "Older Sender, Newer Sender",
            "participants list every sender, oldest first"
        );
    }

    #[test]
    fn new_mail_resumes_after_the_highest_known_uid() {
        let empty = MailboxSnapshot {
            name: "Projects".to_owned(),
            role: None,
            threads: Vec::new(),
            ..Default::default()
        };
        let checkpoint = |last_seen_uid, uids: &[u32]| ImapMailboxCheckpoint {
            last_seen_uid,
            uid_flags: uids.iter().map(|uid| (*uid, Vec::new())).collect(),
            ..ImapMailboxCheckpoint::default()
        };

        assert_eq!(
            resume_uid(Some(&checkpoint(None, &[])), &empty),
            0,
            "a mailbox that was empty fetches its first message next time"
        );
        assert_eq!(resume_uid(Some(&checkpoint(Some(7), &[3, 9])), &empty), 7);
        assert_eq!(
            resume_uid(Some(&checkpoint(None, &[3, 9])), &empty),
            9,
            "legacy checkpoints resume after the flag map"
        );
        assert_eq!(resume_uid(None, &empty), 0);
    }

    #[test]
    fn mailbox_roles_follow_common_names() {
        assert_eq!(guess_role("Inbox").as_deref(), Some("inbox"));
        assert_eq!(guess_role("Sent Items").as_deref(), Some("sent"));
        assert_eq!(guess_role("Projects"), None);
    }

    #[test]
    fn special_use_attributes_name_roles() {
        assert_eq!(
            special_use_role(&[NameAttribute::Marked, NameAttribute::Sent]),
            Some("sent")
        );
        assert_eq!(special_use_role(&[NameAttribute::NoInferiors]), None);
    }

    #[test]
    fn special_use_wins_over_names() {
        let roles = |mailboxes: &[(&str, &[NameAttribute<'static>])]| {
            mailbox_roles(mailboxes)
                .into_iter()
                .map(|role| role.unwrap_or_default())
                .collect::<Vec<_>>()
        };
        // Gmail: the attributes name the roles; INBOX is guessed.
        assert_eq!(
            roles(&[
                ("INBOX", &[]),
                ("[Gmail]/All Mail", &[NameAttribute::All]),
                ("[Gmail]/Sent Mail", &[NameAttribute::Sent]),
                ("[Gmail]/Bin", &[NameAttribute::Trash]),
                ("[Gmail]/Spam", &[NameAttribute::Junk]),
                ("Trash", &[]),
            ]),
            ["inbox", "archive", "sent", "trash", "junk", ""],
            "a folder named like a claimed role gets no role"
        );
        // Without attributes the names still decide.
        assert_eq!(
            roles(&[("Inbox", &[]), ("Sent Items", &[]), ("Projects", &[])]),
            ["inbox", "sent", ""]
        );
        // \Archive beats \All.
        assert_eq!(
            roles(&[
                ("All", &[NameAttribute::All]),
                ("Archive", &[NameAttribute::Archive]),
            ]),
            ["", "archive"]
        );
    }

    #[test]
    fn parents_are_one_level_up() {
        assert_eq!(
            parent_name("Projects/Website", "/").as_deref(),
            Some("Projects")
        );
        assert_eq!(
            parent_name("INBOX.Lists.rust", ".").as_deref(),
            Some("INBOX.Lists")
        );
        assert_eq!(parent_name("Projects", "/"), None);
        assert_eq!(parent_name("/rooted", "/"), None);
    }

    #[test]
    fn preview_excerpt_is_trimmed() {
        let preview = excerpt("one two three four five six seven", 10);
        assert!(preview.starts_with("one two th"));
    }
}
