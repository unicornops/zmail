//! SQLite-backed local store for persisted accounts, sync state, and mailbox metadata.

use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use zmail_core::{
    AccountColor, AccountOverview, AccountSummary, AccountSyncSnapshot, AuthKind,
    CachePolicySummary, DeliveryTarget, DraftAttachment, DraftDetail, DraftState, DraftSummary,
    DraftUpdate, HydratedBody, IdentitySummary, IndexCandidate, IndexProgress, MailboxRole,
    MailboxSelection, MailboxSnapshot, MailboxSummary, MessageAttachment, MessageFlagState,
    MessagePreview, MessageRenderMode, MessageSnapshot, NewAccount, NewDraft, OAuth2TargetConfig,
    OutboxJobState, OutboxJobSummary, OutgoingAttachment, OutgoingConfig, OutgoingMessage,
    PendingOp, PendingOpKind, PendingOutboxJob, ProtocolKind, ReceivedAttachment, SearchHit,
    SearchScope, ServerHitRef, StoreStats, SubmissionKind, SubmissionSecurity, SyncPhase,
    SyncStatusSummary, SyncTarget, ThreadCounts, ThreadFilter, ThreadMarks, ThreadMessage,
    ThreadSnapshot, ThreadState, ThreadSummary, ThreadView, VirtualMailbox,
    preferences::UiPreferences,
};
use zmail_security::sanitize_html_for_display;

mod address_history;
mod search;

/// What removing an account deletes besides its mail: unsent messages and
/// drafts (see [`LocalStore::account_removal_impact`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountRemovalImpact {
    /// Outbox messages that have not gone yet: waiting, retrying or
    /// failed.
    pub unsent_messages: usize,
    /// Outbox messages being sent right now.
    pub sending_messages: usize,
    /// Drafts that are not in the outbox.
    pub drafts: usize,
}

/// The storage stance recorded during Phase 2.
pub const STORAGE_MODEL: &str =
    "SQLite metadata plus content-addressed blobs, optimized for large offline-first mailboxes.";

const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_phase2_store", PHASE2_SCHEMA),
    ("0002_phase3_reader_and_mailbox_ops", PHASE3_SCHEMA),
    ("0003_phase4_compose_and_send", PHASE4_SCHEMA),
    ("0004_phase5_thread_attachment_metadata", PHASE5_SCHEMA),
    ("0005_stable_remote_ids", STABLE_REMOTE_ID_SCHEMA),
    ("0006_pending_ops", PENDING_OPS_SCHEMA),
    ("0007_pending_op_states", PENDING_OP_STATES_SCHEMA),
    ("0008_account_auth_kind", ACCOUNT_AUTH_KIND_SCHEMA),
    ("0009_message_attachments", MESSAGE_ATTACHMENTS_SCHEMA),
    ("0010_account_oauth2_client", ACCOUNT_OAUTH2_CLIENT_SCHEMA),
    ("0011_account_disk_budget", ACCOUNT_DISK_BUDGET_SCHEMA),
    ("0012_message_reply_to", MESSAGE_REPLY_TO_SCHEMA),
    ("0013_app_settings", APP_SETTINGS_SCHEMA),
    ("0014_search_index_v2", SEARCH_INDEX_V2_SCHEMA),
    ("0015_background_indexing", BACKGROUND_INDEXING_SCHEMA),
    ("0016_server_search_hits", SERVER_SEARCH_HITS_SCHEMA),
    ("0017_mailbox_hierarchy", MAILBOX_HIERARCHY_SCHEMA),
    (
        "0018_sender_warning_dismissed",
        SENDER_WARNING_DISMISSED_SCHEMA,
    ),
    (
        "0019_address_history",
        address_history::ADDRESS_HISTORY_SCHEMA,
    ),
    ("0020_account_color", ACCOUNT_COLOR_SCHEMA),
];

/// Failed background body fetches after which a message is skipped by the
/// indexer. It still hydrates when opened.
pub const MAX_BODY_FETCH_FAILURES: i64 = 3;

const PHASE2_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version TEXT PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY,
    display_name TEXT NOT NULL,
    protocol_kind TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    username TEXT NOT NULL,
    cache_policy_name TEXT NOT NULL,
    cache_policy_description TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS identities (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    display_name TEXT NOT NULL,
    email_address TEXT NOT NULL,
    signature TEXT NOT NULL,
    is_primary INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS sync_state (
    account_id INTEGER PRIMARY KEY,
    phase TEXT NOT NULL,
    detail TEXT NOT NULL,
    checkpoint TEXT,
    last_attempt_at TEXT,
    last_success_at TEXT,
    last_error_at TEXT,
    last_error_message TEXT,
    failure_count INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS mailboxes (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    role TEXT,
    sort_order INTEGER NOT NULL,
    total_threads INTEGER NOT NULL,
    unread_threads INTEGER NOT NULL,
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS threads (
    id INTEGER PRIMARY KEY,
    mailbox_id INTEGER NOT NULL,
    subject TEXT NOT NULL,
    participants TEXT NOT NULL,
    preview TEXT NOT NULL,
    message_count INTEGER NOT NULL,
    unread_count INTEGER NOT NULL,
    latest_message_at TEXT NOT NULL,
    latest_message_id INTEGER NOT NULL,
    FOREIGN KEY(mailbox_id) REFERENCES mailboxes(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    thread_id INTEGER NOT NULL,
    identity_id INTEGER NOT NULL,
    sender TEXT NOT NULL,
    recipients TEXT NOT NULL,
    subject TEXT NOT NULL,
    body_preview TEXT NOT NULL,
    received_at TEXT NOT NULL,
    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE,
    FOREIGN KEY(identity_id) REFERENCES identities(id)
);

CREATE INDEX IF NOT EXISTS idx_accounts_protocol ON accounts(protocol_kind, username);
CREATE UNIQUE INDEX IF NOT EXISTS idx_identities_email_per_account ON identities(account_id, email_address);
CREATE INDEX IF NOT EXISTS idx_mailboxes_account_sort ON mailboxes(account_id, sort_order, name);
CREATE INDEX IF NOT EXISTS idx_threads_mailbox_order ON threads(mailbox_id, latest_message_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id, received_at DESC, id DESC);
"#;

const PHASE3_SCHEMA: &str = r#"
ALTER TABLE threads ADD COLUMN is_flagged INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN body_text TEXT;
ALTER TABLE messages ADD COLUMN sanitized_html TEXT;
ALTER TABLE messages ADD COLUMN has_remote_content INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN is_read INTEGER NOT NULL DEFAULT 1;
ALTER TABLE messages ADD COLUMN is_flagged INTEGER NOT NULL DEFAULT 0;

UPDATE messages
SET body_text = COALESCE(body_text, body_preview)
WHERE body_text IS NULL;

CREATE VIRTUAL TABLE IF NOT EXISTS search_documents USING fts5(
    account_id UNINDEXED,
    mailbox_id UNINDEXED,
    thread_id UNINDEXED,
    mailbox_name UNINDEXED,
    subject,
    participants,
    preview,
    body
);

INSERT INTO search_documents(account_id, mailbox_id, thread_id, mailbox_name, subject, participants, preview, body)
SELECT
    mailboxes.account_id,
    mailboxes.id,
    threads.id,
    mailboxes.name,
    threads.subject,
    threads.participants,
    threads.preview,
    COALESCE(GROUP_CONCAT(COALESCE(messages.body_text, messages.sanitized_html, messages.body_preview), ' '), '')
FROM threads
JOIN mailboxes ON mailboxes.id = threads.mailbox_id
LEFT JOIN messages ON messages.thread_id = threads.id
GROUP BY threads.id;
"#;

const PHASE4_SCHEMA: &str = r#"
ALTER TABLE accounts ADD COLUMN submission_kind TEXT;
ALTER TABLE accounts ADD COLUMN submission_host TEXT;
ALTER TABLE accounts ADD COLUMN submission_port INTEGER;
ALTER TABLE accounts ADD COLUMN submission_security TEXT;
ALTER TABLE accounts ADD COLUMN submission_username TEXT;
ALTER TABLE accounts ADD COLUMN submission_hello_name TEXT;

CREATE TABLE IF NOT EXISTS drafts (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    identity_id INTEGER NOT NULL,
    identity_name TEXT NOT NULL,
    identity_email TEXT NOT NULL,
    identity_signature TEXT NOT NULL,
    state TEXT NOT NULL,
    to_json TEXT NOT NULL,
    cc_json TEXT NOT NULL,
    bcc_json TEXT NOT NULL,
    subject TEXT NOT NULL,
    body TEXT NOT NULL,
    queued_at TEXT,
    sent_at TEXT,
    last_error_message TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS draft_attachments (
    id INTEGER PRIMARY KEY,
    draft_id INTEGER NOT NULL,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    original_path TEXT NOT NULL,
    storage_path TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(draft_id) REFERENCES drafts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS outbox_jobs (
    id INTEGER PRIMARY KEY,
    draft_id INTEGER NOT NULL,
    account_id INTEGER NOT NULL,
    state TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_attempt_at TEXT,
    next_attempt_at TEXT,
    last_error_message TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(draft_id) REFERENCES drafts(id) ON DELETE CASCADE,
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_drafts_account_updated ON drafts(account_id, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_outbox_jobs_due ON outbox_jobs(state, next_attempt_at, id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_jobs_active_draft
ON outbox_jobs(draft_id)
WHERE state IN ('queued', 'sending', 'retrying');
"#;

const PHASE5_SCHEMA: &str = r#"
ALTER TABLE threads ADD COLUMN has_attachments INTEGER NOT NULL DEFAULT 0;
"#;

const STABLE_REMOTE_ID_SCHEMA: &str = r#"
ALTER TABLE threads ADD COLUMN remote_thread_id TEXT;
ALTER TABLE messages ADD COLUMN remote_id TEXT;
CREATE INDEX IF NOT EXISTS idx_threads_mailbox_remote ON threads(mailbox_id, remote_thread_id);
CREATE INDEX IF NOT EXISTS idx_messages_remote ON messages(remote_id);
"#;

const PENDING_OPS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS pending_ops (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL,
    op TEXT NOT NULL,
    source_mailbox TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    target_mailbox TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_pending_ops_account ON pending_ops(account_id, id);
"#;

const ACCOUNT_AUTH_KIND_SCHEMA: &str = r#"
ALTER TABLE accounts ADD COLUMN auth_kind TEXT NOT NULL DEFAULT 'password';
"#;

const MESSAGE_ATTACHMENTS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS message_attachments (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    storage_path TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(message_id) REFERENCES messages(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_message_attachments_message
ON message_attachments(message_id, id);
"#;

const ACCOUNT_OAUTH2_CLIENT_SCHEMA: &str = r#"
ALTER TABLE accounts ADD COLUMN oauth2_token_endpoint TEXT;
ALTER TABLE accounts ADD COLUMN oauth2_client_id TEXT;
ALTER TABLE accounts ADD COLUMN oauth2_access_expires_at INTEGER;
"#;

const ACCOUNT_DISK_BUDGET_SCHEMA: &str = r#"
ALTER TABLE accounts ADD COLUMN cache_policy_disk_budget_mb INTEGER;
"#;

/// The account colour the user picked (an [`AccountColor`] name); NULL
/// keeps the colour derived from the account id.
const ACCOUNT_COLOR_SCHEMA: &str = r#"
ALTER TABLE accounts ADD COLUMN color TEXT;
"#;

const MESSAGE_REPLY_TO_SCHEMA: &str = r#"
ALTER TABLE messages ADD COLUMN reply_to TEXT;
"#;

/// App-wide settings as one JSON document per key (for example the UI
/// preferences under [`UI_PREFERENCES_KEY`]).
const APP_SETTINGS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
"#;

/// `app_settings` key holding the serialised [`UiPreferences`].
const UI_PREFERENCES_KEY: &str = "ui_preferences";

/// `app_settings` key holding the imported Zed theme family, as the JSON
/// the user picked.
const CUSTOM_THEME_KEY: &str = "custom_theme";

/// Rebuilds the full-text index as one document per thread keyed by the
/// thread id (the FTS rowid), with the senders and recipients of its
/// messages as their own columns for `from:` and `to:`.
///
/// Mailbox names are no longer part of the document, so moving a thread
/// leaves its document untouched. Triggers record every thread whose
/// indexed text changes in `search_dirty_threads`, and writers call
/// [`flush_search_index`] before they commit, so the index stays exact
/// without re-indexing whole accounts. All existing threads start dirty and
/// are indexed by the flush that runs after the migrations.
const SEARCH_INDEX_V2_SCHEMA: &str = r#"
DROP TABLE IF EXISTS search_documents;
CREATE VIRTUAL TABLE search_documents USING fts5(
    subject,
    participants,
    preview,
    body,
    senders,
    recipients
);

CREATE INDEX IF NOT EXISTS idx_threads_latest ON threads(latest_message_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS search_dirty_threads (
    thread_id INTEGER PRIMARY KEY
);
INSERT OR IGNORE INTO search_dirty_threads(thread_id) SELECT id FROM threads;

CREATE TRIGGER IF NOT EXISTS search_dirty_thread_insert AFTER INSERT ON threads
BEGIN
    INSERT OR IGNORE INTO search_dirty_threads(thread_id) VALUES (new.id);
END;
CREATE TRIGGER IF NOT EXISTS search_dirty_thread_update
AFTER UPDATE OF subject, participants, preview ON threads
BEGIN
    INSERT OR IGNORE INTO search_dirty_threads(thread_id) VALUES (new.id);
END;
CREATE TRIGGER IF NOT EXISTS search_dirty_thread_delete AFTER DELETE ON threads
BEGIN
    INSERT OR IGNORE INTO search_dirty_threads(thread_id) VALUES (old.id);
END;
CREATE TRIGGER IF NOT EXISTS search_dirty_message_insert AFTER INSERT ON messages
BEGIN
    INSERT OR IGNORE INTO search_dirty_threads(thread_id) VALUES (new.thread_id);
END;
CREATE TRIGGER IF NOT EXISTS search_dirty_message_update
AFTER UPDATE OF thread_id, sender, recipients, body_preview, body_text, sanitized_html ON messages
BEGIN
    INSERT OR IGNORE INTO search_dirty_threads(thread_id) VALUES (old.thread_id);
    INSERT OR IGNORE INTO search_dirty_threads(thread_id) VALUES (new.thread_id);
END;
CREATE TRIGGER IF NOT EXISTS search_dirty_message_delete AFTER DELETE ON messages
BEGIN
    INSERT OR IGNORE INTO search_dirty_threads(thread_id) VALUES (old.thread_id);
END;
"#;

/// Background body indexing: a failure counter so a message that cannot be
/// fetched does not block the newest-first queue, and a partial index over
/// the messages still waiting for a body, in queue order.
const BACKGROUND_INDEXING_SCHEMA: &str = r#"
ALTER TABLE messages ADD COLUMN body_fetch_failures INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_messages_unhydrated
ON messages(received_at DESC, id DESC)
WHERE body_text IS NULL AND sanitized_html IS NULL;
"#;

/// Threads the server-side search fallback found for an account's latest
/// server query, so search results still come from local state. One query
/// is kept per account; hits disappear with their threads.
const SERVER_SEARCH_HITS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS server_search_hits (
    account_id INTEGER NOT NULL,
    query TEXT NOT NULL,
    thread_id INTEGER NOT NULL,
    PRIMARY KEY (account_id, query, thread_id),
    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE,
    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
);
"#;

/// Re-indexes every dirty thread: drops its document, rebuilds it from the
/// stored rows (deleted threads simply get no new document) and clears the
/// dirty set. Run inside the writer's transaction. Messages are joined
/// oldest first, so a document never depends on the query plan, and the
/// `CROSS JOIN` keeps the dirty set as the outer loop: grouping by
/// `threads.id` let the planner scan every thread to avoid a sort.
const SEARCH_INDEX_FLUSH: &str = r#"
DELETE FROM search_documents WHERE rowid IN (SELECT thread_id FROM search_dirty_threads);
INSERT INTO search_documents(rowid, subject, participants, preview, body, senders, recipients)
SELECT
    threads.id,
    threads.subject,
    threads.participants,
    threads.preview,
    COALESCE(GROUP_CONCAT(COALESCE(messages.body_text, messages.sanitized_html, messages.body_preview), ' '
        ORDER BY messages.received_at, messages.id), ''),
    COALESCE(GROUP_CONCAT(messages.sender, ' ' ORDER BY messages.received_at, messages.id), ''),
    COALESCE(GROUP_CONCAT(messages.recipients, ' ' ORDER BY messages.received_at, messages.id), '')
FROM search_dirty_threads
CROSS JOIN threads ON threads.id = search_dirty_threads.thread_id
LEFT JOIN messages ON messages.thread_id = threads.id
GROUP BY search_dirty_threads.thread_id;
DELETE FROM search_dirty_threads;
"#;

const PENDING_OP_STATES_SCHEMA: &str = r#"
ALTER TABLE pending_ops ADD COLUMN state TEXT NOT NULL DEFAULT 'pending';
CREATE INDEX IF NOT EXISTS idx_pending_ops_state ON pending_ops(account_id, state);
"#;

/// The mailbox hierarchy for the sidebar's folder tree: the IMAP
/// hierarchy delimiter and the parent mailbox's server name (JMAP's
/// `parentId`, resolved to a name). Rows fill in on the next sync.
const MAILBOX_HIERARCHY_SCHEMA: &str = r#"
ALTER TABLE mailboxes ADD COLUMN delimiter TEXT;
ALTER TABLE mailboxes ADD COLUMN parent_name TEXT;
"#;

/// "Looks safe" on a message's sender-safety banner hides it for that
/// message only; it is not a per-sender allowlist.
const SENDER_WARNING_DISMISSED_SCHEMA: &str = r#"
ALTER TABLE messages ADD COLUMN sender_warning_dismissed INTEGER NOT NULL DEFAULT 0;
"#;

/// Orders mailboxes with the special-use roles first, in
/// [`MailboxRole`] order, then the rest in server order.
const MAILBOX_ROLE_ORDER: &str = "CASE lower(role) \
    WHEN 'inbox' THEN 0 WHEN 'drafts' THEN 1 WHEN 'sent' THEN 2 \
    WHEN 'archive' THEN 3 WHEN 'junk' THEN 4 WHEN 'trash' THEN 5 ELSE 6 END";

static EPHEMERAL_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Thread-safe local store handle backed by a persistent SQLite database.
#[derive(Debug, Clone)]
pub struct LocalStore {
    path: Arc<PathBuf>,
    /// Private directory of an ephemeral store, removed when the last handle
    /// drops. `None` for persistent stores.
    ephemeral_dir: Option<Arc<EphemeralDir>>,
}

/// Owns an ephemeral store's directory (database, WAL files and attachment
/// blobs) and deletes it on drop.
#[derive(Debug)]
struct EphemeralDir(PathBuf);

impl Drop for EphemeralDir {
    fn drop(&mut self) {
        // Best effort: on Windows a file still held open elsewhere can make
        // this fail, and a leftover temp directory is harmless.
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl LocalStore {
    /// Opens a persistent store at the given path and applies migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let store = Self {
            path: Arc::new(path),
            ephemeral_dir: None,
        };
        store.apply_migrations()?;
        store.rebuild_address_history_if_pending()?;
        Ok(store)
    }

    /// Returns the on-disk path of the store database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opens an ephemeral store in its own directory under the OS temp
    /// directory, for tests and fixtures.
    ///
    /// Each ephemeral store gets a private directory so its attachment blobs
    /// (stored next to the database) never mix with another store's. The
    /// directory is deleted when the last clone of the handle drops.
    pub fn open_ephemeral() -> Result<Self> {
        let sequence = EPHEMERAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("zmail-{nanos}-{}-{sequence}", std::process::id()));
        // Created before opening so a failed open still cleans up.
        let guard = Arc::new(EphemeralDir(directory.clone()));
        let mut store = Self::open(directory.join("store.sqlite3"))?;
        store.ephemeral_dir = Some(guard);
        Ok(store)
    }

    /// Returns `true` when the store has no configured accounts yet.
    pub fn needs_onboarding(&self) -> Result<bool> {
        Ok(self.stats()?.account_count == 0)
    }

    /// Inserts a fully specified account draft and returns its new identifier.
    pub fn insert_account(&self, account: &NewAccount) -> Result<i64> {
        if account.identities.is_empty() {
            bail!("accounts require at least one identity");
        }

        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the account creation transaction")?;

        transaction.execute(
            r#"
            INSERT INTO accounts(
                display_name,
                protocol_kind,
                auth_kind,
                endpoint,
                username,
                cache_policy_name,
                cache_policy_description,
                submission_kind,
                submission_host,
                submission_port,
                submission_security,
                submission_username,
                submission_hello_name,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
            params![
                account.display_name,
                account.protocol.as_str(),
                account.auth.as_str(),
                account.endpoint,
                account.username,
                account.cache_policy.name,
                account.cache_policy.description,
                account
                    .outgoing
                    .as_ref()
                    .map(|outgoing| outgoing.kind.as_str()),
                account
                    .outgoing
                    .as_ref()
                    .and_then(|outgoing| outgoing.host.as_deref()),
                account
                    .outgoing
                    .as_ref()
                    .and_then(|outgoing| outgoing.port.map(i64::from)),
                account
                    .outgoing
                    .as_ref()
                    .and_then(|outgoing| outgoing.security.map(SubmissionSecurity::as_str)),
                account
                    .outgoing
                    .as_ref()
                    .and_then(|outgoing| outgoing.username.as_deref()),
                account
                    .outgoing
                    .as_ref()
                    .map(|outgoing| outgoing.hello_name.as_str()),
                utc_now_timestamp(),
            ],
        )?;
        let account_id = transaction.last_insert_rowid();

        {
            let mut insert_identity = transaction.prepare(
                r#"
                INSERT INTO identities(account_id, display_name, email_address, signature, is_primary)
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
            )?;
            for identity in &account.identities {
                insert_identity.execute(params![
                    account_id,
                    identity.display_name,
                    identity.email_address,
                    identity.signature,
                    identity.is_primary,
                ])?;
            }
        }

        transaction.execute(
            r#"
            INSERT INTO sync_state(account_id, phase, detail, checkpoint, failure_count)
            VALUES (?1, ?2, ?3, NULL, 0)
            "#,
            params![
                account_id,
                SyncPhase::Idle.as_str(),
                format!("{} account created locally", account.protocol.label()),
            ],
        )?;

        transaction
            .commit()
            .context("failed to commit the new account transaction")?;

        Ok(account_id)
    }

    /// Creates a deterministic demo account template for Phase 2 onboarding.
    pub fn create_demo_account(&self, protocol: ProtocolKind) -> Result<i64> {
        let ordinal = self.demo_account_ordinal(protocol)?;
        let display_name = match protocol {
            ProtocolKind::Imap => format!("Operations IMAP {ordinal}"),
            ProtocolKind::Jmap => format!("Workspace JMAP {ordinal}"),
        };
        let username = match protocol {
            ProtocolKind::Imap => format!("ops{ordinal}@zmail.dev"),
            ProtocolKind::Jmap => format!("workspace{ordinal}@zmail.dev"),
        };
        let endpoint = match protocol {
            ProtocolKind::Imap => format!("imap{ordinal}.mail.zmail.dev:993"),
            ProtocolKind::Jmap => format!("https://jmap{ordinal}.mail.zmail.dev/jmap"),
        };
        let primary_name = match protocol {
            ProtocolKind::Imap => "Operations Desk",
            ProtocolKind::Jmap => "Workspace Team",
        };
        let secondary_name = match protocol {
            ProtocolKind::Imap => "Incident Response",
            ProtocolKind::Jmap => "Automations",
        };

        let account = NewAccount {
            display_name,
            protocol,
            auth: AuthKind::Password,
            endpoint,
            username: username.clone(),
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(match protocol {
                ProtocolKind::Imap => {
                    OutgoingConfig::smtp_default(format!("smtp{ordinal}.mail.zmail.dev"))
                }
                ProtocolKind::Jmap => OutgoingConfig::jmap_default(),
            }),
            identities: vec![
                zmail_core::NewIdentity {
                    display_name: primary_name.to_owned(),
                    email_address: username.clone(),
                    signature: format!("Regards,\n{primary_name}"),
                    is_primary: true,
                },
                zmail_core::NewIdentity {
                    display_name: secondary_name.to_owned(),
                    email_address: match protocol {
                        ProtocolKind::Imap => format!("alerts{ordinal}@zmail.dev"),
                        ProtocolKind::Jmap => format!("flows{ordinal}@zmail.dev"),
                    },
                    signature: format!("Thanks,\n{secondary_name}"),
                    is_primary: false,
                },
            ],
        };

        self.insert_account(&account)
    }

    /// Returns the list of configured accounts and their sync status.
    pub fn list_accounts(&self) -> Result<Vec<AccountSummary>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            r#"
            SELECT
                accounts.id,
                accounts.display_name,
                accounts.protocol_kind,
                accounts.endpoint,
                accounts.username,
                accounts.cache_policy_name,
                accounts.cache_policy_description,
                COUNT(identities.id) AS identity_count,
                sync_state.phase,
                sync_state.detail,
                sync_state.checkpoint,
                sync_state.last_attempt_at,
                sync_state.last_success_at,
                sync_state.last_error_at,
                sync_state.last_error_message,
                sync_state.failure_count,
                accounts.color
            FROM accounts
            LEFT JOIN identities ON identities.account_id = accounts.id
            LEFT JOIN sync_state ON sync_state.account_id = accounts.id
            GROUP BY
                accounts.id,
                accounts.display_name,
                accounts.protocol_kind,
                accounts.endpoint,
                accounts.username,
                accounts.cache_policy_name,
                accounts.cache_policy_description,
                sync_state.phase,
                sync_state.detail,
                sync_state.checkpoint,
                sync_state.last_attempt_at,
                sync_state.last_success_at,
                sync_state.last_error_at,
                sync_state.last_error_message,
                sync_state.failure_count,
                accounts.color
            ORDER BY accounts.id ASC
            "#,
        )?;

        let rows = statement.query_map([], |row| {
            let protocol = protocol_from_db(row.get::<_, String>(2)?)?;
            let phase_value = row
                .get::<_, Option<String>>(8)?
                .unwrap_or_else(|| SyncPhase::Idle.as_str().to_owned());
            let phase = sync_phase_from_db(phase_value)?;
            Ok(AccountSummary {
                id: row.get(0)?,
                display_name: row.get(1)?,
                protocol,
                endpoint: row.get(3)?,
                username: row.get(4)?,
                identity_count: row.get::<_, usize>(7)?,
                cache_policy: CachePolicySummary {
                    name: row.get(5)?,
                    description: row.get(6)?,
                },
                sync_status: SyncStatusSummary {
                    phase,
                    detail: row
                        .get::<_, Option<String>>(9)?
                        .unwrap_or_else(|| "Awaiting first sync".to_owned()),
                    checkpoint: row.get(10)?,
                    last_attempt_at: row.get(11)?,
                    last_success_at: row.get(12)?,
                    last_error_at: row.get(13)?,
                    last_error_message: row.get(14)?,
                    failure_count: row.get::<_, Option<usize>>(15)?.unwrap_or_default(),
                },
                color_override: row
                    .get::<_, Option<String>>(16)?
                    .as_deref()
                    .and_then(AccountColor::parse),
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list configured accounts")
    }

    /// Returns a sync target by account identifier.
    pub fn sync_target(&self, account_id: i64) -> Result<Option<SyncTarget>> {
        let conn = self.connection()?;
        let account = conn
            .query_row(
                r#"
                SELECT
                    id,
                    display_name,
                    protocol_kind,
                    auth_kind,
                    endpoint,
                    username,
                    cache_policy_name,
                    cache_policy_description,
                    oauth2_token_endpoint,
                    oauth2_client_id,
                    oauth2_access_expires_at
                FROM accounts
                WHERE id = ?1
                "#,
                params![account_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        protocol_from_db(row.get::<_, String>(2)?)?,
                        auth_kind_from_db(row.get::<_, String>(3)?)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                    ))
                },
            )
            .optional()
            .context("failed to load the sync target")?;

        let Some((
            id,
            display_name,
            protocol,
            auth,
            endpoint,
            username,
            policy_name,
            policy_description,
            oauth2_token_endpoint,
            oauth2_client_id,
            oauth2_access_expires_at,
        )) = account
        else {
            return Ok(None);
        };

        let identities = load_identities(&conn, id)?;
        let checkpoint = conn
            .query_row(
                "SELECT checkpoint FROM sync_state WHERE account_id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .context("failed to load the sync checkpoint")?
            .flatten();

        Ok(Some(SyncTarget {
            id,
            display_name,
            protocol,
            auth,
            endpoint,
            username,
            attachments_dir: self.attachment_storage_dir().display().to_string(),
            cache_policy: CachePolicySummary {
                name: policy_name,
                description: policy_description,
            },
            identities,
            checkpoint,
            oauth2: match (auth, oauth2_token_endpoint, oauth2_client_id) {
                (AuthKind::OAuth2, Some(token_endpoint), Some(client_id)) => {
                    Some(OAuth2TargetConfig {
                        token_endpoint,
                        client_id,
                        access_token_expires_at: oauth2_access_expires_at,
                    })
                }
                _ => None,
            },
        }))
    }

    /// Persists the OAuth2 client configuration a background refresh needs.
    ///
    /// The provider client secret is deliberately not stored here; it lives
    /// in the secret store alongside the granted tokens.
    pub fn save_account_oauth2_client(
        &self,
        account_id: i64,
        token_endpoint: &str,
        client_id: &str,
    ) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE accounts SET oauth2_token_endpoint = ?2, oauth2_client_id = ?3, \
             updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![account_id, token_endpoint, client_id],
        )
        .with_context(|| format!("failed to save the OAuth2 client for account {account_id}"))?;
        Ok(())
    }

    /// Records when the current OAuth2 access token expires.
    ///
    /// `None` clears the recorded expiry, used when the provider does not
    /// report a token lifetime.
    pub fn set_account_oauth2_expiry(
        &self,
        account_id: i64,
        expires_at: Option<i64>,
    ) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE accounts SET oauth2_access_expires_at = ?2, \
             updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![account_id, expires_at],
        )
        .with_context(|| format!("failed to record the token expiry for account {account_id}"))?;
        Ok(())
    }

    /// What [`LocalStore::delete_account`] would delete besides the
    /// account's synced mail, which the server keeps.
    pub fn account_removal_impact(&self, account_id: i64) -> Result<AccountRemovalImpact> {
        let conn = self.connection()?;
        let count = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Result<usize> {
            conn.query_row(sql, params, |row| row.get::<_, usize>(0))
                .context("failed to count what removing the account deletes")
        };
        Ok(AccountRemovalImpact {
            unsent_messages: count(
                "SELECT COUNT(*) FROM outbox_jobs WHERE account_id = ?1 AND state IN (?2, ?3, ?4)",
                &[
                    &account_id,
                    &OutboxJobState::Queued.as_str(),
                    &OutboxJobState::Retrying.as_str(),
                    &OutboxJobState::Failed.as_str(),
                ],
            )?,
            sending_messages: count(
                "SELECT COUNT(*) FROM outbox_jobs WHERE account_id = ?1 AND state = ?2",
                &[&account_id, &OutboxJobState::Sending.as_str()],
            )?,
            drafts: count(
                "SELECT COUNT(*) FROM drafts WHERE account_id = ?1 \
                 AND id NOT IN (SELECT draft_id FROM outbox_jobs)",
                &[&account_id],
            )?,
        })
    }

    /// Deletes an account and everything stored locally for it: its synced
    /// mail, downloaded bodies and attachments, drafts, outbox, queued
    /// changes, address history and search index entries, and its place
    /// in the UI preferences. Nothing is deleted on the server, and the
    /// caller removes the account's keychain secrets.
    pub fn delete_account(&self, account_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the account removal")?;
        let paths = |sql: &str| -> Result<Vec<String>> {
            let mut statement = transaction.prepare(sql)?;
            let rows = statement.query_map(params![account_id], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to list the account's stored files")
        };
        let attachments = paths(
            r#"
            SELECT DISTINCT a.storage_path
            FROM message_attachments a
            JOIN messages m ON m.id = a.message_id
            JOIN threads t ON t.id = m.thread_id
            JOIN mailboxes b ON b.id = t.mailbox_id
            WHERE b.account_id = ?1
            "#,
        )?;
        let draft_files = paths(
            r#"
            SELECT d.storage_path
            FROM draft_attachments d
            JOIN drafts ON drafts.id = d.draft_id
            WHERE drafts.account_id = ?1
            "#,
        )?;
        let deleted = transaction
            .execute("DELETE FROM accounts WHERE id = ?1", params![account_id])
            .with_context(|| format!("failed to delete account {account_id}"))?;
        if deleted == 0 {
            return Ok(());
        }
        flush_search_index(&transaction)?;
        let mut preferences = load_ui_preferences(&transaction)?;
        forget_account(&mut preferences, account_id);
        store_ui_preferences(&transaction, &preferences)?;
        // Attachment blobs another account still uses (the same file
        // content) stay.
        let mut still_used = Vec::new();
        for path in &attachments {
            let used: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM message_attachments WHERE storage_path = ?1)",
                params![path],
                |row| row.get(0),
            )?;
            still_used.push(used);
        }
        transaction
            .commit()
            .context("failed to commit the account removal")?;

        // The rows are gone; a file that cannot be removed now is only
        // disk space, so it does not fail the removal.
        let storage = self.attachment_storage_dir();
        for (path, used) in attachments.iter().zip(still_used) {
            let relative = Path::new(path);
            let contained = relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)));
            if !used && contained {
                let _ = fs::remove_file(storage.join(relative));
            }
        }
        for path in draft_files {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }

    /// Sets the account's colour; `None` goes back to the colour derived
    /// from its id.
    pub fn set_account_color(&self, account_id: i64, color: Option<AccountColor>) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE accounts SET color = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![account_id, color.map(AccountColor::as_str)],
        )
        .with_context(|| format!("failed to set the colour of account {account_id}"))?;
        Ok(())
    }

    /// Sets the account's disk budget in MiB; `None` disables the budget.
    pub fn set_account_disk_budget(&self, account_id: i64, budget_mb: Option<u64>) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE accounts SET cache_policy_disk_budget_mb = ?2, \
             updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![account_id, budget_mb.map(|budget| budget as i64),],
        )
        .with_context(|| format!("failed to set the disk budget for account {account_id}"))?;
        Ok(())
    }

    /// Returns the saved UI preferences, or the defaults when none were
    /// saved yet. Records written by older or newer versions load with
    /// per-field defaults (see [`UiPreferences`]).
    pub fn ui_preferences(&self) -> Result<UiPreferences> {
        let conn = self.connection()?;
        load_ui_preferences(&conn)
    }

    /// Saves the UI preferences, replacing any previous record.
    pub fn save_ui_preferences(&self, preferences: &UiPreferences) -> Result<()> {
        let conn = self.connection()?;
        store_ui_preferences(&conn, preferences)
    }

    /// Loads the UI preferences, applies `change` and saves the result in
    /// one transaction, returning the saved preferences. Use this for
    /// single-field changes so concurrent windows do not overwrite each
    /// other's fields.
    pub fn update_ui_preferences(
        &self,
        change: impl FnOnce(&mut UiPreferences),
    ) -> Result<UiPreferences> {
        let mut conn = self.connection()?;
        // Immediate: take the write lock before reading so two concurrent
        // updates serialise instead of one failing to upgrade its lock.
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the UI preferences transaction")?;
        let mut preferences = load_ui_preferences(&transaction)?;
        change(&mut preferences);
        store_ui_preferences(&transaction, &preferences)?;
        transaction
            .commit()
            .context("failed to commit the UI preferences")?;
        Ok(preferences)
    }

    /// Returns the imported Zed theme family JSON, if the user imported
    /// one (Settings → Appearance).
    pub fn custom_theme(&self) -> Result<Option<String>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![CUSTOM_THEME_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to load the imported theme")
    }

    /// Saves an imported Zed theme family's JSON, or with `None` goes back
    /// to the bundled themes. The caller validates the JSON first.
    pub fn set_custom_theme(&self, json: Option<&str>) -> Result<()> {
        let conn = self.connection()?;
        match json {
            Some(json) => conn.execute(
                "INSERT INTO app_settings(key, value, updated_at) \
                 VALUES (?1, ?2, CURRENT_TIMESTAMP) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, \
                 updated_at = excluded.updated_at",
                params![CUSTOM_THEME_KEY, json],
            ),
            None => conn.execute(
                "DELETE FROM app_settings WHERE key = ?1",
                params![CUSTOM_THEME_KEY],
            ),
        }
        .context("failed to save the imported theme")?;
        Ok(())
    }

    /// Returns the account's configured disk budget in bytes, when set.
    ///
    /// The budget covers hydrated message bodies and downloaded attachment
    /// blobs; a missing budget means unlimited.
    pub fn account_disk_budget_bytes(&self, account_id: i64) -> Result<Option<u64>> {
        let conn = self.connection()?;
        let budget = conn
            .query_row(
                "SELECT cache_policy_disk_budget_mb FROM accounts WHERE id = ?1",
                params![account_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .context("failed to load the disk budget")?
            .flatten();
        Ok(budget.map(|budget| budget.max(0) as u64 * 1024 * 1024))
    }

    /// Returns the disk usage of hydrated bodies and attachment blobs in
    /// bytes for the account.
    ///
    /// Body sizes are the character lengths of the stored text columns, so
    /// the usage is an approximation that matches what the eviction pass
    /// frees.
    pub fn account_blob_usage(&self, account_id: i64) -> Result<u64> {
        account_blob_usage(&self.connection()?, account_id)
    }

    /// Evicts the oldest hydrated bodies and attachment blobs until the
    /// account's blob usage fits its budget, returning the freed bytes.
    ///
    /// Eviction follows the local-first design: a cleared body falls back to
    /// preview rendering and re-hydrates on open like a never-opened
    /// message, and a removed attachment returns to metadata-only. Oldest
    /// messages go first; nothing is deleted from the server.
    pub fn evict_account_blobs(&self, account_id: i64, budget_bytes: u64) -> Result<u64> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the eviction transaction")?;
        let mut usage = account_blob_usage(&transaction, account_id)?;
        if usage <= budget_bytes {
            return Ok(0);
        }

        let mut freed: u64 = 0;
        // Hydrated bodies first: they are cheap to refetch on open.
        let mut body_statement = transaction
            .prepare(
                r#"
                SELECT m.id,
                       LENGTH(COALESCE(m.body_text, '')) +
                       LENGTH(COALESCE(m.sanitized_html, ''))
                FROM messages m
                JOIN threads t ON t.id = m.thread_id
                JOIN mailboxes b ON b.id = t.mailbox_id
                WHERE b.account_id = ?1
                  AND (m.body_text IS NOT NULL OR m.sanitized_html IS NOT NULL)
                ORDER BY m.received_at ASC, m.id ASC
                "#,
            )
            .context("failed to prepare the body eviction query")?;
        while usage > budget_bytes {
            let Some((message_id, size)) = body_statement
                .query_row(params![account_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })
                .optional()
                .context("failed to load the next eviction candidate")?
            else {
                break;
            };
            transaction
                .execute(
                    "UPDATE messages SET body_text = NULL, sanitized_html = NULL, \
                     has_remote_content = 0 WHERE id = ?1",
                    params![message_id],
                )
                .with_context(|| format!("failed to evict the body of message {message_id}"))?;
            freed += size.max(0) as u64;
            usage = usage.saturating_sub(size.max(0) as u64);
        }
        drop(body_statement);

        // Attachment blobs next, oldest message first.
        let mut attachment_statement = transaction
            .prepare(
                r#"
                SELECT a.id, a.storage_path, a.size_bytes
                FROM message_attachments a
                JOIN messages m ON m.id = a.message_id
                JOIN threads t ON t.id = m.thread_id
                JOIN mailboxes b ON b.id = t.mailbox_id
                WHERE b.account_id = ?1
                ORDER BY m.received_at ASC, a.id ASC
                "#,
            )
            .context("failed to prepare the attachment eviction query")?;
        while usage > budget_bytes {
            let Some((attachment_id, storage_path, size)) = attachment_statement
                .query_row(params![account_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .optional()
                .context("failed to load the next eviction candidate")?
            else {
                break;
            };
            transaction
                .execute(
                    "DELETE FROM message_attachments WHERE id = ?1",
                    params![attachment_id],
                )
                .with_context(|| format!("failed to evict attachment {attachment_id}"))?;
            // The blob file may already be gone; eviction must not fail on it.
            let _ = std::fs::remove_file(self.attachment_storage_dir().join(&storage_path));
            freed += size.max(0) as u64;
            usage = usage.saturating_sub(size.max(0) as u64);
        }
        drop(attachment_statement);

        // Cleared bodies fall back to their previews in the index.
        flush_search_index(&transaction)?;
        transaction
            .commit()
            .context("failed to commit the eviction transaction")?;
        Ok(freed)
    }

    /// Returns all sync targets in account order.
    pub fn list_sync_targets(&self) -> Result<Vec<SyncTarget>> {
        let account_ids = self
            .list_accounts()?
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>();
        let mut targets = Vec::with_capacity(account_ids.len());
        for account_id in account_ids {
            if let Some(target) = self.sync_target(account_id)? {
                targets.push(target);
            }
        }
        Ok(targets)
    }

    /// Returns the identities configured for an account.
    pub fn list_identities(&self, account_id: i64) -> Result<Vec<IdentitySummary>> {
        let conn = self.connection()?;
        load_identities(&conn, account_id)
    }

    /// Returns the selected account overview used by the shell header.
    pub fn account_overview(&self, account_id: i64) -> Result<Option<AccountOverview>> {
        let accounts = self.list_accounts()?;
        Ok(accounts
            .into_iter()
            .find(|account| account.id == account_id)
            .map(|account| AccountOverview {
                id: account.id,
                display_name: account.display_name,
                identity_count: account.identity_count,
                cache_policy: account.cache_policy,
            }))
    }

    /// Returns global store counts for the header.
    pub fn stats(&self) -> Result<StoreStats> {
        let conn = self.connection()?;
        let account_count = query_count(&conn, "SELECT COUNT(*) FROM accounts")
            .context("failed to count accounts")?;
        let mailbox_count = query_count(&conn, "SELECT COUNT(*) FROM mailboxes")
            .context("failed to count mailboxes")?;
        let thread_count = query_count(&conn, "SELECT COUNT(*) FROM threads")
            .context("failed to count threads")?;
        let message_count = query_count(&conn, "SELECT COUNT(*) FROM messages")
            .context("failed to count messages")?;

        Ok(StoreStats {
            account_count,
            mailbox_count,
            thread_count,
            message_count,
        })
    }

    /// Writes a consistent snapshot copy of the database to `path`.
    ///
    /// Uses SQLite's `VACUUM INTO`, which produces a defragmented,
    /// self-contained backup file while the store stays usable. The
    /// target path must not already exist.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let target = path
            .to_str()
            .context("the backup path must be valid UTF-8")?
            .to_owned();
        let conn = self.connection()?;
        conn.execute("VACUUM INTO ?1", params![target])
            .with_context(|| format!("failed to back the store up to {}", path.display()))?;
        Ok(())
    }

    /// Creates a timestamped backup in `directory`, keeping the newest
    /// `keep` backups. Returns the created backup path.
    pub fn create_backup(&self, directory: impl AsRef<Path>, keep: usize) -> Result<PathBuf> {
        let directory = directory.as_ref();
        fs::create_dir_all(directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = directory.join(format!("zmail-backup-{nanos}.sqlite3"));
        self.backup_to(&path)?;

        let mut backups: Vec<PathBuf> = fs::read_dir(directory)
            .with_context(|| format!("failed to list {}", directory.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with("zmail-backup-") && name.ends_with(".sqlite3")
                })
            })
            .map(|entry| entry.path())
            .collect();
        backups.sort();

        while backups.len() > keep {
            let stale = backups.remove(0);
            remove_backup_with_sidecars(&stale)?;
        }

        Ok(path)
    }

    /// Writes the account's locally mirrored mail to `writer` in mbox
    /// format and returns the number of exported messages.
    ///
    /// The export reconstructs message headers from the stored mirror, so
    /// it is intended for archival and migration rather than as a
    /// byte-faithful copy of the original server messages.
    pub fn export_account_mbox(
        &self,
        account_id: i64,
        writer: &mut impl std::io::Write,
    ) -> Result<usize> {
        let mailboxes = self.account_mailbox_snapshots(account_id)?;
        let mut exported = 0;
        for mailbox in mailboxes {
            for thread in mailbox.threads {
                for message in thread.messages {
                    let (body, _mode) = renderable_body(
                        &message.body_preview,
                        message.body_text.as_deref(),
                        message.sanitized_html.as_deref(),
                    );
                    let sender = message
                        .sender
                        .rsplit(['<', '>'])
                        .next()
                        .map(str::trim)
                        .filter(|value| !value.is_empty() && value.contains('@'))
                        .unwrap_or("MAILER-DAEMON");
                    writeln!(writer, "From {sender} {}", message.received_at)
                        .context("failed to write the mbox entry separator")?;
                    writeln!(writer, "From: {}", message.sender)
                        .and_then(|_| writeln!(writer, "To: {}", message.recipients))
                        .and_then(|_| writeln!(writer, "Subject: {}", message.subject))
                        .and_then(|_| writeln!(writer, "Date: {}", message.received_at))
                        .and_then(|_| writeln!(writer, "Content-Type: text/plain; charset=utf-8"))
                        .and_then(|_| writeln!(writer))
                        .context("failed to write the mbox entry headers")?;
                    for line in body.lines() {
                        let escaped = if line.starts_with("From ") {
                            format!(">{line}")
                        } else {
                            line.to_owned()
                        };
                        writeln!(writer, "{escaped}")
                            .context("failed to write the mbox entry body")?;
                    }
                    writeln!(writer).context("failed to write the mbox entry terminator")?;
                    exported += 1;
                }
            }
        }
        writer.flush().context("failed to flush the mbox export")?;
        Ok(exported)
    }

    /// Lists mailboxes for a single account in display order: the
    /// special-use roles first (Inbox, Drafts, Sent, Archive, Junk,
    /// Trash), then the rest in server order.
    pub fn list_mailboxes(&self, account_id: i64) -> Result<Vec<MailboxSummary>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(&format!(
            r#"
            SELECT {MAILBOX_SUMMARY_COLUMNS}
            FROM mailboxes
            WHERE account_id = ?1
            ORDER BY {MAILBOX_ROLE_ORDER}, sort_order ASC, name ASC
            "#,
        ))?;

        let rows = statement.query_map(params![account_id], mailbox_summary_from_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list mailboxes")
    }

    /// Lists every account's mailboxes, account by account (in account
    /// order) and each account's in [`LocalStore::list_mailboxes`] order.
    pub fn list_all_mailboxes(&self) -> Result<Vec<MailboxSummary>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(&format!(
            r#"
            SELECT {MAILBOX_SUMMARY_COLUMNS}
            FROM mailboxes
            ORDER BY account_id ASC, {MAILBOX_ROLE_ORDER}, sort_order ASC, name ASC
            "#,
        ))?;
        let rows = statement.query_map([], mailbox_summary_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list every mailbox")
    }

    /// Exports the current local mailbox mirror for an account.
    pub fn account_mailbox_snapshots(&self, account_id: i64) -> Result<Vec<MailboxSnapshot>> {
        let conn = self.connection()?;
        let mailbox_rows = conn
            .prepare(
                r#"
                SELECT id, name, role, delimiter, parent_name
                FROM mailboxes
                WHERE account_id = ?1
                ORDER BY sort_order ASC, name ASC
                "#,
            )?
            .query_map(params![account_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list mailbox snapshots")?;

        let mut mailboxes = Vec::with_capacity(mailbox_rows.len());
        for (mailbox_id, name, role, delimiter, parent) in mailbox_rows {
            let thread_rows = conn
                .prepare(
                    r#"
                    SELECT
                        id,
                        COALESCE(remote_thread_id, ''),
                        subject,
                        participants,
                        preview,
                        is_flagged,
                        has_attachments,
                        unread_count,
                        latest_message_at
                    FROM threads
                    WHERE mailbox_id = ?1
                    ORDER BY latest_message_at DESC, id DESC
                    "#,
                )?
                .query_map(params![mailbox_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, usize>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to list thread snapshots")?;

            let mut threads = Vec::with_capacity(thread_rows.len());
            for (
                thread_id,
                remote_thread_id,
                subject,
                participants,
                preview,
                is_flagged,
                has_attachments,
                unread_count,
                latest_message_at,
            ) in thread_rows
            {
                let messages = conn
                    .prepare(
                        r#"
                        SELECT
                            COALESCE(messages.remote_id, ''),
                            identities.email_address,
                            messages.sender,
                            messages.reply_to,
                            messages.recipients,
                            messages.subject,
                            messages.body_preview,
                            messages.body_text,
                            messages.sanitized_html,
                            messages.has_remote_content,
                            messages.is_read,
                            messages.is_flagged,
                            messages.received_at
                        FROM messages
                        JOIN identities ON identities.id = messages.identity_id
                        WHERE messages.thread_id = ?1
                        ORDER BY messages.received_at ASC, messages.id ASC
                        "#,
                    )?
                    .query_map(params![thread_id], |row| {
                        Ok(MessageSnapshot {
                            remote_id: row.get(0)?,
                            identity_email: row.get(1)?,
                            sender: row.get(2)?,
                            reply_to: row.get(3)?,
                            recipients: row.get(4)?,
                            subject: row.get(5)?,
                            body_preview: row.get(6)?,
                            body_text: row.get(7)?,
                            sanitized_html: row.get(8)?,
                            has_remote_content: row.get::<_, bool>(9)?,
                            is_read: row.get::<_, bool>(10)?,
                            is_flagged: row.get::<_, bool>(11)?,
                            received_at: row.get(12)?,
                            attachments: Vec::new(),
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .context("failed to list message snapshots")?;

                threads.push(ThreadSnapshot {
                    remote_thread_id,
                    subject,
                    participants,
                    preview,
                    is_flagged,
                    has_attachments,
                    unread_count,
                    latest_message_at,
                    messages,
                });
            }

            mailboxes.push(MailboxSnapshot {
                name,
                role,
                delimiter,
                parent,
                threads,
            });
        }

        Ok(mailboxes)
    }

    /// Lists a visible range of threads for a mailbox, ordered by newest first.
    pub fn list_threads(&self, mailbox_id: i64, range: Range<usize>) -> Result<Vec<ThreadSummary>> {
        self.list_selection_threads(
            &MailboxSelection::Mailbox {
                account_id: 0,
                mailbox_id,
            },
            range,
        )
    }

    /// Lists a visible range of the threads a mailbox or virtual mailbox
    /// shows, newest first. Each row names its own mailbox and account.
    pub fn list_selection_threads(
        &self,
        selection: &MailboxSelection,
        range: Range<usize>,
    ) -> Result<Vec<ThreadSummary>> {
        self.list_filtered_threads(selection, ThreadFilter::All, range)
    }

    /// Like [`Self::list_selection_threads`], keeping only the threads
    /// `thread_filter` matches.
    pub fn list_filtered_threads(
        &self,
        selection: &MailboxSelection,
        thread_filter: ThreadFilter,
        range: Range<usize>,
    ) -> Result<Vec<ThreadSummary>> {
        let limit = range.end.saturating_sub(range.start) as i64;
        if limit == 0 {
            return Ok(Vec::new());
        }

        let (filter, mut values) = selection_filter(selection);
        let filter = format!("{filter}{}", thread_filter_clause(thread_filter));
        let next = values.len() + 1;
        values.push(limit.into());
        values.push((range.start as i64).into());
        let conn = self.connection()?;
        let mut statement = conn.prepare(&format!(
            r#"
            SELECT
                threads.id,
                threads.subject,
                messages.sender,
                threads.participants,
                threads.preview,
                threads.message_count,
                threads.unread_count,
                threads.is_flagged,
                threads.has_attachments,
                threads.latest_message_at,
                threads.mailbox_id,
                mailboxes.account_id
            FROM threads
            JOIN messages ON messages.id = threads.latest_message_id
            JOIN mailboxes ON mailboxes.id = threads.mailbox_id
            WHERE {filter}
            ORDER BY threads.latest_message_at DESC, threads.id DESC
            LIMIT ?{next} OFFSET ?{}
            "#,
            next + 1
        ))?;

        let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
            Ok(ThreadSummary {
                id: row.get(0)?,
                subject: row.get(1)?,
                latest_sender: row.get(2)?,
                participants: row.get(3)?,
                preview: row.get(4)?,
                message_count: row.get::<_, usize>(5)?,
                unread_count: row.get::<_, usize>(6)?,
                is_flagged: row.get::<_, bool>(7)?,
                has_attachments: row.get::<_, bool>(8)?,
                latest_message_at: row.get(9)?,
                mailbox_id: row.get(10)?,
                account_id: row.get(11)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load a thread range")
    }

    /// Returns the first thread identifier for a mailbox, if one exists.
    pub fn first_thread_id(&self, mailbox_id: i64) -> Result<Option<i64>> {
        let conn = self.connection()?;
        conn.query_row(
            r#"
            SELECT id
            FROM threads
            WHERE mailbox_id = ?1
            ORDER BY latest_message_at DESC, id DESC
            LIMIT 1
            "#,
            params![mailbox_id],
            |row| row.get(0),
        )
        .optional()
        .context("failed to query the first thread id")
    }

    /// The newest thread a mailbox or virtual mailbox shows, if any.
    pub fn first_selection_thread(
        &self,
        selection: &MailboxSelection,
    ) -> Result<Option<ThreadSummary>> {
        Ok(self
            .list_selection_threads(selection, 0..1)?
            .into_iter()
            .next())
    }

    /// Whether a mailbox or virtual mailbox lists `thread_id`.
    pub fn selection_contains(&self, selection: &MailboxSelection, thread_id: i64) -> Result<bool> {
        let conn = self.connection()?;
        let (filter, mut values) = selection_filter(selection);
        let thread = values.len() + 1;
        values.push(thread_id.into());
        conn.query_row(
            &format!(
                // The filter's anonymous `?`s come first, so they number
                // 1, 2, … before the explicit `?{thread}`.
                "SELECT EXISTS(SELECT 1 FROM threads WHERE {filter} AND threads.id = ?{thread})"
            ),
            rusqlite::params_from_iter(values),
            |row| row.get::<_, bool>(0),
        )
        .context("failed to check whether a thread is listed")
    }

    /// How many threads a mailbox or virtual mailbox shows, and how many
    /// of them are unread.
    pub fn selection_counts(&self, selection: &MailboxSelection) -> Result<ThreadCounts> {
        self.filtered_counts(selection, ThreadFilter::All)
    }

    /// Like [`Self::selection_counts`], counting only the threads
    /// `thread_filter` matches.
    pub fn filtered_counts(
        &self,
        selection: &MailboxSelection,
        thread_filter: ThreadFilter,
    ) -> Result<ThreadCounts> {
        let conn = self.connection()?;
        let (filter, values) = selection_filter(selection);
        let filter = format!("{filter}{}", thread_filter_clause(thread_filter));
        conn.query_row(
            &format!(
                r#"
                SELECT COUNT(*), COALESCE(SUM(threads.unread_count > 0), 0)
                FROM threads
                WHERE {filter}
                "#
            ),
            rusqlite::params_from_iter(values),
            |row| {
                Ok(ThreadCounts {
                    total_threads: row.get::<_, usize>(0)?,
                    unread_threads: row.get::<_, usize>(1)?,
                })
            },
        )
        .context("failed to count the listed threads")
    }

    /// Loads the latest message preview for the selected thread.
    pub fn message_preview(&self, thread_id: i64) -> Result<Option<MessagePreview>> {
        let conn = self.connection()?;
        conn.query_row(
            r#"
            SELECT
                mailboxes.name,
                threads.subject,
                messages.sender,
                threads.participants,
                identities.display_name,
                messages.body_preview,
                threads.message_count,
                threads.unread_count,
                threads.latest_message_at
            FROM threads
            JOIN mailboxes ON mailboxes.id = threads.mailbox_id
            JOIN messages ON messages.id = threads.latest_message_id
            JOIN identities ON identities.id = messages.identity_id
            WHERE threads.id = ?1
            "#,
            params![thread_id],
            |row| {
                Ok(MessagePreview {
                    mailbox_name: row.get(0)?,
                    subject: row.get(1)?,
                    sender: row.get(2)?,
                    participants: row.get(3)?,
                    identity_name: row.get(4)?,
                    body_preview: row.get(5)?,
                    message_count: row.get::<_, usize>(6)?,
                    unread_count: row.get::<_, usize>(7)?,
                    latest_message_at: row.get(8)?,
                })
            },
        )
        .optional()
        .context("failed to load the message preview")
    }

    /// Loads where a thread is and its messages' read and flag state,
    /// without the bodies [`Self::thread_view`] hydrates.
    pub fn thread_state(&self, thread_id: i64) -> Result<Option<ThreadState>> {
        let conn = self.connection()?;
        let Some((account_id, mailbox_id, mailbox_name)) = conn
            .query_row(
                r#"
                SELECT mailboxes.account_id, threads.mailbox_id, mailboxes.name
                FROM threads
                JOIN mailboxes ON mailboxes.id = threads.mailbox_id
                WHERE threads.id = ?1
                "#,
                params![thread_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .context("failed to load the thread's mailbox")?
        else {
            return Ok(None);
        };
        let mut statement = conn.prepare(
            r#"
            SELECT id, is_read, is_flagged
            FROM messages
            WHERE thread_id = ?1
            ORDER BY received_at ASC, id ASC
            "#,
        )?;
        let rows = statement.query_map(params![thread_id], |row| {
            Ok(MessageFlagState {
                message_id: row.get(0)?,
                is_read: row.get(1)?,
                is_flagged: row.get(2)?,
            })
        })?;
        let messages = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load the thread's message states")?;
        Ok(Some(ThreadState {
            id: thread_id,
            account_id,
            mailbox_id,
            mailbox_name,
            messages,
        }))
    }

    /// The account, mailbox, flag and unread state of each of
    /// `thread_ids` that still exists and, given `listed_in`, is still in
    /// that thread list, in no particular order. A multi-row selection
    /// reads this to label its actions and to drop threads that left the
    /// list.
    pub fn thread_marks(
        &self,
        thread_ids: &[i64],
        listed_in: Option<&MailboxSelection>,
    ) -> Result<Vec<ThreadMarks>> {
        let conn = self.connection()?;
        let (filter, filter_values) = match listed_in {
            Some(selection) => selection_filter(selection),
            None => ("1".to_owned(), Vec::new()),
        };
        let mut marks = Vec::with_capacity(thread_ids.len());
        // Stay well inside SQLite's bound-parameter limit.
        for chunk in thread_ids.chunks(500) {
            // The filter's anonymous `?`s come first in the SQL, so they
            // number 1, 2, … before these explicit ones.
            let first = filter_values.len() + 1;
            let placeholders = (first..first + chunk.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut statement = conn.prepare(&format!(
                r#"
                SELECT threads.id, mailboxes.account_id, threads.mailbox_id,
                       threads.is_flagged, threads.unread_count > 0
                FROM threads
                JOIN mailboxes ON mailboxes.id = threads.mailbox_id
                WHERE {filter} AND threads.id IN ({placeholders})
                "#
            ))?;
            let values = filter_values
                .iter()
                .cloned()
                .chain(chunk.iter().map(|id| rusqlite::types::Value::from(*id)));
            let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
                Ok(ThreadMarks {
                    id: row.get(0)?,
                    account_id: row.get(1)?,
                    mailbox_id: row.get(2)?,
                    is_flagged: row.get(3)?,
                    is_unread: row.get(4)?,
                })
            })?;
            for row in rows {
                marks.push(row.context("failed to load a selected thread")?);
            }
        }
        Ok(marks)
    }

    /// Puts each listed message of the thread back to the given read and
    /// flag state (from [`Self::thread_state`] before a change), queuing
    /// write-back only for messages whose state differs. Messages no longer
    /// in the thread are skipped.
    pub fn restore_message_flag_states(
        &self,
        thread_id: i64,
        states: &[MessageFlagState],
    ) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the message state transaction")?;
        let Some(location) = thread_location(&transaction, thread_id)? else {
            return Ok(());
        };
        let source_name = mailbox_name(&transaction, location.mailbox_id)?;

        for state in states {
            let current: Option<(bool, bool, Option<String>)> = transaction
                .query_row(
                    "SELECT is_read, is_flagged, remote_id FROM messages WHERE id = ?1 AND thread_id = ?2",
                    params![state.message_id, thread_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .context("failed to load a message's state")?;
            let Some((is_read, is_flagged, remote_id)) = current else {
                continue;
            };
            if (is_read, is_flagged) == (state.is_read, state.is_flagged) {
                continue;
            }
            transaction.execute(
                "UPDATE messages SET is_read = ?2, is_flagged = ?3 WHERE id = ?1",
                params![state.message_id, state.is_read, state.is_flagged],
            )?;
            let Some(remote_id) = remote_id.filter(|remote_id| !remote_id.is_empty()) else {
                continue;
            };
            if is_read != state.is_read {
                enqueue_message_op(
                    &transaction,
                    location.account_id,
                    &remote_id,
                    &source_name,
                    if state.is_read {
                        PendingOpKind::MarkRead
                    } else {
                        PendingOpKind::MarkUnread
                    },
                    None,
                )?;
            }
            if is_flagged != state.is_flagged {
                enqueue_message_op(
                    &transaction,
                    location.account_id,
                    &remote_id,
                    &source_name,
                    if state.is_flagged {
                        PendingOpKind::SetFlagged
                    } else {
                        PendingOpKind::ClearFlagged
                    },
                    None,
                )?;
            }
        }

        let (unread_count, is_flagged): (i64, bool) = transaction
            .query_row(
                r#"
                SELECT COALESCE(SUM(is_read = 0), 0), COALESCE(MAX(is_flagged), 0)
                FROM messages
                WHERE thread_id = ?1
                "#,
                params![thread_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("failed to count the thread's message states")?;
        transaction.execute(
            "UPDATE threads SET unread_count = ?2, is_flagged = ?3 WHERE id = ?1",
            params![thread_id, unread_count, is_flagged],
        )?;
        update_mailbox_unread_threads(
            &transaction,
            location.mailbox_id,
            location.unread_count > 0,
            unread_count > 0,
        )?;
        transaction
            .commit()
            .context("failed to commit the message state transaction")
    }

    /// Loads the full hydrated thread view for the Phase 3 reader.
    pub fn thread_view(&self, thread_id: i64) -> Result<Option<ThreadView>> {
        let conn = self.connection()?;
        let thread = conn
            .query_row(
                r#"
                SELECT
                    threads.id,
                    mailboxes.id,
                    mailboxes.name,
                    threads.subject,
                    threads.participants,
                    threads.message_count,
                    threads.unread_count,
                    threads.is_flagged,
                    threads.has_attachments
                FROM threads
                JOIN mailboxes ON mailboxes.id = threads.mailbox_id
                WHERE threads.id = ?1
                "#,
                params![thread_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, usize>(5)?,
                        row.get::<_, usize>(6)?,
                        row.get::<_, bool>(7)?,
                        row.get::<_, bool>(8)?,
                    ))
                },
            )
            .optional()
            .context("failed to load the thread header")?;

        let Some((
            thread_id,
            mailbox_id,
            mailbox_name,
            subject,
            participants,
            message_count,
            unread_count,
            is_flagged,
            has_attachments,
        )) = thread
        else {
            return Ok(None);
        };

        let mut statement = conn.prepare(
            r#"
            SELECT
                messages.id,
                messages.sender,
                messages.reply_to,
                messages.recipients,
                messages.subject,
                messages.body_preview,
                messages.body_text,
                messages.sanitized_html,
                messages.is_read,
                messages.is_flagged,
                messages.has_remote_content,
                messages.received_at,
                messages.sender_warning_dismissed
            FROM messages
            WHERE messages.thread_id = ?1
            ORDER BY messages.received_at ASC, messages.id ASC
            "#,
        )?;

        let mut messages = statement
            .query_map(params![thread_id], |row| {
                let body_preview = row.get::<_, String>(5)?;
                let body_text = row.get::<_, Option<String>>(6)?;
                let sanitized_html = row.get::<_, Option<String>>(7)?;
                let (rendered_body, render_mode) = renderable_body(
                    &body_preview,
                    body_text.as_deref(),
                    sanitized_html.as_deref(),
                );

                Ok(ThreadMessage {
                    id: row.get(0)?,
                    sender: row.get(1)?,
                    reply_to: row.get(2)?,
                    recipients: row.get(3)?,
                    subject: row.get(4)?,
                    body_preview,
                    body_text,
                    sanitized_html,
                    rendered_body,
                    render_mode,
                    is_read: row.get::<_, bool>(8)?,
                    is_flagged: row.get::<_, bool>(9)?,
                    has_remote_content: row.get::<_, bool>(10)?,
                    sender_warning_dismissed: row.get::<_, bool>(12)?,
                    received_at: row.get(11)?,
                    attachments: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load hydrated thread messages")?;

        let mut attachments_by_message: HashMap<i64, Vec<ReceivedAttachment>> = HashMap::new();
        {
            let mut statement = conn.prepare(
                r#"
                SELECT message_id, id, filename, content_type, size_bytes, storage_path
                FROM message_attachments
                WHERE message_id IN (SELECT id FROM messages WHERE thread_id = ?1)
                ORDER BY id ASC
                "#,
            )?;
            let rows = statement
                .query_map(params![thread_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        ReceivedAttachment {
                            id: row.get(1)?,
                            filename: row.get(2)?,
                            content_type: row.get(3)?,
                            size_bytes: row.get::<_, i64>(4)? as u64,
                            storage_path: row.get(5)?,
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to load the thread attachments")?;
            for (message_id, attachment) in rows {
                attachments_by_message
                    .entry(message_id)
                    .or_default()
                    .push(attachment);
            }
        }
        for message in &mut messages {
            if let Some(attachments) = attachments_by_message.remove(&message.id) {
                message.attachments = attachments;
            }
        }

        let has_remote_content = messages.iter().any(|message| message.has_remote_content);

        Ok(Some(ThreadView {
            id: thread_id,
            mailbox_id,
            mailbox_name,
            subject,
            participants,
            message_count,
            unread_count,
            is_flagged,
            has_attachments,
            has_remote_content,
            messages,
        }))
    }

    /// Hides a message's sender-safety warning ("Looks safe"). Returns
    /// whether the message exists.
    pub fn dismiss_sender_warning(&self, message_id: i64) -> Result<bool> {
        let conn = self.connection()?;
        let changed = conn
            .execute(
                "UPDATE messages SET sender_warning_dismissed = 1 WHERE id = ?1",
                params![message_id],
            )
            .with_context(|| {
                format!("failed to dismiss the sender warning of message {message_id}")
            })?;
        Ok(changed > 0)
    }

    /// The stored blob of a received attachment and its original filename.
    /// The filename comes from the sender: sanitise it before it names a
    /// file (`zmail_security::attachment_safety::sanitize_filename`).
    pub fn attachment_blob(&self, attachment_id: i64) -> Result<(PathBuf, String)> {
        let conn = self.connection()?;
        let (filename, storage_path) = conn
            .query_row(
                r#"
                SELECT filename, storage_path
                FROM message_attachments
                WHERE id = ?1
                "#,
                params![attachment_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .context("failed to load the attachment")?
            .with_context(|| format!("attachment {attachment_id} does not exist"))?;
        drop(conn);

        let relative = Path::new(&storage_path);
        if relative.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) {
            bail!("attachment {attachment_id} has an invalid storage path");
        }
        let source = self.attachment_storage_dir().join(relative);
        if !source.is_file() {
            bail!(
                "the stored blob for attachment {attachment_id} is missing at {}",
                source.display()
            );
        }
        Ok((source, filename))
    }

    /// Copies a received attachment into `destination_dir` using its original
    /// filename and returns the written path.
    pub fn save_attachment(
        &self,
        attachment_id: i64,
        destination_dir: impl AsRef<Path>,
    ) -> Result<PathBuf> {
        let (source, filename) = self.attachment_blob(attachment_id)?;
        let destination_dir = destination_dir.as_ref();
        fs::create_dir_all(destination_dir)
            .with_context(|| format!("failed to create {}", destination_dir.display()))?;
        let safe_filename = sanitize_filename(&filename);
        let destination = destination_dir.join(&safe_filename);
        fs::copy(&source, &destination).with_context(|| {
            format!("failed to copy the attachment to {}", destination.display())
        })?;
        Ok(destination)
    }

    /// Lists the messages of a thread whose bodies are not hydrated yet,
    /// as `(row id, remote id)` pairs. Returns `None` for a missing thread.
    pub fn messages_missing_bodies(&self, thread_id: i64) -> Result<Option<Vec<(i64, String)>>> {
        let conn = self.connection()?;
        let exists: Option<i64> = conn
            .query_row(
                "SELECT id FROM threads WHERE id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Ok(None);
        }

        let mut statement = conn.prepare(
            r#"
            SELECT id, COALESCE(remote_id, '')
            FROM messages
            WHERE thread_id = ?1
              AND body_text IS NULL
              AND sanitized_html IS NULL
            ORDER BY received_at ASC, id ASC
            "#,
        )?;
        let rows = statement
            .query_map(params![thread_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list the messages missing bodies")?;
        Ok(Some(rows))
    }

    /// Returns the account's messages the background indexer should fetch
    /// next: those without a body, newest first, skipping messages that
    /// failed [`MAX_BODY_FETCH_FAILURES`] times.
    ///
    /// With a disk budget the queue respects the eviction order. Once the
    /// hydrated bodies reach the budget only messages newer than the oldest
    /// hydrated body are returned, so each fetch displaces an older body
    /// instead of one eviction undoing the last fetch. The stored set
    /// therefore converges on the newest mail that fits.
    pub fn index_candidates(
        &self,
        account_id: i64,
        limit: usize,
        budget_bytes: Option<u64>,
    ) -> Result<Vec<IndexCandidate>> {
        let conn = self.connection()?;
        let floor = match budget_bytes {
            Some(budget) if account_blob_usage(&conn, account_id)? >= budget => conn
                .query_row(
                    r#"
                    SELECT m.received_at, m.id
                    FROM messages m
                    JOIN threads t ON t.id = m.thread_id
                    JOIN mailboxes b ON b.id = t.mailbox_id
                    WHERE b.account_id = ?1
                      AND (m.body_text IS NOT NULL OR m.sanitized_html IS NOT NULL)
                    ORDER BY m.received_at ASC, m.id ASC
                    LIMIT 1
                    "#,
                    params![account_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .context("failed to find the oldest hydrated body")?,
            _ => None,
        };
        // An empty floor sorts before every timestamp, so it admits all.
        let (floor_at, floor_id) = floor.unwrap_or_default();
        let mut statement = conn
            .prepare(
                r#"
                SELECT m.id, b.name, m.remote_id
                FROM messages m
                CROSS JOIN threads t ON t.id = m.thread_id
                JOIN mailboxes b ON b.id = t.mailbox_id
                WHERE m.body_text IS NULL AND m.sanitized_html IS NULL
                  AND b.account_id = ?1
                  AND m.body_fetch_failures < ?2
                  AND COALESCE(m.remote_id, '') <> ''
                  AND (m.received_at, m.id) > (?3, ?4)
                ORDER BY m.received_at DESC, m.id DESC
                LIMIT ?5
                "#,
            )
            .context("failed to prepare the indexing queue query")?;
        let rows = statement.query_map(
            params![
                account_id,
                MAX_BODY_FETCH_FAILURES,
                floor_at,
                floor_id,
                i64::try_from(limit).unwrap_or(i64::MAX),
            ],
            |row| {
                Ok(IndexCandidate {
                    message_id: row.get(0)?,
                    mailbox_name: row.get(1)?,
                    remote_id: row.get(2)?,
                })
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load the indexing queue")
    }

    /// Records a failed background body fetch. `permanent` failures (such
    /// as a message over the size cap) skip the message right away.
    pub fn record_body_fetch_failure(&self, message_id: i64, permanent: bool) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE messages SET body_fetch_failures = \
             CASE WHEN ?2 THEN MAX(body_fetch_failures, ?3) ELSE body_fetch_failures + 1 END \
             WHERE id = ?1",
            params![message_id, permanent, MAX_BODY_FETCH_FAILURES],
        )
        .with_context(|| format!("failed to record a body fetch failure for {message_id}"))?;
        Ok(())
    }

    /// Returns whether a message currently has a stored body.
    pub fn message_has_body(&self, message_id: i64) -> Result<bool> {
        let conn = self.connection()?;
        Ok(conn
            .query_row(
                "SELECT body_text IS NOT NULL OR sanitized_html IS NOT NULL \
                 FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .context("failed to check the message body")?
            .unwrap_or(false))
    }

    /// Reports how much of the account's mail has a searchable body.
    pub fn index_progress(&self, account_id: i64) -> Result<IndexProgress> {
        let conn = self.connection()?;
        conn.query_row(
            r#"
            SELECT
                COUNT(*),
                COALESCE(SUM(m.body_text IS NOT NULL OR m.sanitized_html IS NOT NULL), 0),
                COALESCE(SUM(m.body_text IS NULL AND m.sanitized_html IS NULL
                    AND m.body_fetch_failures >= ?2), 0)
            FROM messages m
            JOIN threads t ON t.id = m.thread_id
            JOIN mailboxes b ON b.id = t.mailbox_id
            WHERE b.account_id = ?1
            "#,
            params![account_id, MAX_BODY_FETCH_FAILURES],
            |row| {
                Ok(IndexProgress {
                    messages: row.get::<_, usize>(0)?,
                    indexed: row.get::<_, usize>(1)?,
                    skipped: row.get::<_, usize>(2)?,
                })
            },
        )
        .context("failed to measure the indexing progress")
    }

    /// Persists a fetched body (and attachments) for one stored message.
    ///
    /// Existing attachment rows for the message are replaced, so a repeated
    /// hydration cannot duplicate metadata. The thread's full-text search
    /// document is rebuilt in the same transaction, so the body text becomes
    /// searchable as soon as it is stored.
    pub fn hydrate_message_body(&self, message_id: i64, hydration: &HydratedBody) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the hydration transaction")?;
        let thread_id: Option<i64> = transaction
            .query_row(
                "SELECT thread_id FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()
            .context("failed to load the hydrated message thread")?;
        let Some(thread_id) = thread_id else {
            bail!("message {message_id} does not exist");
        };

        transaction
            .execute(
                r#"
                UPDATE messages
                SET body_text = ?2,
                    sanitized_html = ?3,
                    has_remote_content = ?4,
                    body_preview = ?5
                WHERE id = ?1
                "#,
                params![
                    message_id,
                    hydration.body_text,
                    hydration.sanitized_html,
                    hydration.has_remote_content,
                    hydration.body_preview,
                ],
            )
            .context("failed to store the hydrated body")?;
        transaction.execute(
            "DELETE FROM message_attachments WHERE message_id = ?1",
            params![message_id],
        )?;
        for attachment in &hydration.attachments {
            transaction
                .execute(
                    r#"
                    INSERT INTO message_attachments(
                        message_id,
                        filename,
                        content_type,
                        size_bytes,
                        storage_path
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    "#,
                    params![
                        message_id,
                        attachment.filename,
                        attachment.content_type,
                        attachment.size_bytes as i64,
                        attachment.storage_path,
                    ],
                )
                .context("failed to store the hydrated attachment metadata")?;
        }
        let has_attachments = !hydration.attachments.is_empty();
        transaction.execute(
            "UPDATE threads SET has_attachments = MAX(has_attachments, ?2) WHERE id = ?1",
            params![thread_id, has_attachments],
        )?;
        flush_search_index(&transaction)?;
        transaction
            .commit()
            .context("failed to commit the hydration transaction")
    }

    /// Searches one account's local mail: [`Self::search_threads_in`] with
    /// [`SearchScope::Account`].
    pub fn search_threads(
        &self,
        account_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.search_threads_in(&SearchScope::Account(account_id), query, limit)
    }

    /// [`LocalStore::search_threads`] with an explicit clock and time zone
    /// for `before:`/`after:`, for tests.
    pub fn search_threads_at<Tz: chrono::TimeZone>(
        &self,
        account_id: i64,
        query: &str,
        limit: usize,
        now: &chrono::DateTime<Tz>,
    ) -> Result<Vec<SearchHit>> {
        self.search_threads_in_at(&SearchScope::Account(account_id), query, limit, now)
    }

    /// Searches local mail in `scope`: a folder (a mailbox, or a virtual
    /// mailbox such as the Unified Inbox), one account, or every account.
    /// An `in:` clause in the query names the folder itself and takes
    /// precedence over a folder scope.
    ///
    /// `query` uses the search language of [`zmail_core::search`]: words,
    /// quoted phrases, `from:`, `to:`, `subject:`, `has:attachment`,
    /// `is:unread`/`is:read`/`is:flagged`, `in:<mailbox>`,
    /// `before:`/`after:` and `-` negation. Dates are taken in the local
    /// time zone. Returns threads newest first, which keeps the first page
    /// cheap on large mailboxes (the date index stops the scan at `limit`)
    /// and matches how mail clients list results.
    pub fn search_threads_in(
        &self,
        scope: &SearchScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.search_threads_in_at(scope, query, limit, &chrono::Local::now())
    }

    /// [`LocalStore::search_threads_in`] with an explicit clock and time
    /// zone for `before:`/`after:`, for tests.
    ///
    /// Local index hits come first. Threads the server search fallback
    /// recorded for the same query text ([`LocalStore::record_server_search_hits`])
    /// follow, marked [`SearchHit::from_server`], when they are in scope,
    /// pass the query's non-text clauses and the local index did not
    /// already find them.
    pub fn search_threads_in_at<Tz: chrono::TimeZone>(
        &self,
        scope: &SearchScope,
        query: &str,
        limit: usize,
        now: &chrono::DateTime<Tz>,
    ) -> Result<Vec<SearchHit>> {
        let parsed = zmail_core::search::SearchQuery::parse(query);
        let conn = self.connection()?;
        let mut hits =
            match search::compile_search(&parsed, scope, limit, now, search::HitSource::Local) {
                Some(compiled) => run_search(&conn, compiled, false)?,
                None => return Ok(Vec::new()),
            };
        if hits.len() < limit
            && let Some(compiled) = search::compile_search(
                &parsed,
                scope,
                limit,
                now,
                search::HitSource::ServerHits(query.trim()),
            )
        {
            let local = hits.iter().map(|hit| hit.thread_id).collect::<HashSet<_>>();
            let server = run_search(&conn, compiled, true)?
                .into_iter()
                .filter(|hit| !local.contains(&hit.thread_id));
            hits.extend(server.take(limit - hits.len()));
        }
        Ok(hits)
    }

    /// Whether a search could miss mail locally and should also ask the
    /// server: the query needs message bodies
    /// ([`SearchQuery::needs_message_bodies`](zmail_core::search::SearchQuery::needs_message_bodies))
    /// and some of the account's messages have no body in the index.
    pub fn search_needs_server(&self, account_id: i64, query: &str) -> Result<bool> {
        self.search_needs_server_in(account_id, None, query)
    }

    /// Like [`Self::search_needs_server`], looking only at the messages in
    /// `folder` (when given) for unindexed bodies.
    pub fn search_needs_server_in(
        &self,
        account_id: i64,
        folder: Option<&MailboxSelection>,
        query: &str,
    ) -> Result<bool> {
        if !zmail_core::search::SearchQuery::parse(query).needs_message_bodies() {
            return Ok(false);
        }
        let conn = self.connection()?;
        let (folder_filter, mut values) = match folder {
            Some(selection) => {
                let (filter, values) = selection_filter(selection);
                (
                    format!("AND t.id IN (SELECT threads.id FROM threads WHERE {filter})"),
                    values,
                )
            }
            None => (String::new(), Vec::new()),
        };
        values.push(account_id.into());
        conn.query_row(
            &format!(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM messages m
                    JOIN threads t ON t.id = m.thread_id
                    JOIN mailboxes b ON b.id = t.mailbox_id
                    WHERE m.body_text IS NULL AND m.sanitized_html IS NULL
                      {folder_filter}
                      AND b.account_id = ?
                )
                "#
            ),
            rusqlite::params_from_iter(values),
            |row| row.get::<_, bool>(0),
        )
        .context("failed to check for unindexed messages")
    }

    /// The stored plain-text bodies of the messages in `thread_ids`, as
    /// `(thread id, body)` pairs, newest first within each thread, so
    /// search results can show where their text matched. Messages without
    /// a text body (not downloaded yet, or HTML only) are left out.
    pub fn thread_body_texts(&self, thread_ids: &[i64]) -> Result<Vec<(i64, String)>> {
        let conn = self.connection()?;
        let mut texts = Vec::new();
        // Well under SQLite's bound-parameter limit per statement.
        for chunk in thread_ids.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut statement = conn.prepare(&format!(
                "SELECT thread_id, body_text FROM messages \
                 WHERE thread_id IN ({placeholders}) AND body_text IS NOT NULL \
                 ORDER BY thread_id, received_at DESC, id DESC"
            ))?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(chunk), |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to read message bodies")?;
            texts.extend(rows);
        }
        Ok(texts)
    }

    /// Returns the account's mailboxes as `(server name, role)` pairs in
    /// sidebar order, for choosing which mailboxes a server search covers.
    pub fn mailbox_names_and_roles(
        &self,
        account_id: i64,
    ) -> Result<Vec<(String, Option<String>)>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT name, role FROM mailboxes WHERE account_id = ?1 ORDER BY sort_order, name",
        )?;
        let rows = statement.query_map(params![account_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list the mailboxes")
    }

    /// Records the server search fallback's hits for `query`, replacing the
    /// account's previous server results, and returns the hits that match
    /// no stored message (mail that arrived after the last sync).
    pub fn record_server_search_hits(
        &self,
        account_id: i64,
        query: &str,
        hits: &[ServerHitRef],
    ) -> Result<Vec<ServerHitRef>> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction()
            .context("failed to begin the server search transaction")?;
        transaction.execute(
            "DELETE FROM server_search_hits WHERE account_id = ?1",
            params![account_id],
        )?;
        let mut unknown = Vec::new();
        {
            let mut resolve = transaction.prepare(
                r#"
                SELECT m.thread_id
                FROM messages m
                JOIN threads t ON t.id = m.thread_id
                JOIN mailboxes b ON b.id = t.mailbox_id
                WHERE m.remote_id = ?2 AND b.account_id = ?1
                  AND (?3 IS NULL OR b.name = ?3)
                "#,
            )?;
            let mut insert = transaction.prepare(
                "INSERT OR IGNORE INTO server_search_hits(account_id, query, thread_id) \
                 VALUES (?1, ?2, ?3)",
            )?;
            for hit in hits {
                let threads = resolve
                    .query_map(
                        params![account_id, hit.remote_id, hit.mailbox_name],
                        |row| row.get::<_, i64>(0),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if threads.is_empty() {
                    unknown.push(hit.clone());
                }
                for thread_id in threads {
                    insert.execute(params![account_id, query.trim(), thread_id])?;
                }
            }
        }
        transaction
            .commit()
            .context("failed to commit the server search hits")?;
        Ok(unknown)
    }

    /// Records a sync phase transition for an account.
    pub fn begin_sync_attempt(
        &self,
        account_id: i64,
        phase: SyncPhase,
        detail: impl AsRef<str>,
    ) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE sync_state
            SET
                phase = ?2,
                detail = ?3,
                last_attempt_at = ?4
            WHERE account_id = ?1
            "#,
            params![
                account_id,
                phase.as_str(),
                detail.as_ref(),
                utc_now_timestamp()
            ],
        )
        .with_context(|| format!("failed to update sync phase for account {account_id}"))?;
        Ok(())
    }

    /// Records an automatic retry backoff after a failed attempt.
    pub fn record_sync_backoff(
        &self,
        account_id: i64,
        detail: impl AsRef<str>,
        error: impl AsRef<str>,
    ) -> Result<()> {
        self.record_sync_error(account_id, SyncPhase::Backoff, detail, error)
    }

    /// Records a terminal sync failure.
    pub fn record_sync_failure(
        &self,
        account_id: i64,
        detail: impl AsRef<str>,
        error: impl AsRef<str>,
    ) -> Result<()> {
        self.record_sync_error(account_id, SyncPhase::Failed, detail, error)
    }

    /// Applies a synced mailbox snapshot for an account.
    ///
    /// The snapshot is merged into the local mirror instead of replacing
    /// it: mailboxes are matched by name, threads by their remote thread
    /// id, and messages by their remote id. Rows new to the snapshot are
    /// inserted, rows missing from the snapshot are removed, and locally
    /// changed read or flag state is preserved on rows that still exist.
    pub fn replace_account_snapshot(
        &self,
        account_id: i64,
        snapshot: &AccountSyncSnapshot,
    ) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the snapshot transaction")?;

        if let Some(identities) = &snapshot.identities {
            replace_account_identities(&transaction, account_id, identities)?;
        }

        let identity_map = identity_id_map(&transaction, account_id)?;
        let fallback_identity_id = identity_map
            .values()
            .next()
            .copied()
            .context("cannot apply a mailbox snapshot without identities")?;

        let existing_mailboxes = load_account_mailbox_ids(&transaction, account_id)?;
        let pending_remote_ids = load_pending_op_remote_ids(&transaction, account_id)?;
        let mut mail_changed = snapshot.identities.is_some();

        for (sort_order, mailbox) in snapshot.mailboxes.iter().enumerate() {
            let mailbox_id = match existing_mailboxes.get(&mailbox.name) {
                Some(existing) => {
                    if existing.role.as_deref() != mailbox.role.as_deref()
                        || existing.sort_order != sort_order as i64
                        || existing.delimiter != mailbox.delimiter
                        || existing.parent != mailbox.parent
                    {
                        transaction
                            .execute(
                                r#"
                                UPDATE mailboxes
                                SET role = ?2, sort_order = ?3, delimiter = ?4, parent_name = ?5
                                WHERE id = ?1
                                "#,
                                params![
                                    existing.id,
                                    mailbox.role,
                                    sort_order as i64,
                                    mailbox.delimiter,
                                    mailbox.parent
                                ],
                            )
                            .context("failed to update the mailbox during the snapshot merge")?;
                    }
                    existing.id
                }
                None => {
                    transaction
                        .execute(
                            r#"
                        INSERT INTO mailboxes(
                            account_id, name, role, sort_order, delimiter, parent_name,
                            total_threads, unread_threads
                        )
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0)
                        "#,
                            params![
                                account_id,
                                mailbox.name,
                                mailbox.role,
                                sort_order as i64,
                                mailbox.delimiter,
                                mailbox.parent
                            ],
                        )
                        .context("failed to insert the mailbox during the snapshot merge")?;
                    transaction.last_insert_rowid()
                }
            };

            let (kept_thread_ids, threads_changed) = merge_mailbox_threads(
                &transaction,
                mailbox_id,
                mailbox,
                &identity_map,
                fallback_identity_id,
                &pending_remote_ids,
            )?;
            let pruned = prune_mailbox_threads(&transaction, mailbox_id, &kept_thread_ids)?;
            mail_changed |= threads_changed || pruned;
            if threads_changed || pruned {
                transaction
                    .execute(
                        r#"
                UPDATE mailboxes
                SET
                    total_threads = (SELECT COUNT(*) FROM threads WHERE mailbox_id = mailboxes.id),
                    unread_threads = (
                        SELECT COUNT(*) FROM threads
                        WHERE mailbox_id = mailboxes.id AND unread_count > 0
                    )
                WHERE id = ?1
                "#,
                        params![mailbox_id],
                    )
                    .context("failed to refresh the mailbox counters during the snapshot merge")?;
            }
        }

        let snapshot_names: HashSet<&str> = snapshot
            .mailboxes
            .iter()
            .map(|mailbox| mailbox.name.as_str())
            .collect();
        for (name, existing) in &existing_mailboxes {
            if !snapshot_names.contains(name.as_str()) {
                transaction
                    .execute("DELETE FROM mailboxes WHERE id = ?1", params![existing.id])
                    .with_context(|| {
                        format!("failed to remove the {name} mailbox missing from the snapshot")
                    })?;
                mail_changed = true;
            }
        }

        // Autocomplete follows the mail: the history is rebuilt from the
        // mirror whenever the snapshot changed it.
        if mail_changed {
            address_history::rebuild_account_address_history(&transaction, account_id)?;
        }
        flush_search_index(&transaction)?;
        transaction
            .commit()
            .context("failed to commit the snapshot transaction")
    }

    /// Records a successful sync and resets the failure counter.
    pub fn record_sync_success(
        &self,
        account_id: i64,
        detail: impl AsRef<str>,
        checkpoint: impl AsRef<str>,
    ) -> Result<()> {
        let conn = self.connection()?;
        let now = utc_now_timestamp();
        conn.execute(
            r#"
            UPDATE sync_state
            SET
                phase = ?2,
                detail = ?3,
                checkpoint = ?4,
                last_success_at = ?5,
                last_error_at = NULL,
                last_error_message = NULL,
                failure_count = 0
            WHERE account_id = ?1
            "#,
            params![
                account_id,
                SyncPhase::Idle.as_str(),
                detail.as_ref(),
                checkpoint.as_ref(),
                now,
            ],
        )
        .with_context(|| format!("failed to record sync success for account {account_id}"))?;
        Ok(())
    }

    /// Returns the first mailbox id for the role on the selected account.
    pub fn mailbox_id_by_role(&self, account_id: i64, role: &str) -> Result<Option<i64>> {
        let conn = self.connection()?;
        conn.query_row(
            r#"
            SELECT id
            FROM mailboxes
            WHERE account_id = ?1 AND lower(role) = lower(?2)
            ORDER BY sort_order ASC, id ASC
            LIMIT 1
            "#,
            params![account_id, role],
            |row| row.get(0),
        )
        .optional()
        .context("failed to load mailbox by role")
    }

    /// Marks every message in the thread as flagged or unflagged locally and
    /// queues the matching server write-back operations.
    pub fn set_thread_flagged(&self, thread_id: i64, flagged: bool) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the flag-state transaction")?;
        let Some(location) = thread_location(&transaction, thread_id)? else {
            return Ok(());
        };

        transaction
            .execute(
                "UPDATE messages SET is_flagged = ?2 WHERE thread_id = ?1",
                params![thread_id, flagged],
            )
            .with_context(|| format!("failed to update message flags for thread {thread_id}"))?;
        transaction
            .execute(
                "UPDATE threads SET is_flagged = ?2 WHERE id = ?1",
                params![thread_id, flagged],
            )
            .with_context(|| {
                format!("failed to update thread flag state for thread {thread_id}")
            })?;
        let source_name = mailbox_name(&transaction, location.mailbox_id)?;
        enqueue_thread_ops(
            &transaction,
            location.account_id,
            thread_id,
            &source_name,
            if flagged {
                PendingOpKind::SetFlagged
            } else {
                PendingOpKind::ClearFlagged
            },
            None,
        )?;
        transaction
            .commit()
            .context("failed to commit the flag-state transaction")
    }

    /// Marks every message in the thread as read or unread locally and
    /// queues the matching server write-back operations.
    pub fn set_thread_read(&self, thread_id: i64, read: bool) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the read-state transaction")?;
        let Some(location) = thread_location(&transaction, thread_id)? else {
            return Ok(());
        };

        let next_unread_count = if read { 0 } else { location.message_count };
        transaction.execute(
            "UPDATE messages SET is_read = ?2 WHERE thread_id = ?1",
            params![thread_id, read],
        )?;
        transaction.execute(
            "UPDATE threads SET unread_count = ?2 WHERE id = ?1",
            params![thread_id, next_unread_count as i64],
        )?;
        update_mailbox_unread_threads(
            &transaction,
            location.mailbox_id,
            location.unread_count > 0,
            next_unread_count > 0,
        )?;
        let source_name = mailbox_name(&transaction, location.mailbox_id)?;
        enqueue_thread_ops(
            &transaction,
            location.account_id,
            thread_id,
            &source_name,
            if read {
                PendingOpKind::MarkRead
            } else {
                PendingOpKind::MarkUnread
            },
            None,
        )?;
        transaction
            .commit()
            .context("failed to commit the read-state transaction")
    }

    /// Moves a thread between local mailboxes on the same account and
    /// queues the matching server write-back operations.
    pub fn move_thread(&self, thread_id: i64, destination_mailbox_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the move transaction")?;
        let Some(location) = thread_location(&transaction, thread_id)? else {
            return Ok(());
        };

        if location.mailbox_id == destination_mailbox_id {
            return Ok(());
        }

        let Some(destination) = mailbox_identity(&transaction, destination_mailbox_id)? else {
            bail!("destination mailbox {destination_mailbox_id} does not exist");
        };
        if destination.account_id != location.account_id {
            bail!("threads can only move within the same account");
        }

        let source_name = mailbox_name(&transaction, location.mailbox_id)?;
        let destination_name = mailbox_name(&transaction, destination_mailbox_id)?;
        move_thread_in_transaction(
            &transaction,
            thread_id,
            &location,
            destination_mailbox_id,
            destination.role.as_deref(),
        )?;
        enqueue_thread_ops(
            &transaction,
            location.account_id,
            thread_id,
            &source_name,
            PendingOpKind::Move,
            Some(&destination_name),
        )?;
        // Search documents do not carry the mailbox, so a move leaves the
        // index untouched.
        transaction
            .commit()
            .context("failed to commit the move transaction")
    }

    /// Moves a thread into the archive mailbox when one exists.
    pub fn archive_thread(&self, thread_id: i64) -> Result<()> {
        let Some(location) = self.thread_location(thread_id)? else {
            return Ok(());
        };
        let Some(archive_mailbox_id) = self.mailbox_id_by_role(location.account_id, "archive")?
        else {
            bail!(
                "no archive mailbox exists for account {}",
                location.account_id
            );
        };
        self.move_thread(thread_id, archive_mailbox_id)
    }

    /// Deletes a thread locally, preferring the trash mailbox when one exists,
    /// and queues the matching server write-back operations.
    pub fn delete_thread(&self, thread_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the delete transaction")?;
        let Some(location) = thread_location(&transaction, thread_id)? else {
            return Ok(());
        };

        let trash_mailbox = mailbox_id_by_role(&transaction, location.account_id, "trash")?;
        let source_mailbox = mailbox_identity(&transaction, location.mailbox_id)?
            .context("source mailbox should exist for the thread being deleted")?;
        let source_name = mailbox_name(&transaction, location.mailbox_id)?;

        if let Some(trash_mailbox_id) = trash_mailbox
            && trash_mailbox_id != location.mailbox_id
        {
            let trash_name = mailbox_name(&transaction, trash_mailbox_id)?;
            move_thread_in_transaction(
                &transaction,
                thread_id,
                &location,
                trash_mailbox_id,
                Some("trash"),
            )?;
            enqueue_thread_ops(
                &transaction,
                location.account_id,
                thread_id,
                &source_name,
                PendingOpKind::Move,
                Some(&trash_name),
            )?;
            return transaction
                .commit()
                .context("failed to commit the trash move transaction");
        }

        // Without a trash mailbox the local rows are removed; the server
        // operation mirrors that intent once write-back completes.
        enqueue_thread_ops(
            &transaction,
            location.account_id,
            thread_id,
            &source_name,
            PendingOpKind::Destroy,
            None,
        )?;
        transaction.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])?;
        transaction.execute(
            r#"
            UPDATE mailboxes
            SET
                total_threads = MAX(total_threads - 1, 0),
                unread_threads = MAX(unread_threads - ?2, 0)
            WHERE id = ?1
            "#,
            params![source_mailbox.id, i64::from(location.unread_count > 0)],
        )?;
        flush_search_index(&transaction)?;
        transaction
            .commit()
            .context("failed to commit the delete transaction")
    }

    /// Returns the server name of the account's mailbox for a role.
    pub fn mailbox_name_by_role(&self, account_id: i64, role: &str) -> Result<Option<String>> {
        let conn = self.connection()?;
        conn.query_row(
            r#"
            SELECT name
            FROM mailboxes
            WHERE account_id = ?1 AND lower(role) = lower(?2)
            ORDER BY sort_order ASC, id ASC
            LIMIT 1
            "#,
            params![account_id, role],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to load the mailbox name by role")
    }

    /// Lists the queued server write-back operations for an account.
    pub fn list_pending_ops(&self, account_id: i64) -> Result<Vec<PendingOp>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            r#"
            SELECT id, account_id, op, source_mailbox, remote_id, target_mailbox, attempts, last_error
            FROM pending_ops
            WHERE account_id = ?1 AND state = 'pending'
            ORDER BY id ASC
            "#,
        )?;
        let rows = statement.query_map(params![account_id], |row| {
            Ok(PendingOp {
                id: row.get(0)?,
                account_id: row.get(1)?,
                kind: PendingOpKind::parse(&row.get::<_, String>(2)?).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "unknown pending op kind".to_owned(),
                        )),
                    )
                })?,
                source_mailbox: row.get(3)?,
                remote_id: row.get(4)?,
                target_mailbox: row.get(5)?,
                attempts: row.get(6)?,
                last_error: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list pending mailbox operations")
    }

    /// Returns the number of queued write-back operations for an account.
    pub fn pending_op_count(&self, account_id: i64) -> Result<usize> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT COUNT(*) FROM pending_ops WHERE account_id = ?1 AND state = 'pending'",
            params![account_id],
            |row| row.get::<_, usize>(0),
        )
        .context("failed to count the pending write-back operations")
    }

    /// Removes a write-back operation after the server accepted it.
    ///
    /// An IMAP move gives the message a new uid in the target mailbox,
    /// which only the next snapshot brings back. Until then the message
    /// has no remote id, so no change made in between can address another
    /// message that happens to hold its old uid there; such a change stays
    /// local and the snapshot then shows the server's state.
    pub fn complete_pending_op(&self, op_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the write-back completion transaction")?;
        let imap_move: Option<(i64, String, String)> = transaction
            .query_row(
                r#"
                SELECT pending_ops.account_id, pending_ops.remote_id, pending_ops.target_mailbox
                FROM pending_ops
                JOIN accounts ON accounts.id = pending_ops.account_id
                WHERE pending_ops.id = ?1
                  AND pending_ops.op = ?2
                  AND pending_ops.target_mailbox IS NOT NULL
                  AND accounts.protocol_kind = ?3
                "#,
                params![
                    op_id,
                    PendingOpKind::Move.as_str(),
                    ProtocolKind::Imap.as_str()
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .with_context(|| format!("failed to load pending operation {op_id}"))?;
        if let Some((account_id, remote_id, target_mailbox)) = imap_move {
            transaction
                .execute(
                    r#"
                    UPDATE messages
                    SET remote_id = NULL
                    WHERE remote_id = ?2
                      AND thread_id IN (
                        SELECT threads.id
                        FROM threads
                        JOIN mailboxes ON mailboxes.id = threads.mailbox_id
                        WHERE mailboxes.account_id = ?1 AND mailboxes.name = ?3
                      )
                    "#,
                    params![account_id, remote_id, target_mailbox],
                )
                .with_context(|| format!("failed to retire the uid moved by operation {op_id}"))?;
        }
        transaction
            .execute("DELETE FROM pending_ops WHERE id = ?1", params![op_id])
            .with_context(|| format!("failed to complete pending operation {op_id}"))?;
        transaction
            .commit()
            .context("failed to commit the write-back completion")
    }

    /// Lists write-back operations that exhausted their retry budget.
    pub fn list_dead_ops(&self, account_id: i64) -> Result<Vec<PendingOp>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            r#"
            SELECT id, account_id, op, source_mailbox, remote_id, target_mailbox, attempts, last_error
            FROM pending_ops
            WHERE account_id = ?1 AND state = 'dead'
            ORDER BY id ASC
            "#,
        )?;
        let rows = statement.query_map(params![account_id], |row| {
            Ok(PendingOp {
                id: row.get(0)?,
                account_id: row.get(1)?,
                kind: PendingOpKind::parse(&row.get::<_, String>(2)?).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "unknown pending op kind".to_owned(),
                        )),
                    )
                })?,
                source_mailbox: row.get(3)?,
                remote_id: row.get(4)?,
                target_mailbox: row.get(5)?,
                attempts: row.get(6)?,
                last_error: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list dead-lettered mailbox operations")
    }

    /// Counts write-back operations that exhausted their retry budget.
    pub fn dead_op_count(&self, account_id: i64) -> Result<usize> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT COUNT(*) FROM pending_ops WHERE account_id = ?1 AND state = 'dead'",
            params![account_id],
            |row| row.get::<_, usize>(0),
        )
        .context("failed to count the dead-lettered write-back operations")
    }

    /// Moves a write-back operation to the dead-letter state, stopping
    /// further retries.
    pub fn mark_pending_op_dead(&self, op_id: i64, error: impl AsRef<str>) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE pending_ops
            SET attempts = attempts + 1, last_error = ?2, state = 'dead'
            WHERE id = ?1
            "#,
            params![op_id, error.as_ref()],
        )
        .with_context(|| format!("failed to dead-letter pending operation {op_id}"))?;
        Ok(())
    }

    /// Requeues a dead-lettered operation, resetting its retry budget.
    pub fn retry_dead_op(&self, op_id: i64) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE pending_ops
            SET attempts = 0, last_error = NULL, state = 'pending'
            WHERE id = ?1 AND state = 'dead'
            "#,
            params![op_id],
        )
        .with_context(|| format!("failed to requeue dead operation {op_id}"))?;
        Ok(())
    }

    /// Discards a dead-lettered operation record.
    pub fn discard_dead_op(&self, op_id: i64) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "DELETE FROM pending_ops WHERE id = ?1 AND state = 'dead'",
            params![op_id],
        )
        .with_context(|| format!("failed to discard dead operation {op_id}"))?;
        Ok(())
    }

    /// Records a failed write-back attempt; the operation stays queued.
    pub fn record_pending_op_failure(&self, op_id: i64, error: impl AsRef<str>) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE pending_ops
            SET attempts = attempts + 1, last_error = ?2
            WHERE id = ?1
            "#,
            params![op_id, error.as_ref()],
        )
        .with_context(|| format!("failed to record the failure of pending operation {op_id}"))?;
        Ok(())
    }

    /// Creates a new local draft and returns its identifier.
    pub fn create_draft(&self, draft: &NewDraft) -> Result<i64> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the draft creation transaction")?;
        let identity = load_identity_snapshot(&transaction, draft.account_id, draft.identity_id)?
            .with_context(|| {
            format!(
                "identity {} does not belong to account {}",
                draft.identity_id, draft.account_id
            )
        })?;
        let now = utc_now_timestamp();
        let body = apply_default_signature(&draft.body, &identity.signature);
        transaction.execute(
            r#"
            INSERT INTO drafts(
                account_id,
                identity_id,
                identity_name,
                identity_email,
                identity_signature,
                state,
                to_json,
                cc_json,
                bcc_json,
                subject,
                body,
                last_error_message,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, ?13)
            "#,
            params![
                draft.account_id,
                identity.id,
                identity.display_name,
                identity.email_address,
                identity.signature,
                DraftState::Editing.as_str(),
                recipients_to_json(&draft.to)?,
                recipients_to_json(&draft.cc)?,
                recipients_to_json(&draft.bcc)?,
                draft.subject,
                body,
                now,
                now,
            ],
        )?;
        let draft_id = transaction.last_insert_rowid();
        transaction
            .commit()
            .context("failed to commit the draft creation transaction")?;
        Ok(draft_id)
    }

    /// Lists drafts for one account or across the workspace when no filter is supplied.
    pub fn list_drafts(&self, account_id: Option<i64>) -> Result<Vec<DraftSummary>> {
        let conn = self.connection()?;
        let sql = r#"
            SELECT
                drafts.id,
                drafts.account_id,
                drafts.identity_id,
                drafts.identity_name,
                drafts.to_json,
                drafts.cc_json,
                drafts.bcc_json,
                drafts.subject,
                drafts.state,
                drafts.updated_at,
                COUNT(draft_attachments.id) AS attachment_count
            FROM drafts
            LEFT JOIN draft_attachments ON draft_attachments.draft_id = drafts.id
            WHERE (?1 IS NULL OR drafts.account_id = ?1)
            GROUP BY
                drafts.id,
                drafts.account_id,
                drafts.identity_id,
                drafts.identity_name,
                drafts.to_json,
                drafts.cc_json,
                drafts.bcc_json,
                drafts.subject,
                drafts.state,
                drafts.updated_at
            ORDER BY drafts.updated_at DESC, drafts.id DESC
        "#;
        let mut statement = conn.prepare(sql)?;
        let rows = statement.query_map(params![account_id], |row| {
            let to = recipients_from_json(row.get::<_, String>(4)?).map_err(to_from_sql_error)?;
            let cc = recipients_from_json(row.get::<_, String>(5)?).map_err(to_from_sql_error)?;
            let bcc = recipients_from_json(row.get::<_, String>(6)?).map_err(to_from_sql_error)?;
            Ok(DraftSummary {
                id: row.get(0)?,
                account_id: row.get(1)?,
                identity_id: row.get(2)?,
                identity_name: row.get(3)?,
                recipient_summary: summarize_recipients(&to, &cc, &bcc),
                subject: row.get(7)?,
                state: draft_state_from_db(row.get::<_, String>(8)?)?,
                updated_at: row.get(9)?,
                attachment_count: row.get::<_, usize>(10)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list drafts")
    }

    /// Loads the full draft detail for compose and send.
    pub fn draft_detail(&self, draft_id: i64) -> Result<Option<DraftDetail>> {
        let conn = self.connection()?;
        let draft = conn
            .query_row(
                r#"
                SELECT
                    id,
                    account_id,
                    identity_id,
                    identity_name,
                    identity_email,
                    identity_signature,
                    state,
                    to_json,
                    cc_json,
                    bcc_json,
                    subject,
                    body,
                    updated_at,
                    queued_at,
                    sent_at,
                    last_error_message
                FROM drafts
                WHERE id = ?1
                "#,
                params![draft_id],
                |row| {
                    Ok(DraftDetail {
                        id: row.get(0)?,
                        account_id: row.get(1)?,
                        identity_id: row.get(2)?,
                        identity_name: row.get(3)?,
                        identity_email: row.get(4)?,
                        identity_signature: row.get(5)?,
                        state: draft_state_from_db(row.get::<_, String>(6)?)?,
                        to: recipients_from_json(row.get::<_, String>(7)?)
                            .map_err(to_from_sql_error)?,
                        cc: recipients_from_json(row.get::<_, String>(8)?)
                            .map_err(to_from_sql_error)?,
                        bcc: recipients_from_json(row.get::<_, String>(9)?)
                            .map_err(to_from_sql_error)?,
                        subject: row.get(10)?,
                        body: row.get(11)?,
                        attachments: Vec::new(),
                        updated_at: row.get(12)?,
                        queued_at: row.get(13)?,
                        sent_at: row.get(14)?,
                        last_error_message: row.get(15)?,
                    })
                },
            )
            .optional()
            .context("failed to load the draft detail")?;

        let Some(mut draft) = draft else {
            return Ok(None);
        };
        draft.attachments = load_draft_attachments(&conn, draft_id)?;
        Ok(Some(draft))
    }

    /// Persists an autosave update for a draft.
    pub fn update_draft(&self, draft_id: i64, update: &DraftUpdate) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the draft autosave transaction")?;
        let (account_id, state) = transaction
            .query_row(
                "SELECT account_id, state FROM drafts WHERE id = ?1",
                params![draft_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        draft_state_from_db(row.get::<_, String>(1)?)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("draft {draft_id} does not exist"))?;
        ensure_draft_is_editable(state)?;
        let identity = load_identity_snapshot(&transaction, account_id, update.identity_id)?
            .with_context(|| {
                format!(
                    "identity {} does not belong to account {}",
                    update.identity_id, account_id
                )
            })?;
        let now = utc_now_timestamp();
        let next_body = if update.body.trim().is_empty() {
            apply_default_signature(&update.body, &identity.signature)
        } else {
            update.body.clone()
        };
        transaction.execute(
            r#"
            UPDATE drafts
            SET
                identity_id = ?2,
                identity_name = ?3,
                identity_email = ?4,
                identity_signature = ?5,
                state = ?6,
                to_json = ?7,
                cc_json = ?8,
                bcc_json = ?9,
                subject = ?10,
                body = ?11,
                last_error_message = NULL,
                updated_at = ?12
            WHERE id = ?1
            "#,
            params![
                draft_id,
                identity.id,
                identity.display_name,
                identity.email_address,
                identity.signature,
                DraftState::Editing.as_str(),
                recipients_to_json(&update.to)?,
                recipients_to_json(&update.cc)?,
                recipients_to_json(&update.bcc)?,
                update.subject,
                next_body,
                now,
            ],
        )?;
        transaction
            .commit()
            .context("failed to commit the draft autosave transaction")
    }

    /// Adds an attachment to a local draft.
    pub fn add_draft_attachment(
        &self,
        draft_id: i64,
        source_path: impl AsRef<Path>,
    ) -> Result<DraftAttachment> {
        let source_path = source_path.as_ref();
        let filename = source_path
            .file_name()
            .and_then(|filename| filename.to_str())
            .with_context(|| format!("attachment path {} has no filename", source_path.display()))?
            .to_owned();
        let metadata = fs::metadata(source_path)
            .with_context(|| format!("failed to inspect {}", source_path.display()))?;
        let size_bytes = metadata.len();
        let content_type = guess_content_type(source_path);
        let storage_path = self.attachment_storage_dir().join(format!(
            "draft-{draft_id}-{}-{}",
            EPHEMERAL_COUNTER.fetch_add(1, Ordering::Relaxed),
            sanitize_filename(&filename)
        ));
        if let Some(parent) = storage_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(source_path, &storage_path).with_context(|| {
            format!(
                "failed to copy attachment {} into {}",
                source_path.display(),
                storage_path.display()
            )
        })?;

        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the attachment transaction")?;
        let state = transaction
            .query_row(
                "SELECT state FROM drafts WHERE id = ?1",
                params![draft_id],
                |row| draft_state_from_db(row.get::<_, String>(0)?),
            )
            .optional()?
            .with_context(|| format!("draft {draft_id} does not exist"))?;
        ensure_draft_is_editable(state)?;
        let now = utc_now_timestamp();
        transaction.execute(
            r#"
            INSERT INTO draft_attachments(
                draft_id,
                filename,
                content_type,
                size_bytes,
                original_path,
                storage_path,
                created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                draft_id,
                filename,
                content_type,
                size_bytes as i64,
                source_path.display().to_string(),
                storage_path.display().to_string(),
                now,
            ],
        )?;
        let attachment_id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE drafts SET state = ?2, last_error_message = NULL, updated_at = ?3 WHERE id = ?1",
            params![draft_id, DraftState::Editing.as_str(), now],
        )?;
        transaction
            .commit()
            .context("failed to commit the attachment transaction")?;

        Ok(DraftAttachment {
            id: attachment_id,
            filename,
            content_type,
            size_bytes,
            original_path: source_path.display().to_string(),
            storage_path: storage_path.display().to_string(),
        })
    }

    /// Removes an attachment from a draft and deletes its stored copy.
    pub fn remove_draft_attachment(&self, attachment_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the attachment removal transaction")?;
        let attachment = transaction
            .query_row(
                r#"
                SELECT
                    draft_attachments.draft_id,
                    draft_attachments.storage_path,
                    drafts.state
                FROM draft_attachments
                JOIN drafts ON drafts.id = draft_attachments.draft_id
                WHERE draft_attachments.id = ?1
                "#,
                params![attachment_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        draft_state_from_db(row.get::<_, String>(2)?)?,
                    ))
                },
            )
            .optional()?;
        let Some((draft_id, storage_path, state)) = attachment else {
            return Ok(());
        };
        ensure_draft_is_editable(state)?;
        let now = utc_now_timestamp();
        transaction.execute(
            "DELETE FROM draft_attachments WHERE id = ?1",
            params![attachment_id],
        )?;
        transaction.execute(
            "UPDATE drafts SET state = ?2, last_error_message = NULL, updated_at = ?3 WHERE id = ?1",
            params![draft_id, DraftState::Editing.as_str(), now],
        )?;
        transaction
            .commit()
            .context("failed to commit the attachment removal transaction")?;
        let path = PathBuf::from(storage_path);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to delete {}", path.display()))
            }
        }
    }

    /// Queues a draft for background delivery and returns the new outbox job identifier.
    pub fn queue_draft(&self, draft_id: i64) -> Result<i64> {
        self.queue_draft_after(draft_id, Duration::ZERO)
    }

    /// Queues a draft for delivery no sooner than `delay` from now (the
    /// undo-send window) and returns the new outbox job identifier.
    ///
    /// The job's `next_attempt_at` is set to the first whole second at or
    /// after the delay ends, and [`Self::claim_due_outbox_job`] never
    /// claims a job before then, so the message cannot leave early;
    /// [`Self::cancel_queued_outbox_job`] can take it back until it is
    /// claimed.
    pub fn queue_draft_after(&self, draft_id: i64, delay: Duration) -> Result<i64> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the outbox queue transaction")?;
        let draft = transaction
            .query_row(
                r#"
                SELECT account_id, state, to_json, cc_json, bcc_json
                FROM drafts
                WHERE id = ?1
                "#,
                params![draft_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        draft_state_from_db(row.get::<_, String>(1)?)?,
                        recipients_from_json(row.get::<_, String>(2)?)
                            .map_err(to_from_sql_error)?,
                        recipients_from_json(row.get::<_, String>(3)?)
                            .map_err(to_from_sql_error)?,
                        recipients_from_json(row.get::<_, String>(4)?)
                            .map_err(to_from_sql_error)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("draft {draft_id} does not exist"))?;
        if matches!(draft.1, DraftState::Queued | DraftState::Sending) {
            bail!("draft {draft_id} is already queued");
        }
        if draft.1 == DraftState::Sent {
            bail!("draft {draft_id} has already been sent");
        }
        if draft.2.is_empty() && draft.3.is_empty() && draft.4.is_empty() {
            bail!("draft {draft_id} has no recipients");
        }

        let now = utc_now_timestamp();
        let due = if delay.is_zero() {
            now.clone()
        } else {
            utc_timestamp_at_least(delay)
        };
        transaction.execute(
            r#"
            INSERT INTO outbox_jobs(
                draft_id,
                account_id,
                state,
                attempt_count,
                next_attempt_at,
                last_error_message,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, 0, ?4, NULL, ?5, ?6)
            "#,
            params![
                draft_id,
                draft.0,
                OutboxJobState::Queued.as_str(),
                due,
                now,
                now,
            ],
        )?;
        let job_id = transaction.last_insert_rowid();
        transaction.execute(
            r#"
            UPDATE drafts
            SET
                state = ?2,
                queued_at = ?3,
                sent_at = NULL,
                last_error_message = NULL,
                updated_at = ?4
            WHERE id = ?1
            "#,
            params![draft_id, DraftState::Queued.as_str(), now, now],
        )?;
        transaction
            .commit()
            .context("failed to commit the outbox queue transaction")?;
        Ok(job_id)
    }

    /// Takes back a queued message that has not started sending (Undo
    /// Send): the outbox job is deleted and its draft can be edited again.
    ///
    /// Returns `false`, changing nothing, when it is too late: the job has
    /// been claimed for delivery (or has gone). Claiming and cancelling
    /// both take the write lock first, so a message is never both sent and
    /// cancelled.
    pub fn cancel_queued_outbox_job(&self, job_id: i64) -> Result<bool> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the outbox cancel transaction")?;
        let job = transaction
            .query_row(
                "SELECT draft_id, state FROM outbox_jobs WHERE id = ?1",
                params![job_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        outbox_job_state_from_db(row.get::<_, String>(1)?)?,
                    ))
                },
            )
            .optional()?;
        let Some((draft_id, OutboxJobState::Queued)) = job else {
            return Ok(false);
        };
        let now = utc_now_timestamp();
        transaction.execute("DELETE FROM outbox_jobs WHERE id = ?1", params![job_id])?;
        transaction.execute(
            r#"
            UPDATE drafts
            SET
                state = ?2,
                queued_at = NULL,
                last_error_message = NULL,
                updated_at = ?3
            WHERE id = ?1
            "#,
            params![draft_id, DraftState::Editing.as_str(), now],
        )?;
        transaction
            .commit()
            .context("failed to commit the outbox cancel transaction")?;
        Ok(true)
    }

    /// Lists outbox jobs for one account or across the workspace.
    pub fn list_outbox_jobs(&self, account_id: Option<i64>) -> Result<Vec<OutboxJobSummary>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            r#"
            SELECT
                id,
                draft_id,
                account_id,
                state,
                attempt_count,
                next_attempt_at,
                last_error_message
            FROM outbox_jobs
            WHERE (?1 IS NULL OR account_id = ?1)
            ORDER BY updated_at DESC, id DESC
            "#,
        )?;
        let rows = statement.query_map(params![account_id], |row| {
            Ok(OutboxJobSummary {
                id: row.get(0)?,
                draft_id: row.get(1)?,
                account_id: row.get(2)?,
                state: outbox_job_state_from_db(row.get::<_, String>(3)?)?,
                attempt_count: row.get::<_, usize>(4)?,
                next_attempt_at: row.get(5)?,
                last_error_message: row.get(6)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list outbox jobs")
    }

    /// Claims the next due outbox job and marks it as sending.
    pub fn claim_due_outbox_job(
        &self,
        account_id: Option<i64>,
    ) -> Result<Option<PendingOutboxJob>> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the outbox claim transaction")?;
        let now = utc_now_timestamp();
        let job_id = transaction
            .query_row(
                r#"
                SELECT id
                FROM outbox_jobs
                WHERE (?1 IS NULL OR account_id = ?1)
                  AND state IN (?2, ?3)
                  AND (next_attempt_at IS NULL OR next_attempt_at <= ?4)
                ORDER BY COALESCE(next_attempt_at, created_at) ASC, id ASC
                LIMIT 1
                "#,
                params![
                    account_id,
                    OutboxJobState::Queued.as_str(),
                    OutboxJobState::Retrying.as_str(),
                    now,
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(job_id) = job_id else {
            return Ok(None);
        };

        let draft_id = transaction.query_row(
            "SELECT draft_id FROM outbox_jobs WHERE id = ?1",
            params![job_id],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.execute(
            r#"
            UPDATE outbox_jobs
            SET
                state = ?2,
                attempt_count = attempt_count + 1,
                last_attempt_at = ?3,
                updated_at = ?4
            WHERE id = ?1
            "#,
            params![job_id, OutboxJobState::Sending.as_str(), now, now],
        )?;
        transaction.execute(
            r#"
            UPDATE drafts
            SET
                state = ?2,
                last_error_message = NULL,
                updated_at = ?3
            WHERE id = ?1
            "#,
            params![draft_id, DraftState::Sending.as_str(), now],
        )?;
        transaction
            .commit()
            .context("failed to commit the outbox claim transaction")?;
        self.pending_outbox_job(job_id)
    }

    /// Records a transient delivery failure and schedules the next retry attempt.
    pub fn record_outbox_retry(
        &self,
        job_id: i64,
        error: impl AsRef<str>,
        retry_after: Duration,
    ) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the outbox retry transaction")?;
        let draft_id = transaction
            .query_row(
                "SELECT draft_id FROM outbox_jobs WHERE id = ?1",
                params![job_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .with_context(|| format!("outbox job {job_id} does not exist"))?;
        let now = utc_now_timestamp();
        let next_attempt_at = utc_timestamp_after(retry_after);
        transaction.execute(
            r#"
            UPDATE outbox_jobs
            SET
                state = ?2,
                next_attempt_at = ?3,
                last_error_message = ?4,
                updated_at = ?5
            WHERE id = ?1
            "#,
            params![
                job_id,
                OutboxJobState::Retrying.as_str(),
                next_attempt_at,
                error.as_ref(),
                now,
            ],
        )?;
        transaction.execute(
            r#"
            UPDATE drafts
            SET
                state = ?2,
                last_error_message = ?3,
                updated_at = ?4
            WHERE id = ?1
            "#,
            params![draft_id, DraftState::Queued.as_str(), error.as_ref(), now],
        )?;
        transaction
            .commit()
            .context("failed to commit the outbox retry transaction")
    }

    /// Records a terminal delivery failure.
    pub fn record_outbox_failure(&self, job_id: i64, error: impl AsRef<str>) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the outbox failure transaction")?;
        let draft_id = transaction
            .query_row(
                "SELECT draft_id FROM outbox_jobs WHERE id = ?1",
                params![job_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .with_context(|| format!("outbox job {job_id} does not exist"))?;
        let now = utc_now_timestamp();
        transaction.execute(
            r#"
            UPDATE outbox_jobs
            SET
                state = ?2,
                next_attempt_at = NULL,
                last_error_message = ?3,
                updated_at = ?4
            WHERE id = ?1
            "#,
            params![job_id, OutboxJobState::Failed.as_str(), error.as_ref(), now],
        )?;
        transaction.execute(
            r#"
            UPDATE drafts
            SET
                state = ?2,
                last_error_message = ?3,
                updated_at = ?4
            WHERE id = ?1
            "#,
            params![draft_id, DraftState::Failed.as_str(), error.as_ref(), now],
        )?;
        transaction
            .commit()
            .context("failed to commit the outbox failure transaction")
    }

    /// Records a successful delivery attempt.
    pub fn record_outbox_sent(&self, job_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the outbox success transaction")?;
        let draft_id = transaction
            .query_row(
                "SELECT draft_id FROM outbox_jobs WHERE id = ?1",
                params![job_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .with_context(|| format!("outbox job {job_id} does not exist"))?;
        let now = utc_now_timestamp();
        transaction.execute(
            r#"
            UPDATE outbox_jobs
            SET
                state = ?2,
                next_attempt_at = NULL,
                last_error_message = NULL,
                updated_at = ?3
            WHERE id = ?1
            "#,
            params![job_id, OutboxJobState::Sent.as_str(), now],
        )?;
        transaction.execute(
            r#"
            UPDATE drafts
            SET
                state = ?2,
                sent_at = ?3,
                last_error_message = NULL,
                updated_at = ?4
            WHERE id = ?1
            "#,
            params![draft_id, DraftState::Sent.as_str(), now, now],
        )?;
        // The recipients join the address history now, before the sent
        // copy syncs back.
        let (account_id, recipients) = transaction.query_row(
            "SELECT account_id, to_json, cc_json, bcc_json FROM drafts WHERE id = ?1",
            params![draft_id],
            |row| {
                let mut recipients = Vec::new();
                for column in 1..=3 {
                    recipients.extend(
                        recipients_from_json(row.get::<_, String>(column)?)
                            .map_err(to_from_sql_error)?,
                    );
                }
                Ok((row.get::<_, i64>(0)?, recipients))
            },
        )?;
        address_history::record_outbox_recipients(&transaction, account_id, &recipients, &now)?;
        transaction
            .commit()
            .context("failed to commit the outbox success transaction")
    }

    /// Manually requeues a failed outbox job for another delivery attempt.
    pub fn retry_outbox_job(&self, job_id: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin the outbox requeue transaction")?;
        let (draft_id, state) = transaction
            .query_row(
                "SELECT draft_id, state FROM outbox_jobs WHERE id = ?1",
                params![job_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        outbox_job_state_from_db(row.get::<_, String>(1)?)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("outbox job {job_id} does not exist"))?;
        if matches!(state, OutboxJobState::Sending | OutboxJobState::Sent) {
            bail!("outbox job {job_id} cannot be requeued from state {state}");
        }

        let now = utc_now_timestamp();
        transaction.execute(
            r#"
            UPDATE outbox_jobs
            SET
                state = ?2,
                next_attempt_at = ?3,
                last_error_message = NULL,
                updated_at = ?4
            WHERE id = ?1
            "#,
            params![job_id, OutboxJobState::Queued.as_str(), now, now],
        )?;
        transaction.execute(
            r#"
            UPDATE drafts
            SET
                state = ?2,
                last_error_message = NULL,
                updated_at = ?3
            WHERE id = ?1
            "#,
            params![draft_id, DraftState::Queued.as_str(), now],
        )?;
        transaction
            .commit()
            .context("failed to commit the outbox requeue transaction")
    }

    fn apply_migrations(&self) -> Result<()> {
        let mut conn = self.connection()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);",
        )
        .context("failed to initialize the migration tracking table")?;

        for (version, sql) in MIGRATIONS {
            let already_applied = conn
                .query_row(
                    "SELECT 1 FROM schema_migrations WHERE version = ?1 LIMIT 1",
                    params![version],
                    |_| Ok(()),
                )
                .optional()
                .context("failed to inspect the migration table")?
                .is_some();

            if already_applied {
                continue;
            }

            let transaction = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("failed to begin the migration transaction")?;
            transaction
                .execute_batch(sql)
                .with_context(|| format!("failed to apply migration {version}"))?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version) VALUES (?1)",
                    params![version],
                )
                .with_context(|| format!("failed to record migration {version}"))?;
            transaction
                .commit()
                .with_context(|| format!("failed to commit migration {version}"))?;
        }

        // Index whatever a migration (or an interrupted earlier flush) left
        // dirty; a no-op on an up-to-date store.
        let transaction = conn
            .transaction()
            .context("failed to begin the search index transaction")?;
        flush_search_index(&transaction)?;
        transaction
            .commit()
            .context("failed to commit the search index")
    }

    fn record_sync_error(
        &self,
        account_id: i64,
        phase: SyncPhase,
        detail: impl AsRef<str>,
        error: impl AsRef<str>,
    ) -> Result<()> {
        let conn = self.connection()?;
        let now = utc_now_timestamp();
        conn.execute(
            r#"
            UPDATE sync_state
            SET
                phase = ?2,
                detail = ?3,
                last_error_at = ?4,
                last_error_message = ?5,
                failure_count = failure_count + 1
            WHERE account_id = ?1
            "#,
            params![
                account_id,
                phase.as_str(),
                detail.as_ref(),
                now,
                error.as_ref()
            ],
        )
        .with_context(|| format!("failed to record sync error for account {account_id}"))?;
        Ok(())
    }

    fn demo_account_ordinal(&self, protocol: ProtocolKind) -> Result<usize> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT COUNT(*) FROM accounts WHERE protocol_kind = ?1",
            params![protocol.as_str()],
            |row| row.get::<_, usize>(0),
        )
        .map(|count| count + 1)
        .map_err(Into::into)
    }

    /// Opens a connection to the store.
    ///
    /// Every write transaction begins `IMMEDIATE`, taking the write lock up
    /// front. The UI and the sync runtime write from separate connections;
    /// a deferred transaction that reads before writing fails at once with
    /// "database is locked" if the other side commits in between, because
    /// the busy timeout does not cover that upgrade.
    fn connection(&self) -> Result<Connection> {
        let conn = Connection::open(&*self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        configure_connection(&conn)?;
        Ok(conn)
    }

    fn thread_location(&self, thread_id: i64) -> Result<Option<ThreadLocation>> {
        let conn = self.connection()?;
        thread_location(&conn, thread_id)
    }

    fn attachment_storage_dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(|parent| parent.join("attachments"))
            .unwrap_or_else(|| PathBuf::from("attachments"))
    }

    fn pending_outbox_job(&self, job_id: i64) -> Result<Option<PendingOutboxJob>> {
        let conn = self.connection()?;
        let row = conn
            .query_row(
                r#"
                SELECT
                    outbox_jobs.id,
                    outbox_jobs.attempt_count,
                    drafts.id,
                    drafts.identity_id,
                    drafts.identity_name,
                    drafts.identity_email,
                    drafts.subject,
                    drafts.body,
                    drafts.to_json,
                    drafts.cc_json,
                    drafts.bcc_json,
                    accounts.id,
                    accounts.protocol_kind,
                    accounts.endpoint,
                    accounts.username,
                    accounts.submission_kind,
                    accounts.submission_host,
                    accounts.submission_port,
                    accounts.submission_security,
                    accounts.submission_username,
                    accounts.submission_hello_name,
                    accounts.auth_kind
                FROM outbox_jobs
                JOIN drafts ON drafts.id = outbox_jobs.draft_id
                JOIN accounts ON accounts.id = outbox_jobs.account_id
                WHERE outbox_jobs.id = ?1
                "#,
                params![job_id],
                |row| {
                    let account_id = row.get::<_, i64>(11)?;
                    let protocol = protocol_from_db(row.get::<_, String>(12)?)?;
                    let endpoint = row.get::<_, String>(13)?;
                    let username = row.get::<_, String>(14)?;
                    let outgoing = outgoing_from_row(
                        protocol,
                        &endpoint,
                        &username,
                        OutgoingRow {
                            kind: row.get::<_, Option<String>>(15)?,
                            host: row.get::<_, Option<String>>(16)?,
                            port: row.get::<_, Option<i64>>(17)?,
                            security: row.get::<_, Option<String>>(18)?,
                            submission_username: row.get::<_, Option<String>>(19)?,
                            hello_name: row.get::<_, Option<String>>(20)?,
                        },
                    )
                    .map_err(to_from_sql_error)?;
                    let auth = auth_kind_from_db(row.get::<_, String>(21)?)?;
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, usize>(1)?,
                        DeliveryTarget {
                            account_id,
                            draft_id: row.get(2)?,
                            identity_id: row.get(3)?,
                            protocol,
                            auth,
                            endpoint,
                            username,
                            identity_name: row.get(4)?,
                            identity_email: row.get(5)?,
                            outgoing,
                        },
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        recipients_from_json(row.get::<_, String>(8)?)
                            .map_err(to_from_sql_error)?,
                        recipients_from_json(row.get::<_, String>(9)?)
                            .map_err(to_from_sql_error)?,
                        recipients_from_json(row.get::<_, String>(10)?)
                            .map_err(to_from_sql_error)?,
                    ))
                },
            )
            .optional()
            .context("failed to load the pending outbox job")?;
        let Some((job_id, attempt_count, target, subject, body, to, cc, bcc)) = row else {
            return Ok(None);
        };

        let attachments = load_draft_attachments(&conn, target.draft_id)?
            .into_iter()
            .map(|attachment| {
                let bytes = fs::read(&attachment.storage_path).with_context(|| {
                    format!("failed to read attachment {}", attachment.storage_path)
                })?;
                Ok(OutgoingAttachment {
                    filename: attachment.filename,
                    content_type: attachment.content_type,
                    bytes,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Some(PendingOutboxJob {
            job_id,
            attempt_count,
            target: target.clone(),
            message: OutgoingMessage {
                from_email: target.identity_email.clone(),
                from_name: Some(target.identity_name.clone()),
                to,
                cc,
                bcc,
                subject,
                text_body: body,
                attachments,
            },
        }))
    }
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("failed to configure SQLite busy timeout")?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        "#,
    )
    .context("failed to configure SQLite pragmas")
}

fn query_count(conn: &Connection, sql: &str) -> Result<usize> {
    conn.query_row(sql, [], |row| row.get::<_, usize>(0))
        .map_err(Into::into)
}

fn load_identities(conn: &Connection, account_id: i64) -> Result<Vec<IdentitySummary>> {
    let mut statement = conn.prepare(
        r#"
        SELECT id, display_name, email_address, signature, is_primary
        FROM identities
        WHERE account_id = ?1
        ORDER BY is_primary DESC, id ASC
        "#,
    )?;
    let rows = statement.query_map(params![account_id], |row| {
        Ok(IdentitySummary {
            id: row.get(0)?,
            display_name: row.get(1)?,
            email_address: row.get(2)?,
            signature: row.get(3)?,
            is_primary: row.get::<_, bool>(4)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list identities")
}

fn load_identity_snapshot(
    conn: &Connection,
    account_id: i64,
    identity_id: i64,
) -> Result<Option<IdentitySummary>> {
    conn.query_row(
        r#"
        SELECT id, display_name, email_address, signature, is_primary
        FROM identities
        WHERE account_id = ?1 AND id = ?2
        "#,
        params![account_id, identity_id],
        |row| {
            Ok(IdentitySummary {
                id: row.get(0)?,
                display_name: row.get(1)?,
                email_address: row.get(2)?,
                signature: row.get(3)?,
                is_primary: row.get::<_, bool>(4)?,
            })
        },
    )
    .optional()
    .context("failed to load the selected identity")
}

fn load_draft_attachments(conn: &Connection, draft_id: i64) -> Result<Vec<DraftAttachment>> {
    let mut statement = conn.prepare(
        r#"
        SELECT id, filename, content_type, size_bytes, original_path, storage_path
        FROM draft_attachments
        WHERE draft_id = ?1
        ORDER BY id ASC
        "#,
    )?;
    let rows = statement.query_map(params![draft_id], |row| {
        Ok(DraftAttachment {
            id: row.get(0)?,
            filename: row.get(1)?,
            content_type: row.get(2)?,
            size_bytes: row.get::<_, i64>(3)? as u64,
            original_path: row.get(4)?,
            storage_path: row.get(5)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load draft attachments")
}

fn ensure_draft_is_editable(state: DraftState) -> Result<()> {
    if matches!(
        state,
        DraftState::Queued | DraftState::Sending | DraftState::Sent
    ) {
        bail!("draft cannot be modified while it is {state}");
    }
    Ok(())
}

fn recipients_to_json(recipients: &[String]) -> Result<String> {
    serde_json::to_string(recipients).context("failed to encode recipients")
}

fn recipients_from_json(json: String) -> Result<Vec<String>> {
    serde_json::from_str(&json).context("failed to decode recipients")
}

fn summarize_recipients(to: &[String], cc: &[String], bcc: &[String]) -> String {
    let mut recipients = to.iter().chain(cc.iter()).chain(bcc.iter());
    match recipients.next() {
        Some(first) if recipients.next().is_some() => {
            format!("{first} +{}", to.len() + cc.len() + bcc.len() - 1)
        }
        Some(first) => first.clone(),
        None => "(no recipients)".to_owned(),
    }
}

fn apply_default_signature(body: &str, signature: &str) -> String {
    if !body.trim().is_empty() {
        return body.to_owned();
    }
    signature.trim().to_owned()
}

/// Remote ids of a thread's messages whose sender warning was dismissed.
fn dismissed_sender_warnings(conn: &Connection, thread_id: i64) -> Result<HashSet<String>> {
    let mut statement = conn
        .prepare(
            "SELECT remote_id FROM messages \
             WHERE thread_id = ?1 AND sender_warning_dismissed = 1 AND remote_id <> ''",
        )
        .context("failed to prepare the dismissed sender warnings query")?;
    let remote_ids = statement
        .query_map(params![thread_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()
        .context("failed to load the dismissed sender warnings")?;
    Ok(remote_ids)
}

fn sanitize_filename(filename: &str) -> String {
    filename
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect()
}

fn guess_content_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("html") | Some("htm") => "text/html",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
    .to_owned()
}

fn draft_state_from_db(value: String) -> rusqlite::Result<DraftState> {
    DraftState::parse(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown draft state {value}"),
            )),
        )
    })
}

fn outbox_job_state_from_db(value: String) -> rusqlite::Result<OutboxJobState> {
    OutboxJobState::parse(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown outbox job state {value}"),
            )),
        )
    })
}

struct OutgoingRow {
    kind: Option<String>,
    host: Option<String>,
    port: Option<i64>,
    security: Option<String>,
    submission_username: Option<String>,
    hello_name: Option<String>,
}

fn outgoing_from_row(
    protocol: ProtocolKind,
    endpoint: &str,
    username: &str,
    row: OutgoingRow,
) -> Result<OutgoingConfig> {
    let kind = row
        .kind
        .as_deref()
        .and_then(SubmissionKind::parse)
        .unwrap_or(match protocol {
            ProtocolKind::Imap => SubmissionKind::Smtp,
            ProtocolKind::Jmap => SubmissionKind::Jmap,
        });
    let derived = default_outgoing(protocol, endpoint, username);
    let OutgoingConfig {
        host: derived_host,
        port: derived_port,
        username: derived_username,
        security: derived_security,
        hello_name: derived_hello_name,
        ..
    } = derived;
    Ok(OutgoingConfig {
        kind,
        host: row.host.or(derived_host),
        port: row
            .port
            .map(|value| u16::try_from(value).context("invalid submission port"))
            .transpose()?
            .or(derived_port),
        username: row.submission_username.or(derived_username),
        security: row
            .security
            .as_deref()
            .and_then(SubmissionSecurity::parse)
            .or(derived_security),
        hello_name: row.hello_name.unwrap_or(derived_hello_name),
    })
}

fn default_outgoing(protocol: ProtocolKind, endpoint: &str, username: &str) -> OutgoingConfig {
    match protocol {
        ProtocolKind::Jmap => OutgoingConfig::jmap_default(),
        ProtocolKind::Imap => {
            let host = extract_host(endpoint).unwrap_or_else(|| "localhost".to_owned());
            let mut outgoing = OutgoingConfig::smtp_default(host);
            outgoing.username = Some(username.to_owned());
            outgoing
        }
    }
}

fn extract_host(endpoint: &str) -> Option<String> {
    if let Some(without_scheme) = endpoint.split("://").nth(1) {
        return extract_host(without_scheme);
    }

    endpoint
        .split('/')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with('[') {
                value
                    .split(']')
                    .next()
                    .map(|value| format!("{value}]"))
                    .unwrap_or_else(|| value.to_owned())
            } else {
                value.split(':').next().unwrap_or(value).to_owned()
            }
        })
}

/// The first whole second at or after `duration` from now, so a job
/// scheduled with it is never due early.
fn utc_timestamp_at_least(duration: Duration) -> String {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .saturating_add(duration);
    let seconds = at.as_secs() + u64::from(at.subsec_nanos() > 0);
    utc_timestamp_from_epoch(seconds)
}

fn utc_timestamp_after(duration: Duration) -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_add(duration.as_secs());
    utc_timestamp_from_epoch(seconds)
}

fn utc_timestamp_from_epoch(seconds: u64) -> String {
    zmail_core::time::utc_timestamp(seconds)
}

fn to_from_sql_error(error: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    )
}

#[derive(Debug, Clone, Copy)]
struct ThreadLocation {
    account_id: i64,
    mailbox_id: i64,
    message_count: usize,
    unread_count: usize,
}

#[derive(Debug, Clone)]
struct MailboxIdentity {
    id: i64,
    account_id: i64,
    role: Option<String>,
}

fn thread_location(conn: &Connection, thread_id: i64) -> Result<Option<ThreadLocation>> {
    conn.query_row(
        r#"
        SELECT
            mailboxes.account_id,
            threads.mailbox_id,
            threads.message_count,
            threads.unread_count
        FROM threads
        JOIN mailboxes ON mailboxes.id = threads.mailbox_id
        WHERE threads.id = ?1
        "#,
        params![thread_id],
        |row| {
            Ok(ThreadLocation {
                account_id: row.get(0)?,
                mailbox_id: row.get(1)?,
                message_count: row.get::<_, usize>(2)?,
                unread_count: row.get::<_, usize>(3)?,
            })
        },
    )
    .optional()
    .context("failed to load thread location")
}

/// Re-indexes the threads whose indexed text changed since the last flush
/// (see [`SEARCH_INDEX_V2_SCHEMA`]).
///
/// Callers run this inside their write transaction, before committing, so
/// a committed change is always searchable and a concurrent writer cannot
/// slip dirty rows in between the rebuild and the clear.
/// Runs a compiled search and maps its rows to hits.
fn run_search(
    conn: &Connection,
    compiled: search::CompiledSearch,
    from_server: bool,
) -> Result<Vec<SearchHit>> {
    let mut statement = conn
        .prepare(&compiled.sql)
        .context("failed to prepare the search query")?;
    let rows = statement.query_map(rusqlite::params_from_iter(compiled.params), |row| {
        Ok(SearchHit {
            mailbox_id: row.get(0)?,
            mailbox_name: row.get(1)?,
            thread_id: row.get(2)?,
            subject: row.get(3)?,
            latest_sender: row.get(4)?,
            participants: row.get(5)?,
            preview: row.get(6)?,
            unread_count: row.get::<_, usize>(7)?,
            is_flagged: row.get::<_, bool>(8)?,
            has_attachments: row.get::<_, bool>(9)?,
            latest_message_at: row.get(10)?,
            account_id: row.get(11)?,
            from_server,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to search threads")
}

fn flush_search_index(conn: &Connection) -> Result<()> {
    conn.execute_batch(SEARCH_INDEX_FLUSH)
        .context("failed to update the search index")
}

/// Measures the account's blob usage from stored rows: the character
/// lengths of hydrated bodies plus the recorded attachment blob sizes.
fn account_blob_usage(conn: &Connection, account_id: i64) -> Result<u64> {
    let bodies = conn
        .query_row(
            r#"
            SELECT COALESCE(SUM(LENGTH(COALESCE(m.body_text, '')) +
                                LENGTH(COALESCE(m.sanitized_html, ''))), 0)
            FROM messages m
            JOIN threads t ON t.id = m.thread_id
            JOIN mailboxes b ON b.id = t.mailbox_id
            WHERE b.account_id = ?1
            "#,
            params![account_id],
            |row| row.get::<_, i64>(0),
        )
        .context("failed to measure the hydrated body usage")?;
    let attachments = conn
        .query_row(
            r#"
            SELECT COALESCE(SUM(a.size_bytes), 0)
            FROM message_attachments a
            JOIN messages m ON m.id = a.message_id
            JOIN threads t ON t.id = m.thread_id
            JOIN mailboxes b ON b.id = t.mailbox_id
            WHERE b.account_id = ?1
            "#,
            params![account_id],
            |row| row.get::<_, i64>(0),
        )
        .context("failed to measure the attachment blob usage")?;
    Ok((bodies + attachments).max(0) as u64)
}

fn mailbox_identity(conn: &Connection, mailbox_id: i64) -> Result<Option<MailboxIdentity>> {
    conn.query_row(
        "SELECT id, account_id, role FROM mailboxes WHERE id = ?1",
        params![mailbox_id],
        |row| {
            Ok(MailboxIdentity {
                id: row.get(0)?,
                account_id: row.get(1)?,
                role: row.get(2)?,
            })
        },
    )
    .optional()
    .context("failed to load mailbox metadata")
}

fn mailbox_id_by_role(conn: &Connection, account_id: i64, role: &str) -> Result<Option<i64>> {
    conn.query_row(
        r#"
        SELECT id
        FROM mailboxes
        WHERE account_id = ?1 AND lower(role) = lower(?2)
        ORDER BY sort_order ASC, id ASC
        LIMIT 1
        "#,
        params![account_id, role],
        |row| row.get(0),
    )
    .optional()
    .context("failed to resolve a mailbox by role")
}

fn update_mailbox_unread_threads(
    conn: &Connection,
    mailbox_id: i64,
    previously_unread: bool,
    now_unread: bool,
) -> Result<()> {
    let delta = match (previously_unread, now_unread) {
        (false, true) => 1,
        (true, false) => -1,
        _ => 0,
    };
    if delta == 0 {
        return Ok(());
    }

    conn.execute(
        r#"
        UPDATE mailboxes
        SET unread_threads = MAX(unread_threads + ?2, 0)
        WHERE id = ?1
        "#,
        params![mailbox_id, delta],
    )?;
    Ok(())
}

fn move_thread_in_transaction(
    transaction: &Transaction<'_>,
    thread_id: i64,
    location: &ThreadLocation,
    destination_mailbox_id: i64,
    _destination_role: Option<&str>,
) -> Result<()> {
    transaction.execute(
        "UPDATE threads SET mailbox_id = ?2 WHERE id = ?1",
        params![thread_id, destination_mailbox_id],
    )?;
    transaction.execute(
        r#"
        UPDATE mailboxes
        SET
            total_threads = MAX(total_threads - 1, 0),
            unread_threads = MAX(unread_threads - ?2, 0)
        WHERE id = ?1
        "#,
        params![location.mailbox_id, i64::from(location.unread_count > 0)],
    )?;
    transaction.execute(
        r#"
        UPDATE mailboxes
        SET
            total_threads = total_threads + 1,
            unread_threads = unread_threads + ?2
        WHERE id = ?1
        "#,
        params![destination_mailbox_id, i64::from(location.unread_count > 0)],
    )?;
    Ok(())
}

fn renderable_body(
    preview: &str,
    body_text: Option<&str>,
    sanitized_html: Option<&str>,
) -> (String, MessageRenderMode) {
    if let Some(body_text) = body_text.filter(|value| !value.trim().is_empty()) {
        return (body_text.to_owned(), MessageRenderMode::PlainText);
    }

    if let Some(sanitized_html) = sanitized_html.filter(|value| !value.trim().is_empty()) {
        let sanitized = sanitize_html_for_display(sanitized_html);
        return (sanitized.text, MessageRenderMode::SanitizedHtml);
    }

    (preview.to_owned(), MessageRenderMode::PreviewOnly)
}

fn identity_id_map(
    transaction: &Transaction<'_>,
    account_id: i64,
) -> Result<std::collections::HashMap<String, i64>> {
    let mut statement = transaction.prepare(
        "SELECT email_address, id FROM identities WHERE account_id = ?1 ORDER BY is_primary DESC, id ASC",
    )?;
    let rows = statement.query_map(params![account_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let pairs = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to build the identity lookup")?;
    Ok(pairs.into_iter().collect())
}

fn replace_account_identities(
    transaction: &Transaction<'_>,
    account_id: i64,
    identities: &[zmail_core::NewIdentity],
) -> Result<()> {
    if identities.is_empty() {
        bail!("cannot replace identities with an empty set");
    }

    // Identities are merged by email address so their row ids stay stable
    // across syncs; messages reference identity ids and a delete-and-
    // reinsert would break those references.
    let existing = identity_id_map(transaction, account_id)?;
    for identity in identities {
        match existing.get(&identity.email_address) {
            Some(identity_id) => {
                transaction
                    .execute(
                        r#"
                    UPDATE identities
                    SET display_name = ?2, signature = ?3, is_primary = ?4
                    WHERE id = ?1
                    "#,
                        params![
                            identity_id,
                            identity.display_name,
                            identity.signature,
                            identity.is_primary,
                        ],
                    )
                    .context("failed to update the identity during the snapshot merge")?;
            }
            None => {
                transaction.execute(
                    r#"
                    INSERT INTO identities(account_id, display_name, email_address, signature, is_primary)
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    "#,
                    params![
                        account_id,
                        identity.display_name,
                        identity.email_address,
                        identity.signature,
                        identity.is_primary,
                    ],
                )
                .context("failed to insert the identity during the snapshot merge")?;
            }
        }
    }

    // Identities missing from the snapshot are dropped only when no message
    // still references them, so historical rows keep a valid identity.
    let incoming: HashSet<&str> = identities
        .iter()
        .map(|identity| identity.email_address.as_str())
        .collect();
    for (email_address, identity_id) in &existing {
        if incoming.contains(email_address.as_str()) {
            continue;
        }
        let referenced = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE identity_id = ?1)",
                params![identity_id],
                |row| row.get::<_, i64>(0),
            )
            .context("failed to check identity references during the snapshot merge")?;
        if referenced == 0 {
            transaction
                .execute("DELETE FROM identities WHERE id = ?1", params![identity_id])
                .with_context(|| {
                    format!("failed to remove the {email_address} identity from the snapshot merge")
                })?;
        }
    }
    Ok(())
}

/// The `WHERE` clause over `threads` that picks a selection's threads, and
/// the values it binds as `?1`, `?2`, ….
/// The extra `WHERE` condition for a [`ThreadFilter`], on the `threads`
/// table, appended to a selection's filter.
fn thread_filter_clause(filter: ThreadFilter) -> &'static str {
    match filter {
        ThreadFilter::All => "",
        ThreadFilter::Unread => " AND threads.unread_count > 0",
        ThreadFilter::Flagged => " AND threads.is_flagged = 1",
        ThreadFilter::HasAttachments => " AND threads.has_attachments = 1",
    }
}

fn selection_filter(selection: &MailboxSelection) -> (String, Vec<rusqlite::types::Value>) {
    match selection {
        MailboxSelection::Mailbox { mailbox_id, .. } => (
            "threads.mailbox_id = ?".to_owned(),
            vec![(*mailbox_id).into()],
        ),
        // Each account's first mailbox with the role, as
        // `mailbox_id_by_role` resolves it.
        MailboxSelection::Virtual(VirtualMailbox::Unified(role)) => (
            r#"threads.mailbox_id IN (
                SELECT (
                    SELECT role_mailbox.id FROM mailboxes AS role_mailbox
                    WHERE role_mailbox.account_id = accounts.id
                      AND lower(role_mailbox.role) = ?
                    ORDER BY role_mailbox.sort_order, role_mailbox.id
                    LIMIT 1
                )
                FROM accounts
            )"#
            .to_owned(),
            vec![role.as_str().to_owned().into()],
        ),
        // One copy of each flagged conversation per account: a
        // conversation filed in several mailboxes (Gmail's labels, or All
        // Mail) is listed once, preferring its Inbox copy.
        MailboxSelection::Virtual(VirtualMailbox::Flagged) => (
            r#"threads.id IN (
                SELECT thread_id FROM (
                    SELECT
                        flagged.id AS thread_id,
                        ROW_NUMBER() OVER (
                            PARTITION BY
                                flagged_mailbox.account_id,
                                COALESCE(NULLIF(flagged.remote_thread_id, ''), 'local:' || flagged.id)
                            ORDER BY
                                CASE lower(flagged_mailbox.role) WHEN 'inbox' THEN 0 ELSE 1 END,
                                flagged.id
                        ) AS copy
                    FROM threads AS flagged
                    JOIN mailboxes AS flagged_mailbox ON flagged_mailbox.id = flagged.mailbox_id
                    WHERE flagged.is_flagged = 1
                      AND COALESCE(lower(flagged_mailbox.role), '') NOT IN ('trash', 'junk')
                )
                WHERE copy = 1
            )"#
            .to_owned(),
            Vec::new(),
        ),
    }
}

/// The `mailboxes` columns [`mailbox_summary_from_row`] reads, in order.
const MAILBOX_SUMMARY_COLUMNS: &str =
    "id, account_id, name, role, delimiter, parent_name, total_threads, unread_threads";

/// Reads a [`MailboxSummary`] from a row of [`MAILBOX_SUMMARY_COLUMNS`].
fn mailbox_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailboxSummary> {
    Ok(MailboxSummary {
        id: row.get(0)?,
        account_id: row.get(1)?,
        name: row.get(2)?,
        role: row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .and_then(MailboxRole::parse),
        delimiter: row.get(4)?,
        parent: row.get(5)?,
        total_threads: row.get::<_, usize>(6)?,
        unread_threads: row.get::<_, usize>(7)?,
    })
}

/// An existing mailbox row, as the snapshot merge compares it.
struct ExistingMailbox {
    id: i64,
    role: Option<String>,
    sort_order: i64,
    delimiter: Option<String>,
    parent: Option<String>,
}

/// Loads the mailbox rows for an account, keyed by mailbox name.
fn load_account_mailbox_ids(
    transaction: &Transaction<'_>,
    account_id: i64,
) -> Result<std::collections::HashMap<String, ExistingMailbox>> {
    let mut statement = transaction.prepare(
        "SELECT id, name, role, sort_order, delimiter, parent_name \
         FROM mailboxes WHERE account_id = ?1",
    )?;
    let rows = statement.query_map(params![account_id], |row| {
        Ok((
            row.get::<_, String>(1)?,
            ExistingMailbox {
                id: row.get(0)?,
                role: row.get(2)?,
                sort_order: row.get(3)?,
                delimiter: row.get(4)?,
                parent: row.get(5)?,
            },
        ))
    })?;
    let pairs = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load the existing mailboxes during the snapshot merge")?;
    Ok(pairs.into_iter().collect())
}

/// Merges the snapshot threads for one mailbox and returns the ids kept.
///
/// Locally changed read and flag state is preserved only for messages with
/// a queued write-back operation; everywhere else the server snapshot is
/// authoritative, so changes made by other clients become visible.
fn merge_mailbox_threads(
    transaction: &Transaction<'_>,
    mailbox_id: i64,
    mailbox: &MailboxSnapshot,
    identity_map: &std::collections::HashMap<String, i64>,
    fallback_identity_id: i64,
    pending_remote_ids: &HashSet<String>,
) -> Result<(HashSet<i64>, bool)> {
    let mut existing_threads: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    {
        let mut statement = transaction
            .prepare("SELECT id, remote_thread_id FROM threads WHERE mailbox_id = ?1")?;
        let rows = statement.query_map(params![mailbox_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (thread_id, remote_thread_id) =
                row.context("failed to load the existing threads during the snapshot merge")?;
            if let Some(remote_thread_id) = remote_thread_id
                && !remote_thread_id.is_empty()
            {
                existing_threads.insert(remote_thread_id, thread_id);
            }
        }
    }

    let mut kept_thread_ids: HashSet<i64> = HashSet::new();
    let mut changed = false;
    for thread in &mailbox.threads {
        if thread.messages.is_empty() {
            continue;
        }

        let existing_thread_id = existing_threads.get(&thread.remote_thread_id).copied();
        let (thread_flagged, thread_had_attachments, message_flags) = match existing_thread_id {
            Some(existing_id) => {
                let stored_thread = load_stored_thread(transaction, existing_id)?
                    .context("the stored thread vanished during the snapshot merge")?;
                let had_attachments = stored_thread.has_attachments;
                (
                    load_thread_flag(transaction, existing_id)?,
                    had_attachments,
                    load_message_flags(transaction, existing_id)?,
                )
            }
            None => (thread.is_flagged, false, std::collections::HashMap::new()),
        };
        let thread_has_attachments = thread.has_attachments || thread_had_attachments;

        // Locally changed state survives only while its write-back
        // operation is still queued; the server is authoritative elsewhere
        // so changes from other clients become visible.
        let mut thread_has_pending_op = false;
        let mut unread_count = 0usize;
        let mut merged_messages = Vec::with_capacity(thread.messages.len());
        for message in &thread.messages {
            let identity_id = identity_map
                .get(message.identity_email.as_str())
                .copied()
                .unwrap_or(fallback_identity_id);
            let has_pending_op =
                !message.remote_id.is_empty() && pending_remote_ids.contains(&message.remote_id);
            thread_has_pending_op |= has_pending_op;
            let (is_read, is_flagged) = if has_pending_op {
                message_flags
                    .get(&message.remote_id)
                    .map(|(is_read, is_flagged)| (*is_read, *is_flagged))
                    .unwrap_or((message.is_read, message.is_flagged))
            } else {
                (message.is_read, message.is_flagged)
            };
            unread_count += usize::from(!is_read);
            merged_messages.push((message, is_read, is_flagged, identity_id));
        }

        // The thread flag also follows the server unless a queued
        // operation still protects the local change.
        let thread_flagged = if thread_has_pending_op {
            thread_flagged
        } else {
            thread.is_flagged
        };

        // Steady-state syncs re-apply identical snapshots; comparing the
        // merged values against the stored rows skips the rewrite and
        // keeps the search index untouched for unchanged threads.
        if let Some(existing_id) = existing_thread_id
            && stored_thread_matches(
                transaction,
                existing_id,
                thread,
                thread_flagged,
                thread_has_attachments,
                unread_count,
                &merged_messages,
            )?
        {
            kept_thread_ids.insert(existing_id);
            continue;
        }
        changed = true;

        // Lazy adapters sync headers without bodies, and hydration is local
        // state the server snapshot cannot re-supply. When a changed thread is
        // rewritten, the stored bodies, previews, and attachment metadata are
        // re-attached first so the rewrite cannot drop them.
        let write_messages: Vec<MessageSnapshot> = match existing_thread_id {
            Some(existing_id)
                if thread.messages.iter().any(|message| {
                    message.body_text.is_none() && message.sanitized_html.is_none()
                }) || (thread_had_attachments
                    && thread
                        .messages
                        .iter()
                        .any(|message| message.attachments.is_empty())) =>
            {
                materialize_stored_state(transaction, existing_id, thread, thread_had_attachments)?
            }
            Some(_) => thread.messages.clone(),
            None => thread.messages.clone(),
        };
        let merged_messages: Vec<(MessageSnapshot, bool, bool, i64)> = write_messages
            .into_iter()
            .zip(merged_messages)
            .map(|(message, (_, is_read, is_flagged, identity_id))| {
                (message, is_read, is_flagged, identity_id)
            })
            .collect();

        // "Looks safe" on a sender warning is local state the server
        // cannot re-supply; carry it over by remote id.
        let dismissed_warnings = match existing_thread_id {
            Some(existing_id) => dismissed_sender_warnings(transaction, existing_id)?,
            None => HashSet::new(),
        };

        let thread_row_id = match existing_thread_id {
            Some(existing_id) => {
                transaction
                    .execute(
                        r#"
                    UPDATE threads
                    SET
                        subject = ?2,
                        participants = ?3,
                        preview = ?4,
                        is_flagged = ?5,
                        has_attachments = ?6,
                        message_count = ?7,
                        unread_count = ?8,
                        latest_message_at = ?9,
                        latest_message_id = 0
                    WHERE id = ?1
                    "#,
                        params![
                            existing_id,
                            thread.subject,
                            thread.participants,
                            thread.preview,
                            thread_flagged,
                            thread_has_attachments,
                            thread.messages.len() as i64,
                            unread_count as i64,
                            thread.latest_message_at,
                        ],
                    )
                    .context("failed to update the thread during the snapshot merge")?;
                transaction
                    .execute(
                        "DELETE FROM messages WHERE thread_id = ?1",
                        params![existing_id],
                    )
                    .context("failed to replace the thread messages during the snapshot merge")?;
                existing_id
            }
            None => {
                transaction
                    .execute(
                        r#"
                    INSERT INTO threads(
                        mailbox_id,
                        remote_thread_id,
                        subject,
                        participants,
                        preview,
                        is_flagged,
                        has_attachments,
                        message_count,
                        unread_count,
                        latest_message_at,
                        latest_message_id
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0)
                    "#,
                        params![
                            mailbox_id,
                            thread.remote_thread_id,
                            thread.subject,
                            thread.participants,
                            thread.preview,
                            thread_flagged,
                            thread_has_attachments,
                            thread.messages.len() as i64,
                            unread_count as i64,
                            thread.latest_message_at,
                        ],
                    )
                    .context("failed to insert the thread during the snapshot merge")?;
                transaction.last_insert_rowid()
            }
        };

        let mut latest_message_id = None;
        for (message, is_read, is_flagged, identity_id) in merged_messages {
            transaction
                .execute(
                    r#"
                INSERT INTO messages(
                    thread_id,
                    remote_id,
                    identity_id,
                    sender,
                    reply_to,
                    recipients,
                    subject,
                    body_preview,
                    body_text,
                    sanitized_html,
                    has_remote_content,
                    is_read,
                    is_flagged,
                    received_at,
                    sender_warning_dismissed
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
                    params![
                        thread_row_id,
                        message.remote_id,
                        identity_id,
                        message.sender,
                        message.reply_to,
                        message.recipients,
                        message.subject,
                        message.body_preview,
                        message.body_text,
                        message.sanitized_html,
                        message.has_remote_content,
                        is_read,
                        is_flagged,
                        message.received_at,
                        !message.remote_id.is_empty()
                            && dismissed_warnings.contains(&message.remote_id),
                    ],
                )
                .context("failed to insert the message during the snapshot merge")?;
            let message_row_id = transaction.last_insert_rowid();
            for attachment in &message.attachments {
                transaction
                    .execute(
                        r#"
                    INSERT INTO message_attachments(
                        message_id,
                        filename,
                        content_type,
                        size_bytes,
                        storage_path
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    "#,
                        params![
                            message_row_id,
                            attachment.filename,
                            attachment.content_type,
                            attachment.size_bytes as i64,
                            attachment.storage_path,
                        ],
                    )
                    .context("failed to insert the message attachment during the snapshot merge")?;
            }
            latest_message_id = Some(message_row_id);
        }

        if let Some(latest_message_id) = latest_message_id {
            transaction
                .execute(
                    "UPDATE threads SET latest_message_id = ?2 WHERE id = ?1",
                    params![thread_row_id, latest_message_id],
                )
                .context("failed to record the latest message during the snapshot merge")?;
        }
        kept_thread_ids.insert(thread_row_id);
    }

    Ok((kept_thread_ids, changed))
}

/// Returns `true` when the stored thread already matches the merged
/// snapshot values exactly, so the merge can skip rewriting it.
fn stored_thread_matches(
    transaction: &Transaction<'_>,
    thread_id: i64,
    thread: &ThreadSnapshot,
    thread_flagged: bool,
    thread_has_attachments: bool,
    unread_count: usize,
    merged_messages: &[(&MessageSnapshot, bool, bool, i64)],
) -> Result<bool> {
    let stored_thread = match load_stored_thread(transaction, thread_id)? {
        Some(stored) => stored,
        None => return Ok(false),
    };

    if stored_thread.subject != thread.subject
        || stored_thread.participants != thread.participants
        || stored_thread.preview != thread.preview
        || stored_thread.is_flagged != thread_flagged
        || stored_thread.has_attachments != thread_has_attachments
        || stored_thread.message_count != thread.messages.len()
        || stored_thread.unread_count != unread_count
        || stored_thread.latest_message_at != thread.latest_message_at
    {
        return Ok(false);
    }

    let stored_messages = load_stored_messages(transaction, thread_id)?;
    if stored_messages.len() != merged_messages.len() {
        return Ok(false);
    }

    let stored_by_remote_id: std::collections::HashMap<&str, &StoredMessage> = stored_messages
        .iter()
        .map(|message| (message.remote_id.as_str(), message))
        .collect();
    if stored_by_remote_id.len() != stored_messages.len() {
        return Ok(false);
    }

    for (message, is_read, is_flagged, identity_id) in merged_messages {
        let Some(stored) = stored_by_remote_id.get(message.remote_id.as_str()) else {
            return Ok(false);
        };
        let stored_has_body = stored.body_text.is_some() || stored.sanitized_html.is_some();
        let incoming_has_body = message.body_text.is_some() || message.sanitized_html.is_some();
        if incoming_has_body {
            if stored.identity_id != *identity_id
                || stored.sender != message.sender
                || stored.reply_to != message.reply_to
                || stored.recipients != message.recipients
                || stored.subject != message.subject
                || stored.body_preview != message.body_preview
                || stored.body_text != message.body_text
                || stored.sanitized_html != message.sanitized_html
                || stored.has_remote_content != message.has_remote_content
                || stored.is_read != *is_read
                || stored.is_flagged != *is_flagged
                || stored.received_at != message.received_at
            {
                return Ok(false);
            }
        } else if stored.identity_id != *identity_id
            || stored.sender != message.sender
            || stored.reply_to != message.reply_to
            || stored.recipients != message.recipients
            || stored.subject != message.subject
            || stored.is_read != *is_read
            || stored.is_flagged != *is_flagged
            || stored.received_at != message.received_at
            || (!stored_has_body && stored.body_preview != message.body_preview)
        {
            // Lazy snapshot: the body, its preview, and the remote-content
            // flag are local hydration state the snapshot cannot re-supply.
            // A stored body defers entirely; body-less messages must still
            // agree on the preview.
            return Ok(false);
        }

        if message.attachments.is_empty() {
            // Lazy snapshots cannot re-supply attachment metadata; the
            // stored rows are authoritative.
            continue;
        }
        let stored_attachments = load_stored_message_attachments(transaction, stored.id)?;
        if stored_attachments.len() != message.attachments.len() {
            return Ok(false);
        }
        for (stored, incoming) in stored_attachments.iter().zip(&message.attachments) {
            if stored.filename != incoming.filename
                || stored.content_type != incoming.content_type
                || stored.size_bytes != incoming.size_bytes
                || stored.storage_path != incoming.storage_path
            {
                return Ok(false);
            }
        }
    }

    Ok(true)
}

/// Hydrated state re-attached to a lazy snapshot's messages during a
/// rewrite: body, remote-content flag, preview, and attachment metadata.
type StoredHydration = (
    Option<String>,
    Option<String>,
    bool,
    String,
    Vec<MessageAttachment>,
);

/// The stored form of a message attachment for merge comparisons.
struct StoredAttachment {
    filename: String,
    content_type: String,
    size_bytes: u64,
    storage_path: String,
}

/// Loads the attachments of one stored message for merge comparison.
fn load_stored_message_attachments(
    transaction: &Transaction<'_>,
    message_id: i64,
) -> Result<Vec<StoredAttachment>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT filename, content_type, size_bytes, storage_path
        FROM message_attachments
        WHERE message_id = ?1
        ORDER BY id ASC
        "#,
    )?;
    let attachments = statement
        .query_map(params![message_id], |row| {
            Ok(StoredAttachment {
                filename: row.get(0)?,
                content_type: row.get(1)?,
                size_bytes: row.get::<_, i64>(2)? as u64,
                storage_path: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load the stored message attachments")?;
    Ok(attachments)
}

/// The stored form of a thread row for merge comparisons.
struct StoredThread {
    subject: String,
    participants: String,
    preview: String,
    is_flagged: bool,
    has_attachments: bool,
    message_count: usize,
    unread_count: usize,
    latest_message_at: String,
}

/// The stored form of a message row for merge comparisons.
struct StoredMessage {
    id: i64,
    remote_id: String,
    identity_id: i64,
    sender: String,
    reply_to: Option<String>,
    recipients: String,
    subject: String,
    body_preview: String,
    body_text: Option<String>,
    sanitized_html: Option<String>,
    has_remote_content: bool,
    is_read: bool,
    is_flagged: bool,
    received_at: String,
}

/// Loads a thread row for merge comparison.
fn load_stored_thread(
    transaction: &Transaction<'_>,
    thread_id: i64,
) -> Result<Option<StoredThread>> {
    transaction
        .query_row(
            r#"
            SELECT subject, participants, preview, is_flagged, has_attachments,
                   message_count, unread_count, latest_message_at
            FROM threads
            WHERE id = ?1
            "#,
            params![thread_id],
            |row| {
                Ok(StoredThread {
                    subject: row.get(0)?,
                    participants: row.get(1)?,
                    preview: row.get(2)?,
                    is_flagged: row.get(3)?,
                    has_attachments: row.get(4)?,
                    message_count: row.get::<_, usize>(5)?,
                    unread_count: row.get::<_, usize>(6)?,
                    latest_message_at: row.get(7)?,
                })
            },
        )
        .optional()
        .context("failed to load the stored thread for the merge comparison")
}

/// Loads a thread's message rows for merge comparison.
fn load_stored_messages(
    transaction: &Transaction<'_>,
    thread_id: i64,
) -> Result<Vec<StoredMessage>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT id, COALESCE(remote_id, ''), identity_id, sender, reply_to, recipients, subject,
               body_preview, body_text, sanitized_html, has_remote_content,
               is_read, is_flagged, received_at
        FROM messages
        WHERE thread_id = ?1
        "#,
    )?;
    let rows = statement.query_map(params![thread_id], |row| {
        Ok(StoredMessage {
            id: row.get(0)?,
            remote_id: row.get(1)?,
            identity_id: row.get(2)?,
            sender: row.get(3)?,
            reply_to: row.get(4)?,
            recipients: row.get(5)?,
            subject: row.get(6)?,
            body_preview: row.get(7)?,
            body_text: row.get(8)?,
            sanitized_html: row.get(9)?,
            has_remote_content: row.get(10)?,
            is_read: row.get(11)?,
            is_flagged: row.get(12)?,
            received_at: row.get(13)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load the stored messages for the merge comparison")
}

/// Removes threads of a mailbox that the snapshot no longer contains.
fn prune_mailbox_threads(
    transaction: &Transaction<'_>,
    mailbox_id: i64,
    kept_thread_ids: &HashSet<i64>,
) -> Result<bool> {
    let stale_ids: Vec<i64> = {
        let mut statement = transaction.prepare("SELECT id FROM threads WHERE mailbox_id = ?1")?;
        let rows = statement.query_map(params![mailbox_id], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list the threads during the snapshot merge")?
            .into_iter()
            .filter(|thread_id| !kept_thread_ids.contains(thread_id))
            .collect()
    };

    let pruned = !stale_ids.is_empty();
    for thread_id in stale_ids {
        transaction
            .execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
            .context("failed to remove a thread missing from the snapshot merge")?;
    }
    Ok(pruned)
}

/// Loads the locally set flag state of a thread.
fn load_thread_flag(transaction: &Transaction<'_>, thread_id: i64) -> Result<bool> {
    transaction
        .query_row(
            "SELECT is_flagged FROM threads WHERE id = ?1",
            params![thread_id],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to load the thread flag during the snapshot merge")
}

/// Removes a backup database and any transient SQLite sidecar files
/// next to it, ignoring sidecars that are already gone.
fn remove_backup_with_sidecars(path: &Path) -> Result<()> {
    let sidecars = ["-journal", "-wal", "-shm"];
    for suffix in sidecars {
        let _ = fs::remove_file(format!("{}{suffix}", path.display()));
    }
    fs::remove_file(path)
        .with_context(|| format!("failed to remove the stale backup {}", path.display()))
}

/// Loads the remote ids of messages that still have a queued write-back
/// operation, so their local state survives a snapshot merge.
fn load_pending_op_remote_ids(
    transaction: &Transaction<'_>,
    account_id: i64,
) -> Result<HashSet<String>> {
    let mut statement = transaction.prepare(
        "SELECT DISTINCT remote_id FROM pending_ops WHERE account_id = ?1 AND state = 'pending'",
    )?;
    let rows = statement.query_map(params![account_id], |row| row.get::<_, String>(0))?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load the pending write-back ids during the snapshot merge")?;
    Ok(ids.into_iter().collect())
}

/// Loads the mailbox name for a mailbox row id.
fn mailbox_name(transaction: &Transaction<'_>, mailbox_id: i64) -> Result<String> {
    transaction
        .query_row(
            "SELECT name FROM mailboxes WHERE id = ?1",
            params![mailbox_id],
            |row| row.get::<_, String>(0),
        )
        .context("failed to load the mailbox name")
}

/// Queues one write-back operation per message of a thread that has a
/// stable remote id, folded into what is already queued for each message
/// (see [`enqueue_message_op`]).
///
/// The source mailbox is passed explicitly because move operations change
/// the thread's mailbox before this helper runs.
fn enqueue_thread_ops(
    transaction: &Transaction<'_>,
    account_id: i64,
    thread_id: i64,
    source_mailbox: &str,
    kind: PendingOpKind,
    target_mailbox: Option<&str>,
) -> Result<()> {
    let remote_ids = {
        let mut statement = transaction.prepare(
            r#"
            SELECT remote_id
            FROM messages
            WHERE thread_id = ?1 AND remote_id IS NOT NULL AND remote_id != ''
            ORDER BY id ASC
            "#,
        )?;
        let rows = statement.query_map(params![thread_id], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load the remote ids of the thread's messages")?
    };
    for remote_id in remote_ids {
        enqueue_message_op(
            transaction,
            account_id,
            &remote_id,
            source_mailbox,
            kind,
            target_mailbox,
        )?;
    }
    Ok(())
}

/// Queues one write-back operation for a message, folded into the
/// operations still waiting for it so the server only sees the net change.
/// `local_mailbox` is the mailbox the message is in locally before this
/// change.
///
/// - A message has at most one queued move, and it runs after the
///   message's other operations. Until it runs, the server still has the
///   message in the move's source mailbox, under the remote id the queued
///   operations carry (an IMAP uid is only valid in that mailbox), so new
///   read and flag changes are addressed there.
/// - Moving the message back to that source cancels the queued move: an
///   archive, delete or move undone before it syncs leaves nothing to
///   write back. Moving it on retargets the queued move.
/// - A read or flag change replaces any queued change of the same kind;
///   the last one wins either way.
/// - Destroying a message with a queued move destroys it where the server
///   has it.
///
/// The sync service may already be writing back an operation folded here.
/// That loses at worst the newer change, which the next snapshot then
/// shows undone; IMAP never reuses a uid within a mailbox, so the stale
/// operation cannot reach another message.
fn enqueue_message_op(
    transaction: &Transaction<'_>,
    account_id: i64,
    remote_id: &str,
    local_mailbox: &str,
    kind: PendingOpKind,
    target_mailbox: Option<&str>,
) -> Result<()> {
    // Only a move that put the message where it is now counts; IMAP uids
    // repeat across mailboxes, so the same remote id elsewhere is another
    // message.
    let queued_move: Option<(i64, String)> = transaction
        .query_row(
            r#"
            SELECT id, source_mailbox
            FROM pending_ops
            WHERE account_id = ?1
              AND remote_id = ?2
              AND op = ?3
              AND target_mailbox = ?4
              AND state = 'pending'
            ORDER BY id DESC
            LIMIT 1
            "#,
            params![
                account_id,
                remote_id,
                PendingOpKind::Move.as_str(),
                local_mailbox
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("failed to load the queued move for a message")?;
    let server_mailbox = queued_move
        .as_ref()
        .map_or(local_mailbox, |(_, source)| source.as_str());

    match kind {
        PendingOpKind::Move => {
            let target = target_mailbox
                .filter(|name| !name.is_empty())
                .context("a move needs a target mailbox")?;
            match &queued_move {
                Some((op_id, source)) if source == target => {
                    transaction.execute("DELETE FROM pending_ops WHERE id = ?1", params![op_id])?;
                }
                Some((op_id, _)) => {
                    transaction.execute(
                        "UPDATE pending_ops SET target_mailbox = ?2 WHERE id = ?1",
                        params![op_id, target],
                    )?;
                }
                None => insert_pending_op(
                    transaction,
                    account_id,
                    kind,
                    local_mailbox,
                    remote_id,
                    Some(target),
                )?,
            }
        }
        PendingOpKind::Destroy => {
            if let Some((op_id, _)) = &queued_move {
                transaction.execute("DELETE FROM pending_ops WHERE id = ?1", params![op_id])?;
            }
            insert_pending_op(
                transaction,
                account_id,
                kind,
                server_mailbox,
                remote_id,
                None,
            )?;
        }
        PendingOpKind::MarkRead
        | PendingOpKind::MarkUnread
        | PendingOpKind::SetFlagged
        | PendingOpKind::ClearFlagged => {
            let family = match kind {
                PendingOpKind::MarkRead | PendingOpKind::MarkUnread => {
                    [PendingOpKind::MarkRead, PendingOpKind::MarkUnread]
                }
                _ => [PendingOpKind::SetFlagged, PendingOpKind::ClearFlagged],
            };
            transaction.execute(
                r#"
                DELETE FROM pending_ops
                WHERE account_id = ?1
                  AND remote_id = ?2
                  AND source_mailbox = ?3
                  AND op IN (?4, ?5)
                  AND state = 'pending'
                "#,
                params![
                    account_id,
                    remote_id,
                    server_mailbox,
                    family[0].as_str(),
                    family[1].as_str()
                ],
            )?;
            insert_pending_op(
                transaction,
                account_id,
                kind,
                server_mailbox,
                remote_id,
                None,
            )?;
            // Keep the move last, keeping its retry count.
            if let Some((op_id, _)) = &queued_move {
                transaction.execute(
                    r#"
                    UPDATE pending_ops
                    SET id = (SELECT MAX(id) FROM pending_ops) + 1
                    WHERE id = ?1
                    "#,
                    params![op_id],
                )?;
            }
        }
    }
    Ok(())
}

fn insert_pending_op(
    transaction: &Transaction<'_>,
    account_id: i64,
    kind: PendingOpKind,
    source_mailbox: &str,
    remote_id: &str,
    target_mailbox: Option<&str>,
) -> Result<()> {
    transaction
        .execute(
            r#"
            INSERT INTO pending_ops(account_id, op, source_mailbox, remote_id, target_mailbox)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                account_id,
                kind.as_str(),
                source_mailbox,
                remote_id,
                target_mailbox
            ],
        )
        .context("failed to queue a write-back operation")?;
    Ok(())
}

/// Loads the locally set read and flag state of a thread's messages.
fn load_message_flags(
    transaction: &Transaction<'_>,
    thread_id: i64,
) -> Result<std::collections::HashMap<String, (bool, bool)>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT remote_id, is_read, is_flagged
        FROM messages
        WHERE thread_id = ?1 AND remote_id IS NOT NULL
        "#,
    )?;
    let rows = statement.query_map(params![thread_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, bool>(1)?,
            row.get::<_, bool>(2)?,
        ))
    })?;
    let pairs = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load the message state during the snapshot merge")?;
    Ok(pairs
        .into_iter()
        .map(|(remote_id, is_read, is_flagged)| (remote_id, (is_read, is_flagged)))
        .collect())
}

/// Re-attaches the stored hydrated state to a lazy snapshot's messages.
///
/// Called only for threads whose rows are about to be rewritten: bodies and
/// attachment metadata are loaded for that thread alone, so unchanged
/// threads never pay the cost.
fn materialize_stored_state(
    transaction: &Transaction<'_>,
    thread_id: i64,
    thread: &ThreadSnapshot,
    thread_had_attachments: bool,
) -> Result<Vec<MessageSnapshot>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT id, COALESCE(remote_id, ''),
               body_text, sanitized_html, has_remote_content, body_preview
        FROM messages
        WHERE thread_id = ?1
        "#,
    )?;
    let rows = statement
        .query_map(params![thread_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load the stored bodies during the snapshot merge")?;

    let mut stored: std::collections::HashMap<String, StoredHydration> =
        std::collections::HashMap::with_capacity(rows.len());
    for (message_id, remote_id, body_text, sanitized_html, has_remote_content, body_preview) in rows
    {
        let attachments = if thread_had_attachments {
            load_stored_message_attachments(transaction, message_id)?
                .into_iter()
                .map(|attachment| MessageAttachment {
                    filename: attachment.filename,
                    content_type: attachment.content_type,
                    size_bytes: attachment.size_bytes,
                    storage_path: attachment.storage_path,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        stored.insert(
            remote_id,
            (
                body_text,
                sanitized_html,
                has_remote_content,
                body_preview,
                attachments,
            ),
        );
    }

    let mut materialized = Vec::with_capacity(thread.messages.len());
    for message in &thread.messages {
        let mut owned = message.clone();
        if let Some((body_text, sanitized_html, has_remote_content, body_preview, attachments)) =
            stored.get(&owned.remote_id)
        {
            let stored_has_body = body_text.is_some() || sanitized_html.is_some();
            if owned.body_text.is_none() && owned.sanitized_html.is_none() && stored_has_body {
                owned.body_text = body_text.clone();
                owned.sanitized_html = sanitized_html.clone();
                owned.has_remote_content = *has_remote_content;
                owned.body_preview = body_preview.clone();
            }
            if owned.attachments.is_empty() && thread_had_attachments {
                owned.attachments = attachments.clone();
            }
        }
        materialized.push(owned);
    }
    Ok(materialized)
}

fn auth_kind_from_db(value: String) -> rusqlite::Result<AuthKind> {
    AuthKind::parse(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown auth kind {value}"),
            )),
        )
    })
}

fn protocol_from_db(value: String) -> rusqlite::Result<ProtocolKind> {
    ProtocolKind::parse(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown protocol kind {value}"),
            )),
        )
    })
}

fn sync_phase_from_db(value: String) -> rusqlite::Result<SyncPhase> {
    SyncPhase::parse(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown sync phase {value}"),
            )),
        )
    })
}

fn utc_now_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    utc_timestamp_from_epoch(seconds)
}

/// Drops a removed account from the UI preferences: its collapsed folders,
/// and the last open mailbox if it was the account's.
fn forget_account(preferences: &mut UiPreferences, account_id: i64) {
    let tree = &mut preferences.sidebar_tree;
    tree.collapsed_accounts.retain(|id| *id != account_id);
    tree.collapsed_folders
        .retain(|folder| folder.account_id != account_id);
    if let Some(MailboxSelection::Mailbox {
        account_id: last, ..
    }) = preferences.last_mailbox
        && last == account_id
    {
        preferences.last_mailbox = None;
    }
}

fn load_ui_preferences(conn: &Connection) -> Result<UiPreferences> {
    let stored = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![UI_PREFERENCES_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to load the UI preferences")?;
    Ok(stored
        .map(|json| UiPreferences::from_json(&json))
        .unwrap_or_default())
}

fn store_ui_preferences(conn: &Connection, preferences: &UiPreferences) -> Result<()> {
    conn.execute(
        "INSERT INTO app_settings(key, value, updated_at) VALUES (?1, ?2, CURRENT_TIMESTAMP) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![UI_PREFERENCES_KEY, preferences.to_json()],
    )
    .context("failed to save the UI preferences")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use super::LocalStore;
    use zmail_core::{
        AccountColor, AccountSyncSnapshot, AuthKind, CachePolicySummary, DraftState, HydratedBody,
        MailboxRole, MailboxSelection, MailboxSnapshot, MessageAttachment, MessageRenderMode,
        MessageSnapshot, NewAccount, NewDraft, NewIdentity, PendingOpKind, ProtocolKind,
        SearchScope, SyncPhase, ThreadCounts, ThreadFilter, ThreadSnapshot, VirtualMailbox,
    };

    #[test]
    fn creates_demo_accounts_and_lists_sync_targets() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        let accounts = store.list_accounts().expect("accounts should load");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, account_id);
        assert_eq!(accounts[0].protocol, ProtocolKind::Imap);
        assert_eq!(accounts[0].identity_count, 2);
        assert_eq!(accounts[0].sync_status.phase, SyncPhase::Idle);

        let targets = store.list_sync_targets().expect("targets should load");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].identities.len(), 2);
    }

    #[test]
    fn sync_target_preserves_the_account_auth_kind() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "OAuth2 Account".to_owned(),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::OAuth2,
                endpoint: "imaps://mail.example.com:993".to_owned(),
                username: "user@example.com".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: None,
                identities: vec![NewIdentity {
                    display_name: "OAuth2 Account".to_owned(),
                    email_address: "user@example.com".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account should be inserted");

        let target = store
            .sync_target(account_id)
            .expect("the sync target should load")
            .expect("the sync target should exist");
        assert_eq!(target.auth, AuthKind::OAuth2);

        let legacy = store
            .create_demo_account(ProtocolKind::Jmap)
            .expect("the demo account should be created");
        let target = store
            .sync_target(legacy)
            .expect("the sync target should load")
            .expect("the sync target should exist");
        assert_eq!(target.auth, AuthKind::Password);
    }

    #[test]
    fn oauth2_client_config_and_expiry_round_trip_through_sync_targets() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "OAuth2 Account".to_owned(),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::OAuth2,
                endpoint: "imaps://mail.example.com:993".to_owned(),
                username: "user@example.com".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: None,
                identities: vec![NewIdentity {
                    display_name: "OAuth2 Account".to_owned(),
                    email_address: "user@example.com".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account should be inserted");

        // Before any client is saved the target carries no OAuth2 config.
        let target = store
            .sync_target(account_id)
            .expect("the sync target should load")
            .expect("the sync target should exist");
        assert!(target.oauth2.is_none(), "no client saved yet");

        store
            .save_account_oauth2_client(account_id, "https://oauth2.example.test/token", "client-1")
            .expect("the client should save");
        store
            .set_account_oauth2_expiry(account_id, Some(1_700_000_000))
            .expect("the expiry should save");

        let target = store
            .sync_target(account_id)
            .expect("the sync target should load")
            .expect("the sync target should exist");
        let config = target
            .oauth2
            .as_ref()
            .expect("the saved client should load on the target");
        assert_eq!(config.token_endpoint, "https://oauth2.example.test/token");
        assert_eq!(config.client_id, "client-1");
        assert_eq!(config.access_token_expires_at, Some(1_700_000_000));

        store
            .set_account_oauth2_expiry(account_id, None)
            .expect("the expiry should clear");
        let target = store
            .sync_target(account_id)
            .expect("the sync target should load")
            .expect("the sync target should exist");
        assert_eq!(
            target
                .oauth2
                .as_ref()
                .expect("the client should still be saved")
                .access_token_expires_at,
            None
        );

        // Password accounts never surface an OAuth2 config, even with columns set.
        let legacy = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("the demo account should be created");
        store
            .save_account_oauth2_client(legacy, "https://oauth2.example.test/token", "client-1")
            .expect("the client should save");
        let target = store
            .sync_target(legacy)
            .expect("the sync target should load")
            .expect("the sync target should exist");
        assert!(
            target.oauth2.is_none(),
            "password accounts do not carry OAuth2 config"
        );
    }

    #[test]
    fn ephemeral_stores_keep_attachments_apart_and_clean_up() {
        let first = LocalStore::open_ephemeral().expect("store should open");
        let second = LocalStore::open_ephemeral().expect("store should open");
        assert_ne!(
            first.attachment_storage_dir(),
            second.attachment_storage_dir(),
            "parallel tests must not share an attachments directory"
        );

        let directory = first
            .path()
            .parent()
            .expect("an ephemeral store has a parent directory")
            .to_path_buf();
        let blobs = first.attachment_storage_dir();
        fs::create_dir_all(&blobs).expect("the attachments directory should be created");
        fs::write(blobs.join("blob.bin"), b"blob").expect("the blob should write");

        let clone = first.clone();
        drop(first);
        assert!(
            directory.exists(),
            "the directory lives while any handle remains"
        );
        drop(clone);
        assert!(
            !directory.exists(),
            "the directory is removed with the last handle"
        );
    }

    #[test]
    fn the_imported_theme_is_stored_apart_from_the_preferences() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        assert_eq!(store.custom_theme().expect("loads"), None);
        store
            .set_custom_theme(Some(r#"{"name":"Mine"}"#))
            .expect("saves");
        store
            .update_ui_preferences(|preferences| preferences.undo_send_delay_secs = 5)
            .expect("the preferences save");
        assert_eq!(
            store.custom_theme().expect("loads").as_deref(),
            Some(r#"{"name":"Mine"}"#)
        );
        store.set_custom_theme(None).expect("clears");
        assert_eq!(store.custom_theme().expect("loads"), None);
        assert_eq!(
            store.ui_preferences().expect("loads").undo_send_delay_secs,
            5
        );
    }

    #[test]
    fn account_colours_are_stored_and_cleared() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let color = |store: &LocalStore| {
            store.list_accounts().expect("accounts should list")[0].color_override
        };
        assert_eq!(color(&store), None, "a new account uses the derived colour");

        store
            .set_account_color(account_id, Some(AccountColor::Pink))
            .expect("the colour should save");
        assert_eq!(color(&store), Some(AccountColor::Pink));
        assert_eq!(
            store.list_accounts().expect("accounts should list")[0].color(),
            AccountColor::Pink
        );

        store
            .set_account_color(account_id, None)
            .expect("the colour should clear");
        assert_eq!(color(&store), None);
    }

    #[test]
    fn disk_budgets_evict_the_oldest_blobs_first() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        // Budget round trip: MiB in, bytes out; missing means unlimited.
        assert_eq!(
            store
                .account_disk_budget_bytes(account_id)
                .expect("the budget should load"),
            None,
            "a fresh account has no budget"
        );
        store
            .set_account_disk_budget(account_id, Some(1))
            .expect("the budget should set");
        assert_eq!(
            store
                .account_disk_budget_bytes(account_id)
                .expect("the budget should load"),
            Some(1024 * 1024),
            "the budget converts MiB to bytes"
        );

        // Two hydrated threads; the older one (merged first) also carries
        // an attachment blob on disk.
        let mut old = merge_test_snapshot(
            "Old budget subject",
            "oldbody",
            "thread-budget-old",
            "message-budget-old",
            None,
        );
        old.mailboxes[0].threads[0].messages[0].attachments = vec![MessageAttachment {
            filename: "report.bin".to_owned(),
            content_type: "application/octet-stream".to_owned(),
            size_bytes: 10,
            storage_path: "incoming-budget-report.bin".to_owned(),
        }];
        let blob_dir = store.attachment_storage_dir();
        std::fs::create_dir_all(&blob_dir).expect("the attachments directory should be created");
        let blob = blob_dir.join("incoming-budget-report.bin");
        std::fs::write(&blob, b"0123456789").expect("the blob should write");
        // Both threads merge in one snapshot: a later snapshot is
        // server-authoritative, so separate merges would drop the first
        // thread.
        let newer = merge_test_snapshot(
            "New budget subject",
            "newbody",
            "thread-budget-new",
            "message-budget-new",
            None,
        );
        old.mailboxes[0]
            .threads
            .push(newer.mailboxes[0].threads[0].clone());
        store
            .replace_account_snapshot(account_id, &old)
            .expect("the snapshot should apply");

        // Usage covers both bodies (7 chars each) and the 10-byte blob.
        assert_eq!(
            store
                .account_blob_usage(account_id)
                .expect("the usage should measure"),
            24
        );

        // A budget of 17 evicts exactly the oldest body.
        let freed = store
            .evict_account_blobs(account_id, 17)
            .expect("eviction should run");
        assert_eq!(freed, 7, "only the oldest body is evicted");
        assert_eq!(
            store
                .account_blob_usage(account_id)
                .expect("the usage should measure"),
            17
        );
        let mailbox_id = mailbox_id_by_name(&store, account_id, "Inbox");
        let threads = store
            .list_threads(mailbox_id, 0..10)
            .expect("threads should load");
        let old_thread = threads
            .iter()
            .find(|thread| thread.subject == "Old budget subject")
            .expect("the old thread should exist");
        let new_thread = threads
            .iter()
            .find(|thread| thread.subject == "New budget subject")
            .expect("the new thread should exist");
        let old_view = store
            .thread_view(old_thread.id)
            .expect("the old view should load")
            .expect("the old view should exist");
        assert_eq!(
            old_view.messages[0].render_mode,
            MessageRenderMode::PreviewOnly,
            "the evicted message falls back to preview rendering"
        );
        assert!(
            !store
                .messages_missing_bodies(old_thread.id)
                .expect("missing bodies should load")
                .expect("the old thread should exist")
                .is_empty(),
            "the evicted message re-hydrates on open"
        );
        let new_view = store
            .thread_view(new_thread.id)
            .expect("the new view should load")
            .expect("the new view should exist");
        assert_eq!(
            new_view.messages[0].render_mode,
            MessageRenderMode::PlainText,
            "the newer message keeps its body"
        );
        assert_eq!(
            new_view.messages[0].attachments.len(),
            0,
            "the newer message has no attachments"
        );

        // A tighter budget evicts the remaining body and the blob.
        let freed = store
            .evict_account_blobs(account_id, 4)
            .expect("eviction should run");
        assert_eq!(freed, 17, "the remaining body and blob are evicted");
        assert_eq!(
            store
                .account_blob_usage(account_id)
                .expect("the usage should measure"),
            0
        );
        assert!(
            !blob.exists(),
            "the attachment blob file is removed from disk"
        );
        let old_view = store
            .thread_view(old_thread.id)
            .expect("the old view should load")
            .expect("the old view should exist");
        assert_eq!(
            old_view.messages[0].attachments.len(),
            0,
            "the attachment returns to metadata-only"
        );
        let _ = std::fs::remove_file(&blob);

        // An under-budget eviction is a no-op.
        let freed = store
            .evict_account_blobs(account_id, 1024)
            .expect("eviction should run");
        assert_eq!(freed, 0, "nothing is evicted below the budget");
    }

    #[test]
    fn applies_snapshots_and_surfaces_threads() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Jmap)
            .expect("account should be created");

        let target = store
            .sync_target(account_id)
            .expect("target should load")
            .expect("account should exist");
        let primary_identity = target.identities[0].email_address.clone();
        let snapshot = AccountSyncSnapshot {
            checkpoint: "jmap:1".to_owned(),
            detail: "JMAP discovery complete".to_owned(),
            identities: None,
            mailboxes: vec![MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: "thread-1".to_owned(),
                    subject: "Phase 2 review".to_owned(),
                    participants: "Kai Santos, Nora Walsh".to_owned(),
                    preview: "The background supervisor now persists sync state.".to_owned(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 1,
                    latest_message_at: "2026-03-21T09:15:00Z".to_owned(),
                    messages: vec![MessageSnapshot {
                        remote_id: "message-1".to_owned(),
                        identity_email: primary_identity,
                        sender: "Kai Santos".to_owned(),
                        reply_to: None,
                        recipients: target.username.clone(),
                        subject: "Phase 2 review".to_owned(),
                        body_preview: "The background supervisor now persists sync state."
                            .to_owned(),
                        body_text: Some(
                            "The background supervisor now persists sync state.".to_owned(),
                        ),
                        sanitized_html: None,
                        has_remote_content: false,
                        is_read: false,
                        is_flagged: false,
                        received_at: "2026-03-21T09:15:00Z".to_owned(),
                        attachments: Vec::new(),
                    }],
                }],
                ..Default::default()
            }],
        };

        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");
        store
            .record_sync_success(account_id, &snapshot.detail, &snapshot.checkpoint)
            .expect("success should record");

        let mailboxes = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load");
        assert_eq!(mailboxes.len(), 1);
        let mailbox_id = mailboxes[0].id;

        let threads = store
            .list_threads(mailbox_id, 0..50)
            .expect("threads should load");
        assert_eq!(threads.len(), 1);

        let preview = store
            .message_preview(threads[0].id)
            .expect("preview query should work")
            .expect("preview should exist");
        assert_eq!(preview.mailbox_name, "Inbox");
        assert_eq!(preview.subject, "Phase 2 review");
    }

    #[test]
    fn reply_to_survives_the_snapshot_roundtrip() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let target = store
            .sync_target(account_id)
            .expect("target should load")
            .expect("account should exist");
        let primary_identity = target.identities[0].email_address.clone();

        let snapshot = AccountSyncSnapshot {
            checkpoint: "imap:1".to_owned(),
            detail: "sync complete".to_owned(),
            identities: None,
            mailboxes: vec![MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: "thread-1".to_owned(),
                    subject: "Check your account".to_owned(),
                    participants: "Security <security@paypa1.com>".to_owned(),
                    preview: "Urgent: verify your account now.".to_owned(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 1,
                    latest_message_at: "2026-10-15T09:00:00Z".to_owned(),
                    messages: vec![MessageSnapshot {
                        remote_id: "message-1".to_owned(),
                        identity_email: primary_identity,
                        sender: "Security <security@paypa1.com>".to_owned(),
                        reply_to: Some("evil <attacker@evil.example>".to_owned()),
                        recipients: target.username.clone(),
                        subject: "Check your account".to_owned(),
                        body_preview: "Urgent: verify your account now.".to_owned(),
                        body_text: Some("Urgent: verify your account now.".to_owned()),
                        sanitized_html: None,
                        has_remote_content: false,
                        is_read: false,
                        is_flagged: false,
                        received_at: "2026-10-15T09:00:00Z".to_owned(),
                        attachments: Vec::new(),
                    }],
                }],
                ..Default::default()
            }],
        };

        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");

        let mailboxes = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load");
        let threads = store
            .list_threads(mailboxes[0].id, 0..50)
            .expect("threads should load");
        let view = store
            .thread_view(threads[0].id)
            .expect("thread should load")
            .expect("thread should exist");
        assert_eq!(
            view.messages[0].reply_to.as_deref(),
            Some("evil <attacker@evil.example>")
        );

        // The stored Reply-To participates in the sender-safety analysis.
        let report = zmail_security::sender_safety::analyze_sender_identity(
            &view.messages[0].sender,
            view.messages[0].reply_to.as_deref(),
        );
        assert_eq!(report.lookalike_of, Some("paypal.com".to_owned()));
        assert_eq!(report.reply_to_mismatch, Some("evil.example".to_owned()));
    }

    #[test]
    fn dismissing_a_sender_warning_persists_per_message() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let snapshot =
            merge_test_snapshot("Check your account", "Body", "thread-1", "message-1", None);
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");
        let threads = store
            .list_threads(mailbox_id_by_name(&store, account_id, "Inbox"), 0..50)
            .expect("threads should load");
        let view = store
            .thread_view(threads[0].id)
            .expect("thread should load")
            .expect("thread should exist");
        let message_id = view.messages[0].id;
        assert!(!view.messages[0].sender_warning_dismissed);

        assert!(
            store
                .dismiss_sender_warning(message_id)
                .expect("dismissal should save")
        );
        let view = store
            .thread_view(threads[0].id)
            .expect("thread should load")
            .expect("thread should exist");
        assert!(view.messages[0].sender_warning_dismissed);

        // A sync that rewrites the thread (here a changed preview) keeps
        // the choice: messages are re-inserted with new row ids.
        let mut resync = snapshot.clone();
        resync.mailboxes[0].threads[0].preview = "A changed preview".to_owned();
        store
            .replace_account_snapshot(account_id, &resync)
            .expect("snapshot should apply again");
        let view = store
            .thread_view(threads[0].id)
            .expect("thread should load")
            .expect("thread should exist");
        assert!(view.messages[0].sender_warning_dismissed);
        assert!(
            !store
                .dismiss_sender_warning(i64::MAX)
                .expect("a missing message is not an error")
        );
    }

    #[test]
    fn records_sync_failures() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        store
            .begin_sync_attempt(account_id, SyncPhase::Connecting, "Connecting to server")
            .expect("phase update should work");
        store
            .record_sync_failure(account_id, "Sync failed", "server greeting timeout")
            .expect("failure should record");

        let account = store
            .list_accounts()
            .expect("accounts should load")
            .into_iter()
            .find(|account| account.id == account_id)
            .expect("account should exist");
        assert_eq!(account.sync_status.phase, SyncPhase::Failed);
        assert_eq!(account.sync_status.failure_count, 1);
        assert_eq!(
            account.sync_status.last_error_message.as_deref(),
            Some("server greeting timeout")
        );
    }

    /// Builds a one-thread, one-message snapshot with stable remote ids.
    /// A one-message thread for the virtual mailbox tests: `id` names the
    /// conversation, `minute` orders the threads (later is newer).
    fn listed_thread(
        id: &str,
        copy: &str,
        minute: u32,
        flagged: bool,
        unread: bool,
    ) -> ThreadSnapshot {
        let at = format!("2026-03-21T09:{minute:02}:00Z");
        ThreadSnapshot {
            remote_thread_id: id.to_owned(),
            subject: id.to_owned(),
            participants: "Kai Santos".to_owned(),
            preview: id.to_owned(),
            is_flagged: flagged,
            has_attachments: false,
            unread_count: usize::from(unread),
            latest_message_at: at.clone(),
            messages: vec![MessageSnapshot {
                remote_id: format!("{id}-{copy}"),
                identity_email: "phase2@localhost".to_owned(),
                sender: "Kai Santos".to_owned(),
                reply_to: None,
                recipients: "phase2@localhost".to_owned(),
                subject: id.to_owned(),
                body_preview: id.to_owned(),
                body_text: None,
                sanitized_html: None,
                has_remote_content: false,
                is_read: !unread,
                is_flagged: flagged,
                received_at: at,
                attachments: Vec::new(),
            }],
        }
    }

    /// Two accounts with mail in Inbox, Sent, Archive and Trash.
    fn two_account_store() -> (LocalStore, i64, i64) {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let mailbox = |name: &str, role: &str, threads: Vec<ThreadSnapshot>| MailboxSnapshot {
            name: name.to_owned(),
            role: Some(role.to_owned()),
            threads,
            ..Default::default()
        };
        let snapshot = |mailboxes| AccountSyncSnapshot {
            checkpoint: "virtual:test".to_owned(),
            detail: "virtual test sync".to_owned(),
            identities: None,
            mailboxes,
        };
        let work = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(
                work,
                &snapshot(vec![
                    mailbox(
                        "INBOX",
                        "inbox",
                        vec![
                            listed_thread("work-new", "inbox", 50, false, true),
                            listed_thread("work-flagged", "inbox", 10, true, false),
                        ],
                    ),
                    mailbox(
                        "Sent",
                        "sent",
                        vec![listed_thread("work-sent", "sent", 40, false, false)],
                    ),
                    // Gmail-style: the flagged conversation is also in All Mail.
                    mailbox(
                        "All Mail",
                        "archive",
                        vec![
                            listed_thread("work-flagged", "all", 10, true, false),
                            listed_thread("work-archived-flag", "all", 5, true, true),
                        ],
                    ),
                    mailbox(
                        "Trash",
                        "trash",
                        vec![listed_thread("work-binned", "trash", 59, true, true)],
                    ),
                ]),
            )
            .expect("snapshot should apply");
        let home = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(
                home,
                &snapshot(vec![
                    mailbox(
                        "Inbox",
                        "inbox",
                        vec![
                            listed_thread("home-old", "inbox", 1, false, false),
                            listed_thread("home-mid", "inbox", 30, true, true),
                        ],
                    ),
                    mailbox("Sent", "sent", Vec::new()),
                ]),
            )
            .expect("snapshot should apply");
        (store, work, home)
    }

    #[test]
    fn deleting_an_account_removes_its_local_data_and_keeps_the_other() {
        let (store, work, home) = two_account_store();
        let identity = store.list_identities(work).expect("identities")[0].id;
        let draft = |subject: &str| {
            store
                .create_draft(&NewDraft {
                    account_id: work,
                    identity_id: identity,
                    to: vec!["kai@example.test".to_owned()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: subject.to_owned(),
                    body: "Hello".to_owned(),
                })
                .expect("the draft is stored")
        };
        draft("Still a draft");
        let queued = draft("Waiting to send");
        store.queue_draft(queued).expect("queued");
        let inbox = store
            .mailbox_id_by_role(work, "inbox")
            .expect("lookup")
            .expect("an inbox");
        let thread = store
            .first_thread_id(inbox)
            .expect("lookup")
            .expect("a thread");
        store
            .set_thread_read(thread, true)
            .expect("a change is queued");
        store
            .update_ui_preferences(|preferences| {
                preferences.last_mailbox = Some(MailboxSelection::Mailbox {
                    account_id: work,
                    mailbox_id: inbox,
                });
                preferences.sidebar_tree.collapsed_accounts = vec![work, home];
            })
            .expect("the preferences save");

        assert_eq!(
            store.account_removal_impact(work).expect("counts"),
            super::AccountRemovalImpact {
                unsent_messages: 1,
                sending_messages: 0,
                drafts: 1,
            }
        );
        assert!(
            !store
                .search_threads(work, "work-new", 10)
                .expect("search")
                .is_empty()
        );

        store.delete_account(work).expect("the account is removed");

        let accounts = store.list_accounts().expect("accounts");
        assert_eq!(accounts.iter().map(|a| a.id).collect::<Vec<_>>(), [home]);
        assert!(store.list_mailboxes(work).expect("mailboxes").is_empty());
        assert!(store.list_drafts(Some(work)).expect("drafts").is_empty());
        assert!(
            store
                .list_outbox_jobs(Some(work))
                .expect("outbox")
                .is_empty()
        );
        assert_eq!(store.pending_op_count(work).expect("ops"), 0);
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        let left: Vec<String> = store
            .list_selection_threads(&unified, 0..10)
            .expect("threads")
            .into_iter()
            .map(|thread| thread.subject)
            .collect();
        assert_eq!(left, ["home-mid", "home-old"]);
        assert!(
            store
                .search_threads_in(&SearchScope::AllAccounts, "work", 10)
                .expect("search")
                .is_empty(),
            "the search index forgets the account's mail"
        );
        let preferences = store.ui_preferences().expect("preferences");
        assert_eq!(preferences.last_mailbox, None);
        assert_eq!(preferences.sidebar_tree.collapsed_accounts, [home]);

        // Removing it again is harmless.
        store.delete_account(work).expect("nothing to remove");
    }

    #[test]
    fn deleting_an_account_removes_its_attachment_files() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let blob_dir = store.attachment_storage_dir();
        std::fs::create_dir_all(&blob_dir).expect("the attachments directory should be created");
        let mut accounts = Vec::new();
        for (name, file) in [("first", "own.bin"), ("second", "shared.bin")] {
            let account_id = store
                .create_demo_account(ProtocolKind::Imap)
                .expect("account should be created");
            let mut snapshot = merge_test_snapshot(
                name,
                "body",
                &format!("thread-{name}"),
                &format!("message-{name}"),
                None,
            );
            snapshot.mailboxes[0].threads[0].messages[0].attachments = [file, "shared.bin"]
                .into_iter()
                .map(|path| MessageAttachment {
                    filename: path.to_owned(),
                    content_type: "application/octet-stream".to_owned(),
                    size_bytes: 1,
                    storage_path: path.to_owned(),
                })
                .collect();
            store
                .replace_account_snapshot(account_id, &snapshot)
                .expect("snapshot should apply");
            std::fs::write(blob_dir.join(file), b"x").expect("the blob should write");
            accounts.push(account_id);
        }

        store.delete_account(accounts[0]).expect("removed");
        assert!(!blob_dir.join("own.bin").exists(), "its own file goes");
        assert!(
            blob_dir.join("shared.bin").exists(),
            "a file the other account also uses stays"
        );
    }

    #[test]
    fn the_unified_inbox_merges_every_accounts_inbox_newest_first() {
        let (store, work, home) = two_account_store();
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        let rows = store
            .list_selection_threads(&unified, 0..10)
            .expect("threads should list");
        let listed = rows
            .iter()
            .map(|row| (row.subject.as_str(), row.account_id))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            [
                ("work-new", work),
                ("home-mid", home),
                ("work-flagged", work),
                ("home-old", home),
            ]
        );
        let home_inbox = store
            .mailbox_id_by_role(home, "inbox")
            .expect("role lookup")
            .expect("home has an inbox");
        assert_eq!(rows[1].mailbox_id, home_inbox, "rows name their mailbox");
        assert_eq!(
            store
                .list_selection_threads(&unified, 1..3)
                .expect("threads should list")
                .iter()
                .map(|row| row.subject.as_str())
                .collect::<Vec<_>>(),
            ["home-mid", "work-flagged"],
            "ranges page through the merged list"
        );
        assert_eq!(
            store.selection_counts(&unified).expect("counts"),
            ThreadCounts {
                total_threads: 4,
                unread_threads: 2,
            }
        );
        assert_eq!(
            store
                .first_selection_thread(&unified)
                .expect("first thread")
                .map(|row| row.subject),
            Some("work-new".to_owned())
        );

        let sent = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Sent));
        assert_eq!(
            store.selection_counts(&sent).expect("counts").total_threads,
            1
        );
        let one = MailboxSelection::Mailbox {
            account_id: home,
            mailbox_id: home_inbox,
        };
        assert_eq!(
            store
                .list_selection_threads(&one, 0..10)
                .expect("threads should list")
                .len(),
            2
        );
    }

    #[test]
    fn selection_filters_embed_before_numbered_parameters() {
        // The selection filters use anonymous `?`s; queries that add
        // numbered ones after them must still bind the right values.
        let (store, _, home) = two_account_store();
        let home_inbox = store
            .mailbox_id_by_role(home, "inbox")
            .expect("role lookup")
            .expect("home has an inbox");
        let one = MailboxSelection::Mailbox {
            account_id: home,
            mailbox_id: home_inbox,
        };
        let rows = store
            .list_selection_threads(&one, 0..10)
            .expect("threads should list");
        let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids.len(), 2);
        assert!(store.selection_contains(&one, ids[0]).expect("check"));
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        assert!(store.selection_contains(&unified, ids[1]).expect("check"));
        assert_eq!(
            store
                .thread_marks(&ids, Some(&one))
                .expect("marks should load")
                .len(),
            2
        );
        assert_eq!(
            store
                .thread_marks(&ids, Some(&unified))
                .expect("marks should load")
                .len(),
            2
        );
    }

    #[test]
    fn filters_narrow_a_selection_and_its_counts() {
        let (store, _, _) = two_account_store();
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        let subjects = |filter, range| {
            store
                .list_filtered_threads(&unified, filter, range)
                .expect("threads should list")
                .into_iter()
                .map(|row| row.subject)
                .collect::<Vec<_>>()
        };
        let total = |filter| {
            store
                .filtered_counts(&unified, filter)
                .expect("counts")
                .total_threads
        };

        assert_eq!(subjects(ThreadFilter::All, 0..10).len(), 4);
        assert_eq!(
            subjects(ThreadFilter::Unread, 0..10),
            ["work-new", "home-mid"]
        );
        assert_eq!(subjects(ThreadFilter::Unread, 1..10), ["home-mid"]);
        assert_eq!(total(ThreadFilter::Unread), 2);
        assert_eq!(
            subjects(ThreadFilter::Flagged, 0..10),
            ["home-mid", "work-flagged"]
        );
        assert_eq!(total(ThreadFilter::Flagged), 2);
        assert!(subjects(ThreadFilter::HasAttachments, 0..10).is_empty());

        store
            .connection()
            .expect("connection")
            .execute(
                "UPDATE threads SET has_attachments = 1 WHERE subject = 'home-old'",
                [],
            )
            .expect("update");
        assert_eq!(subjects(ThreadFilter::HasAttachments, 0..10), ["home-old"]);
        assert_eq!(
            store
                .filtered_counts(&unified, ThreadFilter::HasAttachments)
                .expect("counts"),
            ThreadCounts {
                total_threads: 1,
                unread_threads: 0,
            }
        );
        assert_eq!(
            store
                .selection_counts(&unified)
                .expect("counts")
                .total_threads,
            4,
            "the unfiltered counts are unchanged"
        );
    }

    #[test]
    fn flagged_lists_each_conversation_once_outside_trash() {
        let (store, work, _) = two_account_store();
        let flagged = MailboxSelection::Virtual(VirtualMailbox::Flagged);
        let rows = store
            .list_selection_threads(&flagged, 0..10)
            .expect("threads should list");
        let listed = rows
            .iter()
            .map(|row| row.subject.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            ["home-mid", "work-flagged", "work-archived-flag"],
            "Trash is left out and the All Mail copy is not repeated"
        );
        assert!(
            store
                .selection_contains(&flagged, rows[1].id)
                .expect("contains")
        );
        let copies = store
            .list_threads(
                store
                    .mailbox_id_by_role(work, "archive")
                    .expect("role lookup")
                    .expect("All Mail"),
                0..10,
            )
            .expect("threads should list");
        assert!(
            !store
                .selection_contains(&flagged, copies[0].id)
                .expect("contains"),
            "the All Mail copy is not listed"
        );
        let work_inbox = store
            .mailbox_id_by_role(work, "inbox")
            .expect("role lookup")
            .expect("work has an inbox");
        assert_eq!(rows[1].mailbox_id, work_inbox, "the Inbox copy is listed");
        assert_eq!(
            store.selection_counts(&flagged).expect("counts"),
            ThreadCounts {
                total_threads: 3,
                unread_threads: 2,
            }
        );
    }

    #[test]
    fn every_accounts_mailboxes_list_together() {
        let (store, work, home) = two_account_store();
        let accounts = store
            .list_all_mailboxes()
            .expect("mailboxes should list")
            .iter()
            .map(|mailbox| (mailbox.account_id, mailbox.name.clone()))
            .collect::<Vec<_>>();
        let expect = |account: i64, names: &[&str]| {
            names
                .iter()
                .map(|name| (account, (*name).to_owned()))
                .collect::<Vec<_>>()
        };
        let mut expected = expect(work, &["INBOX", "Sent", "All Mail", "Trash"]);
        expected.extend(expect(home, &["Inbox", "Sent"]));
        assert_eq!(accounts, expected);
    }

    fn merge_test_snapshot(
        subject: &str,
        body: &str,
        remote_thread_id: &str,
        remote_id: &str,
        extra_identity: Option<&str>,
    ) -> AccountSyncSnapshot {
        AccountSyncSnapshot {
            checkpoint: "merge:test".to_owned(),
            detail: "merge test sync".to_owned(),
            identities: None,
            mailboxes: vec![MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: remote_thread_id.to_owned(),
                    subject: subject.to_owned(),
                    participants: "Kai Santos".to_owned(),
                    preview: body.to_owned(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 1,
                    latest_message_at: "2026-03-21T09:15:00Z".to_owned(),
                    messages: vec![MessageSnapshot {
                        remote_id: remote_id.to_owned(),
                        identity_email: extra_identity
                            .map(str::to_owned)
                            .unwrap_or_else(|| "phase2@localhost".to_owned()),
                        sender: "Kai Santos".to_owned(),
                        reply_to: None,
                        recipients: "phase2@localhost".to_owned(),
                        subject: subject.to_owned(),
                        body_preview: body.to_owned(),
                        body_text: Some(body.to_owned()),
                        sanitized_html: None,
                        has_remote_content: false,
                        is_read: false,
                        is_flagged: false,
                        received_at: "2026-03-21T09:15:00Z".to_owned(),
                        attachments: Vec::new(),
                    }],
                }],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn mailboxes_list_roles_first_with_their_hierarchy() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let mailbox = |name: &str, role: Option<&str>, parent: Option<&str>| MailboxSnapshot {
            name: name.to_owned(),
            role: role.map(str::to_owned),
            delimiter: Some("/".to_owned()),
            parent: parent.map(str::to_owned),
            ..Default::default()
        };
        let mut snapshot = merge_test_snapshot("Subject", "Body", "thread-1", "message-1", None);
        snapshot.mailboxes.extend([
            mailbox("Projects", None, None),
            mailbox("Trash", Some("trash"), None),
            mailbox("Projects/Website", None, Some("Projects")),
            mailbox("Sent", Some("SENT"), None),
            mailbox("Notes", Some("important"), None),
        ]);
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");

        let mailboxes = store
            .list_mailboxes(account_id)
            .expect("mailboxes should list");
        let listed = mailboxes
            .iter()
            .map(|mailbox| (mailbox.name.as_str(), mailbox.role))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            [
                ("Inbox", Some(MailboxRole::Inbox)),
                ("Sent", Some(MailboxRole::Sent)),
                ("Trash", Some(MailboxRole::Trash)),
                ("Projects", None),
                ("Projects/Website", None),
                ("Notes", None),
            ],
            "roles sort first, the rest keep server order; unknown roles are none"
        );
        let website = &mailboxes[4];
        assert_eq!(website.account_id, account_id);
        assert_eq!(website.parent.as_deref(), Some("Projects"));
        assert_eq!(website.delimiter.as_deref(), Some("/"));
        assert_eq!(website.leaf_name(), "Website");

        // A later sync that renames the role or moves the folder updates
        // the row in place.
        let projects = snapshot
            .mailboxes
            .iter_mut()
            .find(|mailbox| mailbox.name == "Projects/Website")
            .expect("the folder is in the snapshot");
        projects.parent = None;
        projects.role = Some("archive".to_owned());
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");
        let website = store
            .list_mailboxes(account_id)
            .expect("mailboxes should list")
            .into_iter()
            .find(|mailbox| mailbox.name == "Projects/Website")
            .expect("the folder is still listed");
        assert_eq!(website.id, mailboxes[4].id, "the row is kept");
        assert_eq!(website.parent, None);
        assert_eq!(website.role, Some(MailboxRole::Archive));

        let exported = store
            .account_mailbox_snapshots(account_id)
            .expect("snapshots should export");
        let sent = exported
            .iter()
            .find(|mailbox| mailbox.name == "Sent")
            .expect("Sent is exported");
        assert_eq!(sent.delimiter.as_deref(), Some("/"));
    }

    #[test]
    fn mailbox_name_by_role_resolves_the_trash_mailbox() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        let mut snapshot =
            merge_test_snapshot("Trash subject", "Trash body", "thread-6", "message-6", None);
        snapshot.mailboxes.push(MailboxSnapshot {
            name: "Deleted Items".to_owned(),
            role: Some("trash".to_owned()),
            threads: Vec::new(),
            ..Default::default()
        });
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");

        assert_eq!(
            store
                .mailbox_name_by_role(account_id, "TRASH")
                .expect("name lookup should work")
                .as_deref(),
            Some("Deleted Items"),
            "role lookup is case-insensitive"
        );
        assert_eq!(
            store
                .mailbox_name_by_role(account_id, "archive")
                .expect("name lookup should work"),
            None,
            "unknown roles resolve to nothing"
        );
    }

    #[test]
    fn dead_lettered_operations_stop_retrying_and_stop_guarding_merges() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot("Dead subject", "Dead body", "thread-5", "message-5", None),
            )
            .expect("snapshot should apply");

        let mailbox_id = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .id;
        let thread_id = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .id;
        store
            .set_thread_read(thread_id, true)
            .expect("thread should be marked read");
        let op = store
            .list_pending_ops(account_id)
            .expect("ops should load")
            .pop()
            .expect("the read operation should be queued");

        // Dead-letter the operation.
        store
            .mark_pending_op_dead(op.id, "permanently rejected")
            .expect("the operation should dead-letter");
        assert_eq!(
            store
                .list_pending_ops(account_id)
                .expect("pending ops should load")
                .len(),
            0,
            "dead operations leave the retry queue"
        );
        assert_eq!(
            store.dead_op_count(account_id).expect("count should load"),
            1,
            "the dead operation is counted"
        );
        let dead = store
            .list_dead_ops(account_id)
            .expect("dead ops should load");
        assert_eq!(dead.len(), 1);
        assert_eq!(
            dead[0].last_error.as_deref(),
            Some("permanently rejected"),
            "the dead operation keeps its last error"
        );

        // A dead operation no longer protects the local read state, so
        // the server snapshot wins again.
        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot("Dead subject", "Dead body", "thread-5", "message-5", None),
            )
            .expect("refreshed snapshot should apply");
        let thread = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .clone();
        assert_eq!(thread.unread_count, 1, "server state wins for dead ops");

        // Retry requeues the operation with a fresh budget; discard removes it.
        store
            .retry_dead_op(dead[0].id)
            .expect("retry should requeue");
        assert_eq!(
            store
                .list_pending_ops(account_id)
                .expect("pending ops should load")
                .len(),
            1,
            "retry returns the operation to the queue"
        );
        assert_eq!(
            store.dead_op_count(account_id).expect("count should load"),
            0
        );
        let requeued = store
            .list_pending_ops(account_id)
            .expect("pending ops should load")
            .pop()
            .expect("the requeued operation exists");
        assert_eq!(requeued.attempts, 0, "retry resets the attempt budget");
        store
            .mark_pending_op_dead(requeued.id, "failed again")
            .expect("the operation should dead-letter again");
        store
            .discard_dead_op(requeued.id)
            .expect("discard should remove the record");
        assert_eq!(
            store.dead_op_count(account_id).expect("count should load"),
            0,
            "discarding removes the dead record"
        );
    }

    #[test]
    fn snapshot_remerge_skips_unchanged_threads() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Stable subject",
                    "Stable body",
                    "thread-7",
                    "message-7",
                    None,
                ),
            )
            .expect("snapshot should apply");

        let mailbox_id = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .id;
        let thread_id = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .id;
        let before = store
            .thread_view(thread_id)
            .expect("thread view should load")
            .expect("thread view should exist");
        let before_message_ids: Vec<i64> = before.messages.iter().map(|m| m.id).collect();
        assert_eq!(before_message_ids.len(), 1);

        // Re-applying the identical snapshot must not rewrite the thread:
        // the message row ids stay the same instead of being re-inserted.
        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Stable subject",
                    "Stable body",
                    "thread-7",
                    "message-7",
                    None,
                ),
            )
            .expect("identical re-merge should apply");

        let after = store
            .thread_view(thread_id)
            .expect("thread view should load")
            .expect("thread view should exist");
        let after_message_ids: Vec<i64> = after.messages.iter().map(|m| m.id).collect();
        assert_eq!(
            before_message_ids, after_message_ids,
            "unchanged threads are not rewritten"
        );

        // Search still finds the thread and a real change still updates it.
        let hits = store
            .search_threads(account_id, "Stable", 10)
            .expect("search should run");
        assert!(
            !hits.is_empty(),
            "the search index stays intact after a skipped merge"
        );

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Changed subject",
                    "Stable body",
                    "thread-7",
                    "message-7",
                    None,
                ),
            )
            .expect("changed re-merge should apply");
        let thread = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .clone();
        assert_eq!(
            thread.subject, "Changed subject",
            "changed threads are still rewritten"
        );
    }

    #[test]
    fn snapshot_merge_follows_the_server_without_pending_operations() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Server subject",
                    "Server body",
                    "thread-4",
                    "message-4",
                    None,
                ),
            )
            .expect("initial snapshot should apply");

        let mailbox_id = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .id;
        let thread_id = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .id;

        store
            .set_thread_read(thread_id, true)
            .expect("thread should be marked read");
        store
            .set_thread_flagged(thread_id, true)
            .expect("thread should be flagged");

        // Once the write-back operations are no longer queued, the server
        // state wins again: another client may have changed it.
        for op in store.list_pending_ops(account_id).expect("ops should load") {
            store
                .complete_pending_op(op.id)
                .expect("op should complete");
        }

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Server subject",
                    "Server body",
                    "thread-4",
                    "message-4",
                    None,
                ),
            )
            .expect("refreshed snapshot should apply");

        let thread = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .clone();
        assert_eq!(thread.unread_count, 1, "the server unread state should win");
        assert!(!thread.is_flagged, "the server flag state should win");
    }

    #[test]
    fn snapshot_merge_preserves_local_read_and_flag_state() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Server subject",
                    "Server body",
                    "thread-1",
                    "message-1",
                    None,
                ),
            )
            .expect("initial snapshot should apply");

        let mailbox_id = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .id;
        let thread_id = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .id;

        store
            .set_thread_read(thread_id, true)
            .expect("thread should be marked read");
        store
            .set_thread_flagged(thread_id, true)
            .expect("thread should be flagged");

        // The next sync reports the server state; the locally changed
        // read and flag state must survive it.
        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Server subject",
                    "Server body",
                    "thread-1",
                    "message-1",
                    None,
                ),
            )
            .expect("refreshed snapshot should apply");

        let mailbox = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .clone();
        assert_eq!(mailbox.total_threads, 1, "the merged thread should remain");
        assert_eq!(
            mailbox.unread_threads, 0,
            "the local read state should survive"
        );

        let thread = store
            .list_threads(mailbox.id, 0..1)
            .expect("threads should load")[0]
            .clone();
        assert_eq!(thread.id, thread_id, "thread row identity should be stable");
        assert!(thread.is_flagged, "the local flag state should survive");
        assert_eq!(thread.unread_count, 0, "the thread should stay read");

        let preview = store
            .message_preview(thread.id)
            .expect("preview query should work")
            .expect("preview should exist");
        assert_eq!(
            preview.unread_count, 0,
            "the message should keep the local read state"
        );
    }

    #[test]
    fn snapshot_merge_applies_additions_and_removals() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot("First message", "First body", "thread-1", "message-1", None),
            )
            .expect("initial snapshot should apply");

        // The refreshed snapshot replaces thread-1 with thread-2.
        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Second message",
                    "Second body",
                    "thread-2",
                    "message-2",
                    None,
                ),
            )
            .expect("refreshed snapshot should apply");

        let mailbox = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .clone();
        assert_eq!(mailbox.total_threads, 1, "exactly one thread should remain");

        let threads = store
            .list_threads(mailbox.id, 0..10)
            .expect("threads should load");
        assert_eq!(threads.len(), 1, "the stale thread should be pruned");
        assert_eq!(threads[0].subject, "Second message");
    }

    #[test]
    fn snapshot_merge_updates_content_of_existing_threads() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Original subject",
                    "Original body",
                    "thread-1",
                    "message-1",
                    None,
                ),
            )
            .expect("initial snapshot should apply");

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Renamed subject",
                    "Renamed body",
                    "thread-1",
                    "message-1",
                    None,
                ),
            )
            .expect("refreshed snapshot should apply");

        let mailbox_id = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .id;
        let thread = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .clone();
        assert_eq!(thread.subject, "Renamed subject");
        assert_eq!(thread.preview, "Renamed body");
    }

    #[test]
    fn snapshot_merge_drops_mailboxes_missing_from_the_snapshot() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        let mut first =
            merge_test_snapshot("Only message", "Only body", "thread-1", "message-1", None);
        first.mailboxes.push(MailboxSnapshot {
            name: "Archive".to_owned(),
            role: Some("archive".to_owned()),
            threads: Vec::new(),
            ..Default::default()
        });
        store
            .replace_account_snapshot(account_id, &first)
            .expect("initial snapshot should apply");

        // The refreshed snapshot no longer contains the Archive mailbox.
        let refreshed =
            merge_test_snapshot("Only message", "Only body", "thread-1", "message-1", None);
        store
            .replace_account_snapshot(account_id, &refreshed)
            .expect("refreshed snapshot should apply");

        let names: Vec<String> = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")
            .into_iter()
            .map(|mailbox| mailbox.name)
            .collect();
        assert_eq!(names, vec!["Inbox".to_owned()]);
    }

    #[test]
    fn replaces_identities_after_clearing_existing_messages() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Jmap)
            .expect("account should be created");

        let original_target = store
            .sync_target(account_id)
            .expect("target should load")
            .expect("account should exist");
        let first_snapshot = AccountSyncSnapshot {
            checkpoint: "jmap:1".to_owned(),
            detail: "Initial sync".to_owned(),
            identities: None,
            mailboxes: vec![MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: "thread-1".to_owned(),
                    subject: "Initial message".to_owned(),
                    participants: "Kai Santos".to_owned(),
                    preview: "Initial body preview".to_owned(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 1,
                    latest_message_at: "2026-03-21T09:15:00Z".to_owned(),
                    messages: vec![MessageSnapshot {
                        remote_id: "message-1".to_owned(),
                        identity_email: original_target.identities[0].email_address.clone(),
                        sender: "Kai Santos".to_owned(),
                        reply_to: None,
                        recipients: original_target.username.clone(),
                        subject: "Initial message".to_owned(),
                        body_preview: "Initial body preview".to_owned(),
                        body_text: Some("Initial body preview".to_owned()),
                        sanitized_html: None,
                        has_remote_content: false,
                        is_read: false,
                        is_flagged: false,
                        received_at: "2026-03-21T09:15:00Z".to_owned(),
                        attachments: Vec::new(),
                    }],
                }],
                ..Default::default()
            }],
        };
        store
            .replace_account_snapshot(account_id, &first_snapshot)
            .expect("initial snapshot should apply");

        let refreshed_identity = NewIdentity {
            display_name: "James Identity".to_owned(),
            email_address: "jmap@localhost".to_owned(),
            signature: "Regards,\nJames".to_owned(),
            is_primary: true,
        };
        let refreshed_snapshot = AccountSyncSnapshot {
            checkpoint: "jmap:2".to_owned(),
            detail: "Identity refresh".to_owned(),
            identities: Some(vec![refreshed_identity.clone()]),
            mailboxes: vec![MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: "thread-1".to_owned(),
                    subject: "Updated message".to_owned(),
                    participants: "James User".to_owned(),
                    preview: "Updated body preview".to_owned(),
                    is_flagged: true,
                    has_attachments: false,
                    unread_count: 0,
                    latest_message_at: "2026-03-21T10:00:00Z".to_owned(),
                    messages: vec![MessageSnapshot {
                        remote_id: "message-1".to_owned(),
                        identity_email: refreshed_identity.email_address.clone(),
                        sender: "James User".to_owned(),
                        reply_to: None,
                        recipients: refreshed_identity.email_address.clone(),
                        subject: "Updated message".to_owned(),
                        body_preview: "Updated body preview".to_owned(),
                        body_text: Some("Updated body preview".to_owned()),
                        sanitized_html: None,
                        has_remote_content: false,
                        is_read: true,
                        is_flagged: true,
                        received_at: "2026-03-21T10:00:00Z".to_owned(),
                        attachments: Vec::new(),
                    }],
                }],
                ..Default::default()
            }],
        };
        store
            .replace_account_snapshot(account_id, &refreshed_snapshot)
            .expect("snapshot with refreshed identities should apply");

        let preview = store
            .message_preview(
                store
                    .list_threads(
                        store
                            .list_mailboxes(account_id)
                            .expect("mailboxes should load")[0]
                            .id,
                        0..1,
                    )
                    .expect("threads should load")[0]
                    .id,
            )
            .expect("preview query should work")
            .expect("preview should exist");
        assert_eq!(preview.identity_name, "James Identity");
        assert_eq!(preview.subject, "Updated message");
    }

    #[test]
    fn hydrates_reader_threads_and_searches_local_mail() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        let target = store
            .sync_target(account_id)
            .expect("target should load")
            .expect("account should exist");
        let identity_email = target.identities[0].email_address.clone();
        let snapshot = AccountSyncSnapshot {
            checkpoint: "imap:phase3".to_owned(),
            detail: "Phase 3 sync".to_owned(),
            identities: None,
            mailboxes: vec![MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: "thread-2".to_owned(),
                    subject: "Searchable incident".to_owned(),
                    participants: "Kai Santos".to_owned(),
                    preview: "Fallback preview".to_owned(),
                    is_flagged: true,
                    has_attachments: true,
                    unread_count: 1,
                    latest_message_at: "2026-03-22T12:00:00Z".to_owned(),
                    messages: vec![MessageSnapshot {
                        remote_id: "message-2".to_owned(),
                        identity_email,
                        sender: "Kai Santos".to_owned(),
                        reply_to: None,
                        recipients: target.username.clone(),
                        subject: "Searchable incident".to_owned(),
                        body_preview: "Fallback preview".to_owned(),
                        body_text: None,
                        sanitized_html: Some(
                            "<div>Status page blocked://status.zmail.test details</div>".to_owned(),
                        ),
                        has_remote_content: true,
                        is_read: false,
                        is_flagged: true,
                        received_at: "2026-03-22T12:00:00Z".to_owned(),
                        attachments: Vec::new(),
                    }],
                }],
                ..Default::default()
            }],
        };

        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");

        let mailbox_id = store
            .mailbox_id_by_role(account_id, "inbox")
            .expect("mailbox lookup should work")
            .expect("inbox mailbox should exist");
        let thread_id = store
            .first_thread_id(mailbox_id)
            .expect("thread lookup should work")
            .expect("thread should exist");

        let thread = store
            .thread_view(thread_id)
            .expect("thread view should load")
            .expect("thread should exist");
        assert!(thread.is_flagged);
        assert!(thread.has_remote_content);
        assert_eq!(thread.messages.len(), 1);
        assert_eq!(
            thread.messages[0].render_mode,
            MessageRenderMode::SanitizedHtml
        );
        assert!(thread.messages[0].rendered_body.contains("Status page"));

        let hits = store
            .search_threads(account_id, "Status details", 10)
            .expect("search should work");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subject, "Searchable incident");
        assert!(hits[0].is_flagged);
        assert!(hits[0].has_attachments);
    }

    #[test]
    fn local_operations_queue_server_write_back() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot("Op subject", "Op body", "thread-9", "message-9", None),
            )
            .expect("snapshot should apply");

        let mailbox_id = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")[0]
            .id;
        let thread_id = store
            .list_threads(mailbox_id, 0..1)
            .expect("threads should load")[0]
            .id;

        store
            .set_thread_read(thread_id, true)
            .expect("thread should be marked read");
        store
            .set_thread_flagged(thread_id, true)
            .expect("thread should be flagged");

        let ops = store
            .list_pending_ops(account_id)
            .expect("pending operations should load");
        assert_eq!(ops.len(), 2, "read and flag operations should be queued");
        assert!(ops.iter().any(|op| {
            op.kind == PendingOpKind::MarkRead
                && op.remote_id == "message-9"
                && op.source_mailbox == "Inbox"
        }));
        assert!(
            ops.iter()
                .any(|op| op.kind == PendingOpKind::SetFlagged && op.remote_id == "message-9")
        );

        // Completing one operation removes it while the other stays queued.
        store
            .complete_pending_op(ops[0].id)
            .expect("operation should complete");
        let remaining = store
            .list_pending_ops(account_id)
            .expect("pending operations should load");
        assert_eq!(remaining.len(), 1, "one operation should remain queued");

        // Failures keep the operation queued with the error recorded.
        store
            .record_pending_op_failure(remaining[0].id, "server rejected the operation")
            .expect("failure should be recorded");
        let retried = store
            .list_pending_ops(account_id)
            .expect("pending operations should load");
        assert_eq!(retried.len(), 1, "failed operations stay queued");
        assert_eq!(retried[0].attempts, 1);
        assert_eq!(
            retried[0].last_error.as_deref(),
            Some("server rejected the operation")
        );
    }

    #[test]
    fn backups_are_restorable_and_bounded() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Backup subject",
                    "Backup body",
                    "thread-11",
                    "message-11",
                    None,
                ),
            )
            .expect("snapshot should apply");

        let backup_dir =
            std::env::temp_dir().join(format!("zmail-backup-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&backup_dir);

        let first = store
            .create_backup(&backup_dir, 2)
            .expect("the first backup should be created");

        store
            .create_backup(&backup_dir, 2)
            .expect("the second backup should be created");
        store
            .create_backup(&backup_dir, 2)
            .expect("the third backup should be created");

        let mut remaining: Vec<_> = fs::read_dir(&backup_dir)
            .expect("the backup directory should list")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect();
        remaining.sort();

        assert_eq!(remaining.len(), 2, "only the two newest backups remain");
        assert!(!remaining.contains(&first), "the oldest backup is pruned");

        let restored = LocalStore::open(remaining[0].clone()).expect("the backup should reopen");
        assert_eq!(
            restored.stats().expect("stats should load").message_count,
            store.stats().expect("stats should load").message_count,
            "the backup contains the same messages"
        );

        let _ = fs::remove_dir_all(&backup_dir);
    }

    #[test]
    fn persists_and_saves_received_attachments() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        let blob = b"attachment payload bytes".to_vec();
        let target = store
            .sync_target(account_id)
            .expect("the sync target should load")
            .expect("the sync target should exist");
        let attachments_dir = std::path::PathBuf::from(&target.attachments_dir);
        std::fs::create_dir_all(&attachments_dir).expect("the attachments dir should be created");
        std::fs::write(attachments_dir.join("incoming-inbox-1-0-report.txt"), &blob)
            .expect("the blob should be written");

        let mut snapshot = merge_test_snapshot(
            "Attachment subject",
            "Attachment body",
            "thread-attach",
            "message-attach",
            None,
        );
        snapshot.mailboxes[0].threads[0].messages[0].attachments = vec![MessageAttachment {
            filename: "report.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            size_bytes: blob.len() as u64,
            storage_path: "incoming-inbox-1-0-report.txt".to_owned(),
        }];
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");

        let threads = store
            .list_threads(mailbox_id_by_name(&store, account_id, "Inbox"), 0..10)
            .expect("threads should load");
        let thread = store
            .thread_view(threads[0].id)
            .expect("the thread view should load")
            .expect("the thread view should exist");
        assert_eq!(thread.messages.len(), 1);
        assert_eq!(thread.messages[0].attachments.len(), 1);
        let attachment = &thread.messages[0].attachments[0];
        assert_eq!(attachment.filename, "report.txt");
        assert_eq!(attachment.content_type, "text/plain");
        assert_eq!(attachment.size_bytes, blob.len() as u64);
        assert_eq!(attachment.storage_path, "incoming-inbox-1-0-report.txt");

        // An identical re-merge skips the thread and does not duplicate rows.
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("the identical snapshot should apply");
        let thread = store
            .thread_view(threads[0].id)
            .expect("the thread view should load")
            .expect("the thread view should exist");
        assert_eq!(
            thread.messages[0].attachments.len(),
            1,
            "an identical re-merge keeps a single attachment row"
        );

        let destination =
            std::env::temp_dir().join(format!("zmail-attachment-save-{}", std::process::id()));
        let saved = store
            .save_attachment(attachment.id, &destination)
            .expect("the attachment should save");
        assert_eq!(saved, destination.join("report.txt"));
        let written = std::fs::read(&saved).expect("the saved blob should read back");
        assert_eq!(written, blob, "save-as writes the original bytes");
        let _ = std::fs::remove_dir_all(&destination);
    }

    /// Returns the local id of an account mailbox by name.
    fn mailbox_id_by_name(store: &LocalStore, account_id: i64, name: &str) -> i64 {
        store
            .list_mailboxes(account_id)
            .expect("mailboxes should load")
            .into_iter()
            .find(|mailbox| mailbox.name == name)
            .map(|mailbox| mailbox.id)
            .expect("the mailbox should exist")
    }

    #[test]
    fn hydrated_bodies_survive_header_only_resyncs() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        // Header-only snapshot, as the lazy IMAP adapter sends it.
        let mut snapshot = merge_test_snapshot(
            "Lazy subject",
            "Lazy body",
            "thread-lazy",
            "message-lazy",
            None,
        );
        snapshot.mailboxes[0].threads[0].messages[0].body_text = None;
        snapshot.mailboxes[0].threads[0].messages[0].sanitized_html = None;
        snapshot.mailboxes[0].threads[0].messages[0].body_preview =
            "Body not downloaded.".to_owned();
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");

        let mailbox_id = mailbox_id_by_name(&store, account_id, "Inbox");
        let threads = store
            .list_threads(mailbox_id, 0..10)
            .expect("threads should load");
        let view = store
            .thread_view(threads[0].id)
            .expect("the thread view should load")
            .expect("the thread view should exist");
        assert_eq!(view.messages[0].render_mode, MessageRenderMode::PreviewOnly);
        let missing = store
            .messages_missing_bodies(threads[0].id)
            .expect("missing bodies should load")
            .expect("the thread should exist");
        assert_eq!(missing.len(), 1);

        store
            .hydrate_message_body(
                missing[0].0,
                &HydratedBody {
                    body_text: Some("Lazy body".to_owned()),
                    sanitized_html: None,
                    has_remote_content: false,
                    body_preview: "Lazy body".to_owned(),
                    attachments: vec![MessageAttachment {
                        filename: "notes.txt".to_owned(),
                        content_type: "text/plain".to_owned(),
                        size_bytes: 6,
                        storage_path: "incoming-inbox-1-0-notes.txt".to_owned(),
                    }],
                },
            )
            .expect("hydration should persist");

        let view = store
            .thread_view(threads[0].id)
            .expect("the thread view should load")
            .expect("the thread view should exist");
        assert_eq!(view.messages[0].render_mode, MessageRenderMode::PlainText);
        assert_eq!(view.messages[0].attachments.len(), 1);
        assert!(
            view.has_attachments,
            "hydration sets the thread attachment flag"
        );
        assert!(
            store
                .messages_missing_bodies(threads[0].id)
                .expect("missing bodies should load")
                .expect("the thread should exist")
                .is_empty(),
            "hydration drains the missing list"
        );

        // The next header-only sync must not wipe the hydrated state.
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("the header-only snapshot should re-apply");
        let view = store
            .thread_view(threads[0].id)
            .expect("the thread view should load")
            .expect("the thread view should exist");
        assert_eq!(
            view.messages[0].render_mode,
            MessageRenderMode::PlainText,
            "syncs must preserve hydrated bodies"
        );
        assert_eq!(
            view.messages[0].attachments.len(),
            1,
            "syncs must preserve hydrated attachment metadata"
        );
        assert!(
            view.has_attachments,
            "syncs must preserve the thread attachment flag"
        );
        assert_eq!(
            view.messages[0].body_preview, "Lazy body",
            "syncs must keep the hydrated preview"
        );
    }

    #[test]
    fn hydrated_bodies_become_searchable() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        // Header-only snapshot, as the lazy IMAP adapter sends it: the
        // searchable body is the placeholder, so the real body text cannot
        // match yet.
        let mut snapshot = merge_test_snapshot(
            "Searchable subject",
            "The lazy cormorant body",
            "thread-searchable",
            "message-searchable",
            None,
        );
        snapshot.mailboxes[0].threads[0].messages[0].body_text = None;
        snapshot.mailboxes[0].threads[0].messages[0].sanitized_html = None;
        snapshot.mailboxes[0].threads[0].messages[0].body_preview =
            "Body not downloaded.".to_owned();
        snapshot.mailboxes[0].threads[0].preview = "Body not downloaded.".to_owned();
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");

        assert!(
            store
                .search_threads(account_id, "cormorant", 10)
                .expect("search should run")
                .is_empty(),
            "a body that was never fetched cannot match"
        );

        let mailbox_id = mailbox_id_by_name(&store, account_id, "Inbox");
        let threads = store
            .list_threads(mailbox_id, 0..10)
            .expect("threads should load");
        let missing = store
            .messages_missing_bodies(threads[0].id)
            .expect("missing bodies should load")
            .expect("the thread should exist");
        assert_eq!(missing.len(), 1);

        store
            .hydrate_message_body(
                missing[0].0,
                &HydratedBody {
                    body_text: Some("The lazy cormorant body".to_owned()),
                    sanitized_html: None,
                    has_remote_content: false,
                    body_preview: "The lazy cormorant body".to_owned(),
                    attachments: Vec::new(),
                },
            )
            .expect("hydration should persist");

        let hits = store
            .search_threads(account_id, "cormorant", 10)
            .expect("search should run");
        assert_eq!(hits.len(), 1, "the hydrated body must match");
        assert_eq!(hits[0].subject, "Searchable subject");

        // A repeated hydration must not duplicate the document row.
        store
            .hydrate_message_body(
                missing[0].0,
                &HydratedBody {
                    body_text: Some("The lazy cormorant body".to_owned()),
                    sanitized_html: None,
                    has_remote_content: false,
                    body_preview: "The lazy cormorant body".to_owned(),
                    attachments: Vec::new(),
                },
            )
            .expect("repeated hydration should persist");
        let hits = store
            .search_threads(account_id, "cormorant", 10)
            .expect("search should run");
        assert_eq!(hits.len(), 1, "re-hydration keeps a single search document");
    }

    #[test]
    fn mbox_export_writes_message_entries() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot(
                    "Export subject",
                    "Export body",
                    "thread-12",
                    "message-12",
                    None,
                ),
            )
            .expect("snapshot should apply");

        let mut buffer = Vec::new();
        let exported = store
            .export_account_mbox(account_id, &mut buffer)
            .expect("the export should succeed");
        assert_eq!(exported, 1, "one message should be exported");

        let mbox = String::from_utf8(buffer).expect("the export should be UTF-8");
        assert!(
            mbox.starts_with("From "),
            "mbox entries start with a From line"
        );
        assert!(mbox.contains("Subject: Export subject"));
        assert!(mbox.contains("Export body"));
        assert!(
            mbox.ends_with("\n\n"),
            "entries end with a blank separator line"
        );
    }

    #[test]
    fn move_and_delete_queue_server_write_back() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        store
            .replace_account_snapshot(
                account_id,
                &merge_test_snapshot("Move subject", "Move body", "thread-10", "message-10", None),
            )
            .expect("snapshot should apply");

        let mailboxes = store
            .list_mailboxes(account_id)
            .expect("mailboxes should load");
        let inbox_id = mailboxes[0].id;
        let thread_id = store
            .list_threads(inbox_id, 0..1)
            .expect("threads should load")[0]
            .id;

        store
            .move_thread(thread_id, inbox_id)
            .expect("same-mailbox move should be a no-op");
        assert!(
            store
                .list_pending_ops(account_id)
                .expect("pending operations should load")
                .is_empty(),
            "a no-op move should not queue anything"
        );

        store
            .delete_thread(thread_id)
            .expect("thread should be deleted");
        let ops = store
            .list_pending_ops(account_id)
            .expect("pending operations should load");
        // The demo account has no trash mailbox, so the destroy operation
        // mirrors the locally removed rows.
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, PendingOpKind::Destroy);
        assert_eq!(ops[0].remote_id, "message-10");
    }

    /// An IMAP account with Inbox (one thread of two messages, the first
    /// read), Archive and Trash. Returns the store, the account and the
    /// thread.
    fn write_back_fixture(protocol: ProtocolKind) -> (LocalStore, i64, i64) {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(protocol)
            .expect("account should be created");
        let message = |remote_id: &str, is_read: bool, at: &str| MessageSnapshot {
            remote_id: remote_id.to_owned(),
            identity_email: "phase2@localhost".to_owned(),
            sender: "Kai Santos".to_owned(),
            reply_to: None,
            recipients: "phase2@localhost".to_owned(),
            subject: "Undo subject".to_owned(),
            body_preview: "Undo body".to_owned(),
            body_text: Some("Undo body".to_owned()),
            sanitized_html: None,
            has_remote_content: false,
            is_read,
            is_flagged: false,
            received_at: at.to_owned(),
            attachments: Vec::new(),
        };
        let mut snapshot =
            merge_test_snapshot("Undo subject", "Undo body", "thread-u", "unused", None);
        snapshot.mailboxes[0].threads[0].messages = vec![
            message("11", true, "2026-03-21T09:15:00Z"),
            message("12", false, "2026-03-21T10:15:00Z"),
        ];
        for (name, role) in [("Archive", "archive"), ("Trash", "trash")] {
            snapshot.mailboxes.push(MailboxSnapshot {
                name: name.to_owned(),
                role: Some(role.to_owned()),
                threads: Vec::new(),
                ..Default::default()
            });
        }
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");
        let inbox_id = store
            .mailbox_id_by_role(account_id, "inbox")
            .expect("lookup works")
            .expect("inbox exists");
        let thread_id = store
            .first_thread_id(inbox_id)
            .expect("lookup works")
            .expect("thread exists");
        (store, account_id, thread_id)
    }

    fn role_id(store: &LocalStore, account_id: i64, role: &str) -> i64 {
        store
            .mailbox_id_by_role(account_id, role)
            .expect("the mailbox lookup works")
            .expect("the mailbox exists")
    }

    /// The queued operations as (kind, source, remote id, target).
    fn queued(
        store: &LocalStore,
        account_id: i64,
    ) -> Vec<(PendingOpKind, String, String, Option<String>)> {
        store
            .list_pending_ops(account_id)
            .expect("pending operations should load")
            .into_iter()
            .map(|op| (op.kind, op.source_mailbox, op.remote_id, op.target_mailbox))
            .collect()
    }

    #[test]
    fn moving_back_before_write_back_cancels_the_queued_move() {
        let (store, account_id, thread_id) = write_back_fixture(ProtocolKind::Imap);
        let inbox_id = role_id(&store, account_id, "inbox");
        let archive_id = role_id(&store, account_id, "archive");

        // Delete into the trash, then undo it before any sync: nothing is
        // left for the server, in particular no delete.
        store.delete_thread(thread_id).expect("delete works");
        assert_eq!(queued(&store, account_id).len(), 2, "one move per message");
        store
            .move_thread(thread_id, inbox_id)
            .expect("moving back works");
        assert_eq!(queued(&store, account_id), Vec::new());

        // Archive, then move on to the trash: one move per message, from
        // where the server has it to where it ends up.
        store.archive_thread(thread_id).expect("archive works");
        store.delete_thread(thread_id).expect("delete works");
        let trash = Some("Trash".to_owned());
        assert_eq!(
            queued(&store, account_id),
            vec![
                (
                    PendingOpKind::Move,
                    "Inbox".to_owned(),
                    "11".to_owned(),
                    trash.clone()
                ),
                (
                    PendingOpKind::Move,
                    "Inbox".to_owned(),
                    "12".to_owned(),
                    trash
                ),
            ]
        );
        // And back through Archive to the Inbox.
        store
            .move_thread(thread_id, archive_id)
            .expect("move works");
        store.move_thread(thread_id, inbox_id).expect("move works");
        assert_eq!(queued(&store, account_id), Vec::new());
    }

    #[test]
    fn changes_to_a_moved_message_are_addressed_where_the_server_has_it() {
        let (store, account_id, thread_id) = write_back_fixture(ProtocolKind::Imap);

        store.archive_thread(thread_id).expect("archive works");
        store
            .set_thread_flagged(thread_id, true)
            .expect("flag works");
        store
            .set_thread_flagged(thread_id, false)
            .expect("unflag works");
        // The flag changes run in the Inbox, where the uids are valid, before
        // the move; the later one replaced the earlier.
        let archive = Some("Archive".to_owned());
        assert_eq!(
            queued(&store, account_id),
            vec![
                (
                    PendingOpKind::ClearFlagged,
                    "Inbox".to_owned(),
                    "11".to_owned(),
                    None
                ),
                (
                    PendingOpKind::Move,
                    "Inbox".to_owned(),
                    "11".to_owned(),
                    archive.clone()
                ),
                (
                    PendingOpKind::ClearFlagged,
                    "Inbox".to_owned(),
                    "12".to_owned(),
                    None
                ),
                (
                    PendingOpKind::Move,
                    "Inbox".to_owned(),
                    "12".to_owned(),
                    archive
                ),
            ]
        );

        // Deleting it from the archive for good destroys it in the Inbox.
        let conn = store.connection().expect("connection opens");
        conn.execute("DELETE FROM mailboxes WHERE name = 'Trash'", [])
            .expect("the trash goes");
        drop(conn);
        store.delete_thread(thread_id).expect("delete works");
        let ops = queued(&store, account_id);
        assert!(
            ops.iter()
                .all(|(kind, source, _, _)| *kind != PendingOpKind::Move && source == "Inbox"),
            "{ops:?}"
        );
        assert_eq!(
            ops.iter()
                .filter(|(kind, _, _, _)| *kind == PendingOpKind::Destroy)
                .count(),
            2
        );
    }

    #[test]
    fn restoring_message_states_puts_back_a_partly_read_thread() {
        let (store, account_id, thread_id) = write_back_fixture(ProtocolKind::Imap);
        let inbox_id = role_id(&store, account_id, "inbox");
        let before = store
            .thread_state(thread_id)
            .expect("state loads")
            .expect("thread exists");
        assert_eq!(before.mailbox_name, "Inbox");
        assert!(!before.is_read());
        assert_eq!(
            before
                .messages
                .iter()
                .map(|message| message.is_read)
                .collect::<Vec<_>>(),
            [true, false]
        );

        store
            .set_thread_read(thread_id, true)
            .expect("mark read works");
        store
            .set_thread_flagged(thread_id, true)
            .expect("flag works");
        store
            .restore_message_flag_states(thread_id, &before.messages)
            .expect("restore works");

        assert_eq!(
            store.thread_state(thread_id).expect("state loads"),
            Some(before)
        );
        let summary = &store.list_threads(inbox_id, 0..1).expect("threads load")[0];
        assert_eq!(summary.unread_count, 1);
        assert!(!summary.is_flagged);
        let inbox = store
            .list_mailboxes(account_id)
            .expect("mailboxes load")
            .into_iter()
            .find(|mailbox| mailbox.id == inbox_id)
            .expect("inbox exists");
        assert_eq!(inbox.unread_threads, 1);
        // Each message keeps one, last-wins, operation of each kind; the
        // already read first message keeps the harmless Mark Read.
        assert_eq!(
            queued(&store, account_id),
            vec![
                (
                    PendingOpKind::MarkRead,
                    "Inbox".to_owned(),
                    "11".to_owned(),
                    None
                ),
                (
                    PendingOpKind::ClearFlagged,
                    "Inbox".to_owned(),
                    "11".to_owned(),
                    None
                ),
                (
                    PendingOpKind::MarkUnread,
                    "Inbox".to_owned(),
                    "12".to_owned(),
                    None
                ),
                (
                    PendingOpKind::ClearFlagged,
                    "Inbox".to_owned(),
                    "12".to_owned(),
                    None
                ),
            ]
        );
    }

    #[test]
    fn a_completed_imap_move_retires_the_old_uid_until_the_next_snapshot() {
        for protocol in [ProtocolKind::Imap, ProtocolKind::Jmap] {
            let (store, account_id, thread_id) = write_back_fixture(protocol);
            store.archive_thread(thread_id).expect("archive works");
            for op in store
                .list_pending_ops(account_id)
                .expect("pending operations should load")
            {
                store.complete_pending_op(op.id).expect("completion works");
            }
            // Moving it back now must not address uid 11 or 12 in the
            // Archive: there they may be other messages.
            let inbox_id = role_id(&store, account_id, "inbox");
            store.move_thread(thread_id, inbox_id).expect("move works");
            let ops = queued(&store, account_id);
            match protocol {
                ProtocolKind::Imap => assert_eq!(ops, Vec::new()),
                // JMAP ids name the message wherever it is.
                ProtocolKind::Jmap => assert_eq!(ops.len(), 2, "{ops:?}"),
            }
        }
    }

    #[test]
    fn applies_local_mailbox_operations_and_updates_counts() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");

        let target = store
            .sync_target(account_id)
            .expect("target should load")
            .expect("account should exist");
        let identity_email = target.identities[0].email_address.clone();
        let snapshot = AccountSyncSnapshot {
            checkpoint: "imap:ops".to_owned(),
            detail: "Mailbox operations seed".to_owned(),
            identities: None,
            mailboxes: vec![
                MailboxSnapshot {
                    name: "Inbox".to_owned(),
                    role: Some("inbox".to_owned()),
                    threads: vec![ThreadSnapshot {
                        remote_thread_id: "thread-3".to_owned(),
                        subject: "Inbox task".to_owned(),
                        participants: "Ops".to_owned(),
                        preview: "Handle mailbox ops".to_owned(),
                        is_flagged: false,
                        has_attachments: false,
                        unread_count: 1,
                        latest_message_at: "2026-03-22T13:00:00Z".to_owned(),
                        messages: vec![MessageSnapshot {
                            remote_id: "message-3".to_owned(),
                            identity_email: identity_email.clone(),
                            sender: "Ops".to_owned(),
                            reply_to: None,
                            recipients: target.username.clone(),
                            subject: "Inbox task".to_owned(),
                            body_preview: "Handle mailbox ops".to_owned(),
                            body_text: Some("Handle mailbox ops".to_owned()),
                            sanitized_html: None,
                            has_remote_content: false,
                            is_read: false,
                            is_flagged: false,
                            received_at: "2026-03-22T13:00:00Z".to_owned(),
                            attachments: Vec::new(),
                        }],
                    }],
                    ..Default::default()
                },
                MailboxSnapshot {
                    name: "Archive".to_owned(),
                    role: Some("archive".to_owned()),
                    threads: Vec::new(),
                    ..Default::default()
                },
                MailboxSnapshot {
                    name: "Trash".to_owned(),
                    role: Some("trash".to_owned()),
                    threads: Vec::new(),
                    ..Default::default()
                },
            ],
        };

        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("snapshot should apply");

        let inbox_id = store
            .mailbox_id_by_role(account_id, "inbox")
            .expect("mailbox lookup should work")
            .expect("inbox should exist");
        let archive_id = store
            .mailbox_id_by_role(account_id, "archive")
            .expect("archive lookup should work")
            .expect("archive should exist");
        let trash_id = store
            .mailbox_id_by_role(account_id, "trash")
            .expect("trash lookup should work")
            .expect("trash should exist");
        let thread_id = store
            .first_thread_id(inbox_id)
            .expect("thread lookup should work")
            .expect("thread should exist");

        store
            .set_thread_flagged(thread_id, true)
            .expect("flagging should work");
        store
            .set_thread_read(thread_id, true)
            .expect("marking read should work");
        let thread = store
            .thread_view(thread_id)
            .expect("thread view should load")
            .expect("thread should exist");
        assert!(thread.is_flagged);
        assert_eq!(thread.unread_count, 0);

        store
            .move_thread(thread_id, archive_id)
            .expect("moving should work");
        let archive_threads = store
            .list_threads(archive_id, 0..10)
            .expect("archive threads should load");
        assert_eq!(archive_threads.len(), 1);
        assert_eq!(
            store
                .list_threads(inbox_id, 0..10)
                .expect("inbox threads")
                .len(),
            0
        );

        store
            .delete_thread(thread_id)
            .expect("delete should move the thread to trash");
        assert_eq!(
            store
                .list_threads(archive_id, 0..10)
                .expect("archive threads")
                .len(),
            0
        );
        assert_eq!(
            store
                .list_threads(trash_id, 0..10)
                .expect("trash threads")
                .len(),
            1
        );

        store
            .delete_thread(thread_id)
            .expect("deleting from trash should remove the thread");
        assert_eq!(
            store
                .list_threads(trash_id, 0..10)
                .expect("trash threads")
                .len(),
            0
        );
        assert!(
            store
                .search_threads(account_id, "mailbox ops", 10)
                .expect("search should work")
                .is_empty()
        );
    }

    /// A write that starts while another connection (the sync runtime,
    /// say) holds the write lock waits for it instead of failing with
    /// "database is locked". A deferred transaction would read first, and
    /// its later upgrade to a write fails at once once the other writer
    /// has committed: the busy timeout does not apply to that.
    #[test]
    fn writes_wait_for_a_concurrent_writer() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let identity = store
            .list_identities(account_id)
            .expect("identities should load")
            .into_iter()
            .next()
            .expect("demo account should have an identity");

        let other = rusqlite::Connection::open(store.path()).expect("second connection opens");
        other
            .busy_timeout(Duration::from_secs(5))
            .expect("busy timeout set");
        other
            .execute_batch("BEGIN IMMEDIATE; UPDATE accounts SET display_name = display_name;")
            .expect("the other connection takes the write lock");

        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                store.create_draft(&NewDraft {
                    account_id,
                    identity_id: identity.id,
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: "Concurrent".to_owned(),
                    body: String::new(),
                })
            });
            // Let the draft write start and block on the lock.
            std::thread::sleep(Duration::from_millis(300));
            other
                .execute_batch("COMMIT")
                .expect("the other writer commits");
            writer
                .join()
                .expect("the writer thread finishes")
                .expect("the draft is created once the lock is free");
        });
    }

    #[test]
    fn creates_autosaves_and_attaches_to_drafts() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let identity = store
            .list_identities(account_id)
            .expect("identities should load")
            .into_iter()
            .next()
            .expect("demo account should have an identity");

        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id: identity.id,
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: String::new(),
                body: String::new(),
            })
            .expect("draft should be created");

        let initial = store
            .draft_detail(draft_id)
            .expect("draft detail should load")
            .expect("draft should exist");
        assert_eq!(initial.body, identity.signature);

        let attachment_source =
            std::env::temp_dir().join(format!("zmail-draft-attachment-{}.txt", draft_id));
        fs::write(&attachment_source, b"attachment payload").expect("attachment should be written");
        let attachment = store
            .add_draft_attachment(draft_id, &attachment_source)
            .expect("attachment should be added");

        store
            .update_draft(
                draft_id,
                &zmail_core::DraftUpdate {
                    identity_id: identity.id,
                    to: vec!["recipient@example.test".to_owned()],
                    cc: vec!["copy@example.test".to_owned()],
                    bcc: Vec::new(),
                    subject: "Phase 4".to_owned(),
                    body: "Draft body".to_owned(),
                },
            )
            .expect("draft should autosave");

        let updated = store
            .draft_detail(draft_id)
            .expect("draft detail should load")
            .expect("draft should exist");
        assert_eq!(updated.subject, "Phase 4");
        assert_eq!(updated.body, "Draft body");
        assert_eq!(updated.to, vec!["recipient@example.test".to_owned()]);
        assert_eq!(updated.cc, vec!["copy@example.test".to_owned()]);
        assert_eq!(updated.attachments.len(), 1);
        assert_eq!(updated.attachments[0].id, attachment.id);

        store
            .remove_draft_attachment(attachment.id)
            .expect("attachment should be removable");
        let without_attachment = store
            .draft_detail(draft_id)
            .expect("draft detail should load")
            .expect("draft should exist");
        assert!(without_attachment.attachments.is_empty());
    }

    #[test]
    fn queues_retries_and_completes_outbox_jobs() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let identity = store
            .list_identities(account_id)
            .expect("identities should load")
            .into_iter()
            .next()
            .expect("demo account should have an identity");

        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id: identity.id,
                to: vec!["recipient@example.test".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Queued draft".to_owned(),
                body: "Queued body".to_owned(),
            })
            .expect("draft should be created");
        let job_id = store.queue_draft(draft_id).expect("draft should queue");

        let jobs = store
            .list_outbox_jobs(Some(account_id))
            .expect("jobs should load");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, job_id);

        let claimed = store
            .claim_due_outbox_job(Some(account_id))
            .expect("job should claim")
            .expect("job should exist");
        assert_eq!(claimed.job_id, job_id);
        assert_eq!(claimed.message.subject, "Queued draft");
        assert_eq!(claimed.target.auth, AuthKind::Password);

        store
            .record_outbox_retry(job_id, "offline", Duration::from_secs(30))
            .expect("retry should record");
        let retried = store
            .draft_detail(draft_id)
            .expect("draft detail should load")
            .expect("draft should exist");
        assert_eq!(retried.state, DraftState::Queued);
        assert_eq!(retried.last_error_message.as_deref(), Some("offline"));

        store
            .retry_outbox_job(job_id)
            .expect("job should requeue manually");
        let claimed_again = store
            .claim_due_outbox_job(Some(account_id))
            .expect("job should claim again")
            .expect("job should still exist");
        assert_eq!(claimed_again.job_id, job_id);

        store
            .record_outbox_sent(job_id)
            .expect("success should record");
        let sent = store
            .draft_detail(draft_id)
            .expect("draft detail should load")
            .expect("draft should exist");
        assert_eq!(sent.state, DraftState::Sent);
        assert!(sent.sent_at.is_some());
    }

    #[test]
    fn undo_send_holds_the_job_until_its_time_and_can_take_it_back() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        let identity_id = store.list_identities(account_id).expect("identities load")[0].id;
        let draft = |subject: &str| {
            store
                .create_draft(&NewDraft {
                    account_id,
                    identity_id,
                    to: vec!["recipient@example.test".to_owned()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: subject.to_owned(),
                    body: "Body".to_owned(),
                })
                .expect("draft should be created")
        };
        let state = |draft_id| {
            store
                .draft_detail(draft_id)
                .expect("draft loads")
                .expect("draft exists")
                .state
        };

        // Queued ten seconds ahead: due no sooner than that, and not
        // claimable before.
        let draft_id = draft("Undo me");
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let job_id = store
            .queue_draft_after(draft_id, Duration::from_secs(10))
            .expect("draft should queue");
        let job = &store.list_outbox_jobs(Some(account_id)).expect("jobs load")[0];
        let due =
            zmail_core::time::parse_timestamp(job.next_attempt_at.as_deref().expect("a due time"))
                .expect("the due time parses")
                .timestamp() as u64;
        assert!(due >= before + 10, "due {due}, queued at {before}");
        assert_eq!(state(draft_id), DraftState::Queued);
        assert!(
            store
                .claim_due_outbox_job(Some(account_id))
                .expect("claim works")
                .is_none(),
            "nothing is sent inside the undo window"
        );

        // Undo: the job goes and the draft can be edited again.
        assert!(
            store
                .cancel_queued_outbox_job(job_id)
                .expect("cancel works")
        );
        assert!(
            store
                .list_outbox_jobs(Some(account_id))
                .expect("jobs load")
                .is_empty()
        );
        assert_eq!(state(draft_id), DraftState::Editing);
        store
            .update_draft(
                draft_id,
                &zmail_core::DraftUpdate {
                    identity_id,
                    to: vec!["recipient@example.test".to_owned()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: "Undo me, edited".to_owned(),
                    body: "Body".to_owned(),
                },
            )
            .expect("the draft is editable again");
        // Cancelling twice is too late, not an error.
        assert!(
            !store
                .cancel_queued_outbox_job(job_id)
                .expect("cancel works")
        );

        // Once the job is claimed it is too late to take back.
        let job_id = store
            .queue_draft_after(draft_id, Duration::from_secs(10))
            .expect("draft should queue again");
        // (Retry makes a job due now, as the window ending would.)
        store.retry_outbox_job(job_id).expect("job becomes due");
        let claimed = store
            .claim_due_outbox_job(Some(account_id))
            .expect("claim works")
            .expect("the due job is claimed");
        assert_eq!(claimed.job_id, job_id);
        assert!(
            !store
                .cancel_queued_outbox_job(job_id)
                .expect("cancel works")
        );
        assert_eq!(state(draft_id), DraftState::Sending);
        store.record_outbox_sent(job_id).expect("sent");
        assert!(
            !store
                .cancel_queued_outbox_job(job_id)
                .expect("cancel works")
        );
        assert_eq!(state(draft_id), DraftState::Sent);
    }

    #[test]
    fn outbox_jobs_carry_the_account_auth_kind() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "OAuth2 Sender".to_owned(),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::OAuth2,
                endpoint: "imaps://imap.gmail.com:993".to_owned(),
                username: "user@gmail.example".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: None,
                identities: vec![NewIdentity {
                    display_name: "OAuth2 Sender".to_owned(),
                    email_address: "user@gmail.example".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account should be inserted");
        let identity = store
            .list_identities(account_id)
            .expect("identities should load")
            .into_iter()
            .next()
            .expect("the account should have an identity");

        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id: identity.id,
                to: vec!["recipient@example.test".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "OAuth2 draft".to_owned(),
                body: "OAuth2 body".to_owned(),
            })
            .expect("draft should be created");
        store.queue_draft(draft_id).expect("draft should queue");

        let claimed = store
            .claim_due_outbox_job(Some(account_id))
            .expect("job should claim")
            .expect("job should exist");
        // The claimed target tells the SMTP adapter to use AUTH XOAUTH2
        // with the account's refreshed access token.
        assert_eq!(claimed.target.auth, AuthKind::OAuth2);
    }

    #[test]
    fn ui_preferences_default_until_saved() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        assert_eq!(
            store.ui_preferences().expect("preferences should load"),
            zmail_core::preferences::UiPreferences::default()
        );
    }

    #[test]
    fn ui_preferences_round_trip_through_the_store() {
        use zmail_core::preferences::{
            Density, KeymapName, MotionMode, PaneWidths, ReadingPanePosition, SenderWarnings,
            ThemeMode, UiPreferences,
        };
        use zmail_core::{MailboxSelection, VirtualMailbox};

        let store = LocalStore::open_ephemeral().expect("store should open");
        let preferences = UiPreferences {
            theme_mode: ThemeMode::Light,
            density: Density::Compact,
            reading_pane: ReadingPanePosition::Off,
            pane_widths: PaneWidths {
                sidebar: Some(220.0),
                message_list: None,
            },
            sidebar_collapsed: true,
            sidebar_tree: Default::default(),
            last_mailbox: Some(MailboxSelection::Virtual(VirtualMailbox::Flagged)),
            keymap: KeymapName::Gmail,
            undo_send_delay_secs: 5,
            indexing: Default::default(),
            recent_commands: vec!["virtual:flagged".to_owned()],
            sender_warnings: SenderWarnings::Fewer,
            motion: MotionMode::Full,
        };
        store
            .save_ui_preferences(&preferences)
            .expect("preferences should save");
        // A second handle on the same file sees the saved record.
        let reopened = LocalStore::open(store.path()).expect("store should reopen");
        assert_eq!(
            reopened.ui_preferences().expect("preferences should load"),
            preferences
        );

        let updated = store
            .update_ui_preferences(|preferences| preferences.theme_mode = ThemeMode::Dark)
            .expect("preferences should update");
        assert_eq!(updated.theme_mode, ThemeMode::Dark);
        assert_eq!(updated.density, Density::Compact, "other fields are kept");
        assert_eq!(store.ui_preferences().expect("load"), updated);
    }

    #[test]
    fn ui_preferences_from_an_older_record_load_with_defaults() {
        use zmail_core::preferences::{ThemeMode, UiPreferences};

        let store = LocalStore::open_ephemeral().expect("store should open");
        // A record written before most preference fields existed.
        let conn = store.connection().expect("connection");
        conn.execute(
            "INSERT INTO app_settings(key, value) VALUES ('ui_preferences', '{\"theme_mode\":\"dark\"}')",
            [],
        )
        .expect("legacy record should insert");
        assert_eq!(
            store.ui_preferences().expect("preferences should load"),
            UiPreferences {
                theme_mode: ThemeMode::Dark,
                ..UiPreferences::default()
            }
        );
    }
}
