//! Shared domain types used across the Zmail workspace.

use std::fmt;

pub mod key_change;
pub mod participants;
pub mod preferences;
pub mod reauth;
pub mod search;
pub mod text;
pub mod time;

/// Static application metadata.
pub const APP_NAME: &str = "Zmail";

/// Human-readable milestone label exposed by the desktop shell.
pub const PHASE_LABEL: &str = "Phase 5";

/// Runtime facts shown by the shell while protocol work runs in the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSummary {
    /// Number of Tokio worker threads configured for the dedicated background runtime.
    pub worker_threads: usize,
    /// Whether a background task probe completed successfully during startup.
    pub bridge_verified: bool,
}

/// Light-or-dark appearance for a bundled UI theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeAppearance {
    /// Light theme variant.
    Light,
    /// Dark theme variant.
    Dark,
}

/// Theme facts shown by the shell while the real theming engine is still growing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSummary {
    /// Theme family name from the unchanged Zed-format theme file.
    pub family_name: String,
    /// Selected theme name within the family.
    pub theme_name: String,
    /// Light or dark appearance of the active theme.
    pub appearance: ThemeAppearance,
    /// App background color.
    pub background: Option<String>,
    /// Surface color for cards and panes.
    pub surface: Option<String>,
    /// Border color.
    pub border: Option<String>,
    /// Primary text color.
    pub foreground: Option<String>,
    /// Muted / secondary text color.
    pub muted_foreground: Option<String>,
    /// Accent color.
    pub accent: Option<String>,
    /// Selected item background color.
    pub selected_background: Option<String>,
    /// Hovered item background color.
    pub hover_background: Option<String>,
    /// Danger / error semantic color.
    pub danger: Option<String>,
    /// Warning semantic color.
    pub warning: Option<String>,
    /// Success semantic color.
    pub success: Option<String>,
    /// Info semantic color.
    pub info: Option<String>,
    /// Button / interactive element background color.
    pub button_background: Option<String>,
    /// Error banner background color.
    pub danger_background: Option<String>,
    /// Warning banner background color.
    pub warning_background: Option<String>,
    /// Success banner background color.
    pub success_background: Option<String>,
}

/// Local cache policy recorded for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePolicySummary {
    /// Human-readable policy name.
    pub name: String,
    /// Short description of the policy.
    pub description: String,
}

impl CachePolicySummary {
    /// Returns the default performance-first cache policy from the plan.
    pub fn performance_first() -> Self {
        Self {
            name: "Performance First".to_owned(),
            description:
                "Full metadata mirror, recent-body prefetch, older bodies and attachments lazy."
                    .to_owned(),
        }
    }
}

/// Supported account protocol families for MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolKind {
    /// Classic IMAP metadata and message sync.
    Imap,
    /// Standards-based JMAP sync.
    Jmap,
    /// Proton Mail's API (ADR 0004). Not offered in the UI until the local
    /// store is encrypted at rest (#139).
    Proton,
}

impl ProtocolKind {
    /// Stable storage name for the protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Imap => "imap",
            Self::Jmap => "jmap",
            Self::Proton => "proton",
        }
    }

    /// Human-facing label for the protocol.
    pub fn label(self) -> &'static str {
        match self {
            Self::Imap => "IMAP",
            Self::Jmap => "JMAP",
            Self::Proton => "Proton Mail",
        }
    }

    /// Parses the protocol from the persisted storage name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "imap" => Some(Self::Imap),
            "jmap" => Some(Self::Jmap),
            "proton" => Some(Self::Proton),
            _ => None,
        }
    }
}

impl fmt::Display for ProtocolKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Coarse-grained sync state exposed to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncPhase {
    /// No active job is running.
    Idle,
    /// Establishing a connection or session.
    Connecting,
    /// Discovering capabilities, identities, or mailbox structure.
    Discovering,
    /// Pulling or reconciling mailbox state.
    Syncing,
    /// Waiting before an automatic retry.
    Backoff,
    /// Sync failed and manual or scheduled retry is required.
    Failed,
}

impl SyncPhase {
    /// Stable storage name for the phase.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Discovering => "discovering",
            Self::Syncing => "syncing",
            Self::Backoff => "backoff",
            Self::Failed => "failed",
        }
    }

    /// Human-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Connecting => "Connecting",
            Self::Discovering => "Discovering",
            Self::Syncing => "Syncing",
            Self::Backoff => "Backoff",
            Self::Failed => "Failed",
        }
    }

    /// Parses the phase from persisted storage.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "connecting" => Some(Self::Connecting),
            "discovering" => Some(Self::Discovering),
            "syncing" => Some(Self::Syncing),
            "backoff" => Some(Self::Backoff),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

impl fmt::Display for SyncPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Identity metadata shown in account setup and message previews.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentitySummary {
    /// Identity identifier.
    pub id: i64,
    /// Human display name.
    pub display_name: String,
    /// Email address used by the identity.
    pub email_address: String,
    /// Signature block appended to new drafts by default.
    pub signature: String,
    /// Whether this is the primary identity for the account.
    pub is_primary: bool,
}

/// Outgoing transport family used when sending mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubmissionKind {
    /// Send using SMTP submission.
    Smtp,
    /// Send using JMAP `EmailSubmission`.
    Jmap,
    /// Send through Proton's API (not implemented yet; #237).
    Proton,
}

impl SubmissionKind {
    /// Stable storage name for the submission kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smtp => "smtp",
            Self::Jmap => "jmap",
            Self::Proton => "proton",
        }
    }

    /// Human-facing label for the submission kind.
    pub fn label(self) -> &'static str {
        match self {
            Self::Smtp => "SMTP",
            Self::Jmap => "JMAP",
            Self::Proton => "Proton Mail",
        }
    }

    /// Parses the submission kind from persisted storage.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "smtp" => Some(Self::Smtp),
            "jmap" => Some(Self::Jmap),
            "proton" => Some(Self::Proton),
            _ => None,
        }
    }
}

impl fmt::Display for SubmissionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// SMTP transport security mode for outgoing delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubmissionSecurity {
    /// Cleartext transport, intended for local harnesses or explicitly configured relays.
    Plain,
    /// STARTTLS-upgraded submission.
    StartTls,
    /// Submission wrapped in TLS from connect time.
    ImplicitTls,
}

impl SubmissionSecurity {
    /// Stable storage name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::StartTls => "starttls",
            Self::ImplicitTls => "implicit_tls",
        }
    }

    /// Human-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "Plain",
            Self::StartTls => "STARTTLS",
            Self::ImplicitTls => "Implicit TLS",
        }
    }

    /// Parses a persisted transport security mode.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "plain" => Some(Self::Plain),
            "starttls" => Some(Self::StartTls),
            "implicit_tls" => Some(Self::ImplicitTls),
            _ => None,
        }
    }
}

impl fmt::Display for SubmissionSecurity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Outgoing transport configuration stored for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingConfig {
    /// Outgoing transport family.
    pub kind: SubmissionKind,
    /// Optional hostname used for SMTP submission.
    pub host: Option<String>,
    /// Optional port used for SMTP submission.
    pub port: Option<u16>,
    /// Optional username used for outgoing authentication when it differs from the account login.
    pub username: Option<String>,
    /// Optional SMTP security mode.
    pub security: Option<SubmissionSecurity>,
    /// EHLO name used for SMTP connections.
    pub hello_name: String,
}

impl OutgoingConfig {
    /// Returns a default SMTP config for an account host.
    pub fn smtp_default(host: impl Into<String>) -> Self {
        Self {
            kind: SubmissionKind::Smtp,
            host: Some(host.into()),
            port: Some(587),
            username: None,
            security: Some(SubmissionSecurity::StartTls),
            hello_name: "localhost".to_owned(),
        }
    }

    /// Returns the Proton API submission config (sending is a later
    /// phase of #237).
    pub fn proton_default() -> Self {
        Self {
            kind: SubmissionKind::Proton,
            ..Self::jmap_default()
        }
    }

    /// Returns a JMAP-native submission config.
    pub fn jmap_default() -> Self {
        Self {
            kind: SubmissionKind::Jmap,
            host: None,
            port: None,
            username: None,
            security: None,
            hello_name: "localhost".to_owned(),
        }
    }
}

/// Persisted sync status for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatusSummary {
    /// Current coarse-grained phase.
    pub phase: SyncPhase,
    /// Human-readable status detail.
    pub detail: String,
    /// Last stored checkpoint token from the protocol adapter.
    pub checkpoint: Option<String>,
    /// Time of the latest attempt, if any.
    pub last_attempt_at: Option<String>,
    /// Time of the latest successful sync, if any.
    pub last_success_at: Option<String>,
    /// Time of the latest error, if any.
    pub last_error_at: Option<String>,
    /// Latest error string, if any.
    pub last_error_message: Option<String>,
    /// Consecutive failure count recorded by the supervisor.
    pub failure_count: usize,
}

impl SyncStatusSummary {
    /// Returns the default idle state for newly created accounts.
    pub fn idle(detail: impl Into<String>) -> Self {
        Self {
            phase: SyncPhase::Idle,
            detail: detail.into(),
            checkpoint: None,
            last_attempt_at: None,
            last_success_at: None,
            last_error_at: None,
            last_error_message: None,
            failure_count: 0,
        }
    }
}

/// High-level account metadata shown in the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSummary {
    /// Account identifier.
    pub id: i64,
    /// Display name.
    pub display_name: String,
    /// Protocol family.
    pub protocol: ProtocolKind,
    /// Canonical endpoint for the account.
    pub endpoint: String,
    /// Username or account address used for sign-in.
    pub username: String,
    /// Number of configured identities.
    pub identity_count: usize,
    /// Cache policy summary.
    pub cache_policy: CachePolicySummary,
    /// Current sync status for the account.
    pub sync_status: SyncStatusSummary,
    /// The colour the user picked, or `None` for the one derived from the
    /// id. Read it through [`AccountSummary::color`].
    pub color_override: Option<AccountColor>,
}

impl AccountSummary {
    /// The account's colour: the user's choice, or the derived one.
    pub fn color(&self) -> AccountColor {
        self.color_override
            .unwrap_or_else(|| AccountColor::derived(self.id))
    }
}

/// An account's colour, from a fixed palette that reads on light and dark
/// themes. It only marks which account something belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountColor {
    /// Blue.
    Blue,
    /// Green.
    Green,
    /// Amber.
    Amber,
    /// Pink.
    Pink,
    /// Violet.
    Violet,
    /// Cyan.
    Cyan,
    /// Orange.
    Orange,
    /// Lime.
    Lime,
}

impl AccountColor {
    /// The palette, in the order accounts are given colours.
    pub const ALL: [AccountColor; 8] = [
        AccountColor::Blue,
        AccountColor::Green,
        AccountColor::Amber,
        AccountColor::Pink,
        AccountColor::Violet,
        AccountColor::Cyan,
        AccountColor::Orange,
        AccountColor::Lime,
    ];

    /// The colour an account gets when the user has not picked one:
    /// derived from its id, so the first accounts all differ and an
    /// account keeps its colour across launches. Ids start at 1, and the
    /// first account gets the first colour.
    pub fn derived(account_id: i64) -> Self {
        let len = Self::ALL.len() as i64;
        Self::ALL[(account_id - 1).rem_euclid(len) as usize]
    }

    /// The colour's position in [`AccountColor::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// The stable name stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Amber => "amber",
            Self::Pink => "pink",
            Self::Violet => "violet",
            Self::Cyan => "cyan",
            Self::Orange => "orange",
            Self::Lime => "lime",
        }
    }

    /// Parses a stored name; unknown names (from a newer version) give
    /// `None`, so the derived colour applies.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.as_str() == value)
    }

    /// The name shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Green => "Green",
            Self::Amber => "Amber",
            Self::Pink => "Pink",
            Self::Violet => "Violet",
            Self::Cyan => "Cyan",
            Self::Orange => "Orange",
            Self::Lime => "Lime",
        }
    }
}

/// Account metadata shown in the shell header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountOverview {
    /// Account identifier.
    pub id: i64,
    /// Display name.
    pub display_name: String,
    /// Number of configured identities.
    pub identity_count: usize,
    /// Cache policy summary.
    pub cache_policy: CachePolicySummary,
}

/// Writes attachment blobs for the sync adapters. The store implements it
/// with its encrypted files (ADR 0005), so an adapter never writes a
/// received attachment to disk itself.
pub trait BlobWriter: Send + Sync + fmt::Debug {
    /// Stores `contents` under the adapter's `logical_name` (which may
    /// carry a mailbox or a filename: it never reaches the disk), replacing
    /// an earlier blob with the same name, and returns the storage name
    /// to record in [`MessageAttachment::storage_path`].
    fn write_blob(&self, logical_name: &str, contents: &[u8]) -> std::io::Result<String>;

    /// Where the blobs live, for equality and diagnostics.
    fn location(&self) -> &std::path::Path;
}

/// A shared [`BlobWriter`], carried in a [`SyncTarget`].
#[derive(Debug, Clone)]
pub struct BlobStore(std::sync::Arc<dyn BlobWriter>);

impl BlobStore {
    /// Wraps a writer.
    pub fn new(writer: std::sync::Arc<dyn BlobWriter>) -> Self {
        Self(writer)
    }

    /// Stores `contents` under `logical_name`; see [`BlobWriter::write_blob`].
    pub fn write(&self, logical_name: &str, contents: &[u8]) -> std::io::Result<String> {
        self.0.write_blob(logical_name, contents)
    }
}

impl PartialEq for BlobStore {
    fn eq(&self, other: &Self) -> bool {
        self.0.location() == other.0.location()
    }
}

impl Eq for BlobStore {}

/// Immutable sync target assembled from the local store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncTarget {
    /// Account identifier.
    pub id: i64,
    /// Display name.
    pub display_name: String,
    /// Protocol family.
    pub protocol: ProtocolKind,
    /// Authentication method used against the server.
    pub auth: AuthKind,
    /// Endpoint used by the adapter.
    pub endpoint: String,
    /// Username used by the adapter.
    pub username: String,
    /// Cache policy summary.
    pub cache_policy: CachePolicySummary,
    /// Configured identities.
    pub identities: Vec<IdentitySummary>,
    /// Where sync adapters persist received attachment blobs: the store's
    /// encrypted attachment files.
    pub blobs: BlobStore,
    /// Previous checkpoint token for incremental sync.
    pub checkpoint: Option<String>,
    /// OAuth2 client configuration for accounts that authenticate with
    /// bearer tokens; `None` for password accounts.
    pub oauth2: Option<OAuth2TargetConfig>,
}

/// The OAuth2 client configuration a background sync needs to refresh
/// expired access tokens without user interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuth2TargetConfig {
    /// Provider token endpoint used for refresh requests.
    pub token_endpoint: String,
    /// Provider application client id.
    pub client_id: String,
    /// Unix timestamp (seconds) when the current access token expires,
    /// when the provider reported a lifetime.
    pub access_token_expires_at: Option<i64>,
}

/// How an account authenticates against its server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AuthKind {
    /// Username and password (or app password) authentication.
    #[default]
    Password,
    /// OAuth2 bearer token authentication (IMAP `XOAUTH2`).
    OAuth2,
    /// A Proton session: the session UID, refresh token and salted key
    /// password in the Proton secret slots. No password is stored.
    ProtonSession,
}

impl AuthKind {
    /// Returns the stable database representation of the auth kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::OAuth2 => "oauth2",
            Self::ProtonSession => "proton-session",
        }
    }

    /// Parses the database representation of an auth kind.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "password" => Some(Self::Password),
            "oauth2" => Some(Self::OAuth2),
            "proton-session" => Some(Self::ProtonSession),
            _ => None,
        }
    }
}

/// Draft used to create a new account in the local store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccount {
    /// Display name.
    pub display_name: String,
    /// Protocol family.
    pub protocol: ProtocolKind,
    /// Authentication method used against the server.
    pub auth: AuthKind,
    /// Canonical endpoint string.
    pub endpoint: String,
    /// Username or account address.
    pub username: String,
    /// Cache policy summary.
    pub cache_policy: CachePolicySummary,
    /// Optional outgoing delivery configuration.
    pub outgoing: Option<OutgoingConfig>,
    /// Identities that belong to the account.
    pub identities: Vec<NewIdentity>,
}

/// Draft used to create a new identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIdentity {
    /// Human display name.
    pub display_name: String,
    /// Email address.
    pub email_address: String,
    /// Signature block.
    pub signature: String,
    /// Whether this is the primary identity.
    pub is_primary: bool,
}

/// Persisted state of a draft in the local compose workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DraftState {
    /// The draft is editable and not queued.
    Editing,
    /// The draft is queued for background send.
    Queued,
    /// The draft is currently being sent.
    Sending,
    /// The draft was sent successfully.
    Sent,
    /// The draft has a failed outbox job and needs retry.
    Failed,
}

impl DraftState {
    /// Stable storage name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Editing => "editing",
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }

    /// Human-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Editing => "Editing",
            Self::Queued => "Queued",
            Self::Sending => "Sending",
            Self::Sent => "Sent",
            Self::Failed => "Failed",
        }
    }

    /// Parses the draft state from storage.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "editing" => Some(Self::Editing),
            "queued" => Some(Self::Queued),
            "sending" => Some(Self::Sending),
            "sent" => Some(Self::Sent),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

impl fmt::Display for DraftState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Persisted state of a queued outbox job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutboxJobState {
    /// Waiting for the first send attempt.
    Queued,
    /// Currently being sent.
    Sending,
    /// Waiting until the next retry time.
    Retrying,
    /// Delivered successfully.
    Sent,
    /// Exhausted attempts or requires manual retry.
    Failed,
}

impl OutboxJobState {
    /// Stable storage name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Retrying => "retrying",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }

    /// Human-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Sending => "Sending",
            Self::Retrying => "Retrying",
            Self::Sent => "Sent",
            Self::Failed => "Failed",
        }
    }

    /// Parses the outbox job state from storage.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "sending" => Some(Self::Sending),
            "retrying" => Some(Self::Retrying),
            "sent" => Some(Self::Sent),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

impl fmt::Display for OutboxJobState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Summary row for a draft in compose lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSummary {
    /// Draft identifier.
    pub id: i64,
    /// Account identifier.
    pub account_id: i64,
    /// Identity identifier.
    pub identity_id: i64,
    /// Selected identity display name.
    pub identity_name: String,
    /// Short recipient summary.
    pub recipient_summary: String,
    /// Subject line.
    pub subject: String,
    /// Number of attachments linked to the draft.
    pub attachment_count: usize,
    /// Draft send state.
    pub state: DraftState,
    /// Last local autosave timestamp.
    pub updated_at: String,
}

/// Attachment linked to a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftAttachment {
    /// Attachment identifier.
    pub id: i64,
    /// Display filename.
    pub filename: String,
    /// MIME content type.
    pub content_type: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Original source path supplied by the user.
    pub original_path: String,
    /// Local persisted storage path.
    pub storage_path: String,
}

/// An address offered by recipient autocomplete, from the local address
/// history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressSuggestion {
    /// The display name last seen with the address, if any.
    pub name: Option<String>,
    /// The address, lowercased.
    pub email: String,
}

impl AddressSuggestion {
    /// The suggestion as an address list entry, for example
    /// `Alice Johnson <alice@example.com>`.
    pub fn to_header_value(&self) -> String {
        participants::Participant {
            name: self.name.clone(),
            email: Some(self.email.clone()),
        }
        .to_header_value()
    }
}

/// Fully loaded draft used for compose and send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftDetail {
    /// Draft identifier.
    pub id: i64,
    /// Account identifier.
    pub account_id: i64,
    /// Identity identifier.
    pub identity_id: i64,
    /// Selected identity display name.
    pub identity_name: String,
    /// Selected identity email address.
    pub identity_email: String,
    /// Selected identity signature.
    pub identity_signature: String,
    /// Current draft state.
    pub state: DraftState,
    /// To recipients.
    pub to: Vec<String>,
    /// Cc recipients.
    pub cc: Vec<String>,
    /// Bcc recipients.
    pub bcc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body: String,
    /// Linked attachments.
    pub attachments: Vec<DraftAttachment>,
    /// Last autosave timestamp.
    pub updated_at: String,
    /// Time the draft entered the outbox, if any.
    pub queued_at: Option<String>,
    /// Time the draft was sent successfully, if any.
    pub sent_at: Option<String>,
    /// Latest send error, if any.
    pub last_error_message: Option<String>,
}

/// Draft payload used when creating a new local draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDraft {
    /// Account identifier.
    pub account_id: i64,
    /// Selected identity identifier.
    pub identity_id: i64,
    /// To recipients.
    pub to: Vec<String>,
    /// Cc recipients.
    pub cc: Vec<String>,
    /// Bcc recipients.
    pub bcc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body: String,
}

/// Full autosave payload for an existing draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftUpdate {
    /// Selected identity identifier.
    pub identity_id: i64,
    /// To recipients.
    pub to: Vec<String>,
    /// Cc recipients.
    pub cc: Vec<String>,
    /// Bcc recipients.
    pub bcc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body: String,
}

/// Summary row for an outbox job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxJobSummary {
    /// Job identifier.
    pub id: i64,
    /// Draft identifier.
    pub draft_id: i64,
    /// Account identifier.
    pub account_id: i64,
    /// Current job state.
    pub state: OutboxJobState,
    /// Attempt count so far.
    pub attempt_count: usize,
    /// Next retry time, if scheduled.
    pub next_attempt_at: Option<String>,
    /// Latest error, if any.
    pub last_error_message: Option<String>,
}

/// Attachment bytes loaded for a delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingAttachment {
    /// Display filename.
    pub filename: String,
    /// MIME content type.
    pub content_type: String,
    /// Attachment bytes.
    pub bytes: Vec<u8>,
}

/// Fully composed message handed to an outgoing transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    /// Envelope sender and From address.
    pub from_email: String,
    /// Optional display name used in the From header.
    pub from_name: Option<String>,
    /// To recipients.
    pub to: Vec<String>,
    /// Cc recipients.
    pub cc: Vec<String>,
    /// Bcc recipients.
    pub bcc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub text_body: String,
    /// Attachment payloads.
    pub attachments: Vec<OutgoingAttachment>,
}

/// Resolved account and identity used by an outbox delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryTarget {
    /// Account identifier.
    pub account_id: i64,
    /// Draft identifier.
    pub draft_id: i64,
    /// Identity identifier.
    pub identity_id: i64,
    /// Current account protocol.
    pub protocol: ProtocolKind,
    /// Authentication method used for submission (SMTP `XOAUTH2` vs
    /// password); mirrors the account's auth kind.
    pub auth: AuthKind,
    /// Canonical account endpoint.
    pub endpoint: String,
    /// Account username.
    pub username: String,
    /// Identity display name.
    pub identity_name: String,
    /// Identity email address.
    pub identity_email: String,
    /// Stored outgoing transport configuration.
    pub outgoing: OutgoingConfig,
}

/// Claimed outbox job loaded with the message payload to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOutboxJob {
    /// Job identifier.
    pub job_id: i64,
    /// Current attempt count.
    pub attempt_count: usize,
    /// Target transport and identity.
    pub target: DeliveryTarget,
    /// Fully composed outgoing message.
    pub message: OutgoingMessage,
}

/// Mailbox metadata shown in the left pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxSummary {
    /// Mailbox identifier.
    pub id: i64,
    /// Account that owns the mailbox.
    pub account_id: i64,
    /// Server name: the full path for IMAP (`Projects/Website`), the
    /// mailbox's own name for JMAP.
    pub name: String,
    /// What the mailbox is for, when the server said or the name made it
    /// clear.
    pub role: Option<MailboxRole>,
    /// The IMAP hierarchy delimiter (`/` or `.`); `None` for JMAP and for
    /// servers with a flat namespace.
    pub delimiter: Option<String>,
    /// Server name of the parent mailbox, if the mailbox is nested. The
    /// parent may not be a stored mailbox itself: IMAP parents that cannot
    /// hold mail (`\Noselect`, such as Gmail's `[Gmail]`) are not synced.
    pub parent: Option<String>,
    /// Total thread count in the local store.
    pub total_threads: usize,
    /// Unread thread count.
    pub unread_threads: usize,
}

impl MailboxSummary {
    /// The name shown for the mailbox in a folder tree: the last segment
    /// of an IMAP path, or the JMAP name as is.
    pub fn leaf_name(&self) -> &str {
        mailbox_leaf_name(
            &self.name,
            self.parent.as_deref(),
            self.delimiter.as_deref(),
        )
    }
}

/// The last segment of a mailbox name: `name` without its `parent` path
/// and delimiter, or after the last `delimiter`, or `name` itself.
pub fn mailbox_leaf_name<'a>(
    name: &'a str,
    parent: Option<&str>,
    delimiter: Option<&str>,
) -> &'a str {
    let Some(delimiter) = delimiter.filter(|delimiter| !delimiter.is_empty()) else {
        return name;
    };
    if let Some(leaf) = parent
        .and_then(|parent| name.strip_prefix(parent))
        .and_then(|rest| rest.strip_prefix(delimiter))
        .filter(|leaf| !leaf.is_empty())
    {
        return leaf;
    }
    name.rsplit_once(delimiter)
        .map(|(_, leaf)| leaf)
        .filter(|leaf| !leaf.is_empty())
        .unwrap_or(name)
}

/// What a mailbox is for: the special-use roles of RFC 6154 (IMAP) and
/// RFC 8621 (JMAP) that the app treats specially.
///
/// The variants are in sidebar order, so sorting by role puts Inbox first
/// and Trash last.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum MailboxRole {
    /// Incoming mail.
    Inbox,
    /// Unsent drafts.
    Drafts,
    /// Copies of sent mail.
    Sent,
    /// Archived mail (IMAP `\Archive`, or `\All` where there is no
    /// archive).
    Archive,
    /// Spam.
    Junk,
    /// Deleted mail.
    Trash,
}

impl MailboxRole {
    /// Every role, in sidebar order.
    pub const ALL: [Self; 6] = [
        Self::Inbox,
        Self::Drafts,
        Self::Sent,
        Self::Archive,
        Self::Junk,
        Self::Trash,
    ];

    /// Stable storage name, as in the `mailboxes.role` column and JMAP.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Drafts => "drafts",
            Self::Sent => "sent",
            Self::Archive => "archive",
            Self::Junk => "junk",
            Self::Trash => "trash",
        }
    }

    /// Human-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Drafts => "Drafts",
            Self::Sent => "Sent",
            Self::Archive => "Archive",
            Self::Junk => "Junk",
            Self::Trash => "Trash",
        }
    }

    /// Parses a stored role, ignoring case. Roles the app does not treat
    /// specially (JMAP's `important`, say) are `None`.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str().eq_ignore_ascii_case(value.trim()))
    }
}

/// A thread list that spans accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualMailbox {
    /// The mailbox with this role in every account: `Inbox` is the
    /// Unified Inbox, `Sent` the Sent favourite.
    Unified(MailboxRole),
    /// Flagged conversations in every account, outside Trash and Junk.
    /// A conversation that is in several mailboxes of one account (Gmail's
    /// labels) is listed once, from its Inbox copy when it has one.
    Flagged,
}

impl VirtualMailbox {
    /// The name the sidebar and the list header show.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unified(MailboxRole::Inbox) => "Unified Inbox",
            Self::Unified(role) => role.label(),
            Self::Flagged => "Flagged",
        }
    }
}

/// What the message list shows: one account's mailbox, or a virtual
/// mailbox across accounts. The UI keeps the last one in
/// [`preferences::UiPreferences::last_mailbox`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxSelection {
    /// One stored mailbox.
    Mailbox {
        /// Account that owns the mailbox.
        account_id: i64,
        /// Mailbox identifier.
        mailbox_id: i64,
    },
    /// A thread list across accounts.
    Virtual(VirtualMailbox),
}

/// Where a search looks. Searches start in the folder the list shows and
/// can widen to its account or to every account; an `in:` clause in the
/// query names the folder instead (see
/// [`search::SearchQuery::mailbox_clause`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchScope {
    /// One mailbox, or a virtual mailbox such as the Unified Inbox (every
    /// account's inbox) or Flagged.
    Folder(MailboxSelection),
    /// Every mailbox of one account.
    Account(i64),
    /// Every mailbox of every account.
    AllAccounts,
}

/// Which of a list's threads to show: the message list's filter menu.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ThreadFilter {
    /// Every thread.
    #[default]
    All,
    /// Threads with unread mail.
    Unread,
    /// Flagged threads.
    Flagged,
    /// Threads with one or more attachments.
    HasAttachments,
}

impl ThreadFilter {
    /// Every filter, in menu order.
    pub const ALL: [Self; 4] = [Self::All, Self::Unread, Self::Flagged, Self::HasAttachments];

    /// Human-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unread => "Unread",
            Self::Flagged => "Flagged",
            Self::HasAttachments => "Has attachments",
        }
    }
}

/// Thread counts of a [`MailboxSelection`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadCounts {
    /// Conversations listed.
    pub total_threads: usize,
    /// Conversations with unread mail.
    pub unread_threads: usize,
}

/// Thread metadata shown in the center pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
    /// Thread identifier.
    pub id: i64,
    /// Mailbox the thread row belongs to. Lists that span mailboxes
    /// (the Unified Inbox, Flagged) open each thread in its own mailbox.
    pub mailbox_id: i64,
    /// Account that owns the mailbox, so lists that span accounts can show
    /// where each row comes from.
    pub account_id: i64,
    /// Subject line.
    pub subject: String,
    /// Latest sender display string.
    pub latest_sender: String,
    /// Participant summary.
    pub participants: String,
    /// Preview text from the latest local message.
    pub preview: String,
    /// Number of messages in the thread.
    pub message_count: usize,
    /// Number of unread messages in the thread.
    pub unread_count: usize,
    /// Whether any message in the thread is flagged.
    pub is_flagged: bool,
    /// Whether any message in the thread has one or more attachments.
    pub has_attachments: bool,
    /// Latest local timestamp in ISO-8601 form.
    pub latest_message_at: String,
}

/// The selected message preview shown in the right pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePreview {
    /// Source mailbox name.
    pub mailbox_name: String,
    /// Subject line.
    pub subject: String,
    /// Latest sender display string.
    pub sender: String,
    /// Participant summary.
    pub participants: String,
    /// Identity used for the latest local message in the thread.
    pub identity_name: String,
    /// Preview body excerpt.
    pub body_preview: String,
    /// Number of messages in the thread.
    pub message_count: usize,
    /// Number of unread messages in the thread.
    pub unread_count: usize,
    /// Latest local timestamp in ISO-8601 form.
    pub latest_message_at: String,
}

/// Safe rendering mode chosen for a hydrated message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRenderMode {
    /// Render the plain-text body directly.
    PlainText,
    /// Render text derived from sanitized HTML.
    SanitizedHtml,
    /// No hydrated body is available yet.
    PreviewOnly,
}

impl MessageRenderMode {
    /// Human-facing label for diagnostics and the shell.
    pub fn label(self) -> &'static str {
        match self {
            Self::PlainText => "Plain text",
            Self::SanitizedHtml => "Sanitized HTML",
            Self::PreviewOnly => "Preview only",
        }
    }
}

impl fmt::Display for MessageRenderMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// A link in a message's rendered text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyLink {
    /// The link text, as a byte range of the rendered text.
    pub range: std::ops::Range<usize>,
    /// The original target, scheme included (`http`, `https`, `mailto`,
    /// `tel` or `sms`), checked by the sanitiser.
    pub target: String,
}

/// One message of a thread without its body: a row of a list of single
/// messages (the terminal UI's ungrouped Message Index).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSummary {
    /// Local message identifier.
    pub id: i64,
    /// The thread the message belongs to.
    pub thread_id: i64,
    /// Sender display string.
    pub sender: String,
    /// Subject line.
    pub subject: String,
    /// Received timestamp in ISO-8601 form.
    pub received_at: String,
    /// Whether the message is read.
    pub is_read: bool,
    /// Whether the message is flagged.
    pub is_flagged: bool,
    /// Whether the message has an attachment (not counting inline parts).
    pub has_attachments: bool,
}

/// Hydrated message shown in the Phase 3 reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMessage {
    /// Local message identifier.
    pub id: i64,
    /// Sender display string.
    pub sender: String,
    /// Reply-To header value, when the message carries one.
    pub reply_to: Option<String>,
    /// Recipient summary.
    pub recipients: String,
    /// Subject line.
    pub subject: String,
    /// Preview excerpt used in lists and fallback rendering.
    pub body_preview: String,
    /// Full plain-text body when available.
    pub body_text: Option<String>,
    /// Sanitized HTML body when plain text is unavailable.
    pub sanitized_html: Option<String>,
    /// Safe text rendered from the best available body representation.
    pub rendered_body: String,
    /// The links of an HTML body's text: where each run of link text in
    /// `rendered_body` points. Empty for plain text, whose links are the
    /// addresses written in it.
    pub body_links: Vec<BodyLink>,
    /// Chosen rendering mode for the hydrated body.
    pub render_mode: MessageRenderMode,
    /// Whether this message is currently marked as read locally.
    pub is_read: bool,
    /// Whether this message is flagged locally.
    pub is_flagged: bool,
    /// Whether the source HTML referenced remote content that remains blocked.
    pub has_remote_content: bool,
    /// Whether the user chose to always load remote content from this
    /// message's sender (per account; ADR 0006).
    pub remote_content_sender_allowed: bool,
    /// Whether the user marked this message's sender-safety warning as
    /// "Looks safe", so the reader no longer shows it.
    pub sender_warning_dismissed: bool,
    /// Received timestamp in ISO-8601 form.
    pub received_at: String,
    /// Attachments persisted with this message.
    pub attachments: Vec<ReceivedAttachment>,
}

/// Full conversation state shown in the Phase 3 reader pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadView {
    /// Thread identifier.
    pub id: i64,
    /// Mailbox identifier.
    pub mailbox_id: i64,
    /// Mailbox name.
    pub mailbox_name: String,
    /// Subject line.
    pub subject: String,
    /// Participant summary.
    pub participants: String,
    /// Number of messages in the thread.
    pub message_count: usize,
    /// Number of unread messages in the thread.
    pub unread_count: usize,
    /// Whether any message in the thread is flagged.
    pub is_flagged: bool,
    /// Whether any message in the thread has one or more attachments.
    pub has_attachments: bool,
    /// Whether any message body in the thread contains blocked remote content.
    pub has_remote_content: bool,
    /// Hydrated thread messages ordered oldest to newest.
    pub messages: Vec<ThreadMessage>,
}

/// Where a thread is and the read and flag state of each of its messages:
/// what a change records so that undo can put the thread back exactly,
/// including a partly read thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadState {
    /// Thread identifier.
    pub id: i64,
    /// Account that owns the thread.
    pub account_id: i64,
    /// Mailbox identifier.
    pub mailbox_id: i64,
    /// Mailbox name.
    pub mailbox_name: String,
    /// The thread's messages, oldest first.
    pub messages: Vec<MessageFlagState>,
}

impl ThreadState {
    /// Whether every message is read.
    pub fn is_read(&self) -> bool {
        self.messages.iter().all(|message| message.is_read)
    }

    /// Whether any message is flagged.
    pub fn is_flagged(&self) -> bool {
        self.messages.iter().any(|message| message.is_flagged)
    }
}

/// Where a thread is and whether it is flagged or unread: what a list
/// selection needs to know about each selected thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadMarks {
    /// Thread identifier.
    pub id: i64,
    /// Account that owns the thread.
    pub account_id: i64,
    /// Mailbox identifier.
    pub mailbox_id: i64,
    /// Whether any message is flagged.
    pub is_flagged: bool,
    /// Whether any message is unread.
    pub is_unread: bool,
}

/// The read and flag state of one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageFlagState {
    /// Local message identifier.
    pub message_id: i64,
    /// Whether the message is read.
    pub is_read: bool,
    /// Whether the message is flagged.
    pub is_flagged: bool,
}

/// Search hit returned by the local Phase 3 mail search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Account the thread belongs to.
    pub account_id: i64,
    /// Mailbox identifier.
    pub mailbox_id: i64,
    /// Mailbox name.
    pub mailbox_name: String,
    /// Thread identifier.
    pub thread_id: i64,
    /// Subject line.
    pub subject: String,
    /// Latest sender display string.
    pub latest_sender: String,
    /// Participant summary.
    pub participants: String,
    /// Matching preview snippet.
    pub preview: String,
    /// Whether the thread has any unread messages.
    pub unread_count: usize,
    /// Whether the thread is flagged.
    pub is_flagged: bool,
    /// Whether any message in the thread has one or more attachments.
    pub has_attachments: bool,
    /// Latest local timestamp in ISO-8601 form.
    pub latest_message_at: String,
    /// Whether the thread matched only through the server-side search
    /// fallback, because its bodies are not in the local index yet.
    pub from_server: bool,
}

/// A message the server reported as a search hit, located by its stable
/// remote id. IMAP hits carry the mailbox (UIDs are per mailbox); JMAP
/// email ids are account-wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHitRef {
    /// Server name of the mailbox holding the message, for IMAP.
    pub mailbox_name: Option<String>,
    /// Stable remote id (IMAP UID or JMAP email id).
    pub remote_id: String,
}

/// Store-wide counts shown in the shell header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStats {
    /// Number of accounts in the local store.
    pub account_count: usize,
    /// Number of mailboxes in the local store.
    pub mailbox_count: usize,
    /// Number of threads in the local store.
    pub thread_count: usize,
    /// Number of messages in the local store.
    pub message_count: usize,
}

/// Snapshot returned by a protocol adapter after discovery and sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSyncSnapshot {
    /// Updated checkpoint token for incremental sync.
    pub checkpoint: String,
    /// Short detail string recorded with the latest successful sync.
    pub detail: String,
    /// Optional identity replacement discovered by the protocol adapter.
    pub identities: Option<Vec<NewIdentity>>,
    /// Mailbox mirror written into the local store.
    pub mailboxes: Vec<MailboxSnapshot>,
}

/// Changes to some of an account's messages, for adapters that learn of
/// changes message by message (a Proton event batch). Unlike an
/// [`AccountSyncSnapshot`] it holds only the messages that changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailDelta {
    /// The changed messages, in their mailboxes and conversations. A
    /// message's places here are all of its places. Mailboxes that are not
    /// in the store yet are created, even when empty.
    pub mailboxes: Vec<MailboxSnapshot>,
    /// Remote ids of every message the delta covers: each leaves any
    /// mailbox and conversation `mailboxes` does not put it in. A deleted
    /// message is listed here and placed nowhere.
    pub message_ids: Vec<String>,
}

/// Mailbox snapshot written into the local store by a sync adapter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailboxSnapshot {
    /// Server name (the full path for IMAP); the store matches mailboxes
    /// by it.
    pub name: String,
    /// Server-reported role if known.
    pub role: Option<String>,
    /// IMAP hierarchy delimiter, if the server has one.
    pub delimiter: Option<String>,
    /// Server name of the parent mailbox, if nested.
    pub parent: Option<String>,
    /// Threads stored within the mailbox.
    pub threads: Vec<ThreadSnapshot>,
}

/// Kind of mailbox operation queued for server write-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOpKind {
    /// Mark the message as read on the server.
    MarkRead,
    /// Mark the message as unread on the server.
    MarkUnread,
    /// Add the flagged keyword on the server.
    SetFlagged,
    /// Remove the flagged keyword on the server.
    ClearFlagged,
    /// Move the message into another server mailbox.
    Move,
    /// Destroy the message on the server.
    Destroy,
}

impl PendingOpKind {
    /// Returns the stable database representation of the operation kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MarkRead => "mark_read",
            Self::MarkUnread => "mark_unread",
            Self::SetFlagged => "set_flagged",
            Self::ClearFlagged => "clear_flagged",
            Self::Move => "move",
            Self::Destroy => "destroy",
        }
    }

    /// Parses the database representation of an operation kind.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "mark_read" => Some(Self::MarkRead),
            "mark_unread" => Some(Self::MarkUnread),
            "set_flagged" => Some(Self::SetFlagged),
            "clear_flagged" => Some(Self::ClearFlagged),
            "move" => Some(Self::Move),
            "destroy" => Some(Self::Destroy),
            _ => None,
        }
    }
}

/// A locally applied mailbox operation waiting for server write-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOp {
    /// Pending operation row id.
    pub id: i64,
    /// Account the operation applies to.
    pub account_id: i64,
    /// Operation kind.
    pub kind: PendingOpKind,
    /// Server name of the mailbox holding the message.
    pub source_mailbox: String,
    /// Stable remote id of the message within the source mailbox.
    pub remote_id: String,
    /// Server name of the target mailbox for move operations.
    pub target_mailbox: Option<String>,
    /// Number of failed write-back attempts.
    pub attempts: i64,
    /// Last write-back error, if any.
    pub last_error: Option<String>,
}

/// Result of applying queued mailbox operations against a server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpApplyReport {
    /// Ids of operations the server accepted.
    pub applied: Vec<i64>,
    /// Ids of operations the server rejected, with the error detail.
    pub failed: Vec<(i64, String)>,
}

/// Thread snapshot written into the local store by a sync adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSnapshot {
    /// Stable server-side thread identifier, unique within a mailbox.
    ///
    /// JMAP adapters use the server thread id; IMAP adapters, which model
    /// each message as its own thread, use the message UID.
    pub remote_thread_id: String,
    /// Subject line.
    pub subject: String,
    /// Participant summary.
    pub participants: String,
    /// Preview text.
    pub preview: String,
    /// Whether any message in the thread is flagged.
    pub is_flagged: bool,
    /// Whether any message in the thread has one or more attachments.
    pub has_attachments: bool,
    /// Number of unread messages.
    pub unread_count: usize,
    /// Latest timestamp in ISO-8601 form.
    pub latest_message_at: String,
    /// Messages that belong to the thread.
    pub messages: Vec<MessageSnapshot>,
}

/// Attachment metadata carried on a message snapshot; the blob itself is
/// persisted by the sync adapter into the store's attachments directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageAttachment {
    /// Display filename from the MIME part.
    pub filename: String,
    /// MIME content type of the part.
    pub content_type: String,
    /// Size of the stored blob in bytes.
    pub size_bytes: u64,
    /// Path of the stored blob relative to the store's attachments directory.
    pub storage_path: String,
    /// The part's Content-ID, without angle brackets, so HTML can show it
    /// through `cid:` (ADR 0006).
    pub content_id: Option<String>,
    /// A body part kept only for its Content-ID (an inline image without
    /// a file name): not listed as an attachment.
    pub inline_only: bool,
}

/// A message body fetched on demand by a sync adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydratedBody {
    /// Full plain-text body when available.
    pub body_text: Option<String>,
    /// Sanitized HTML body when plain text is unavailable.
    pub sanitized_html: Option<String>,
    /// The original `text/html` part, kept even when a plain-text part
    /// exists, for HTML renderers (ADR 0006).
    pub raw_html: Option<String>,
    /// Whether the source body referenced remote content.
    pub has_remote_content: bool,
    /// Preview excerpt derived from the fetched body.
    pub body_preview: String,
    /// Attachments persisted alongside the fetched body.
    pub attachments: Vec<MessageAttachment>,
}

/// A stored message whose body the background indexer should fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCandidate {
    /// Local message row id.
    pub message_id: i64,
    /// Server name of the mailbox holding the message.
    pub mailbox_name: String,
    /// Stable remote id of the message within the mailbox (the IMAP UID).
    pub remote_id: String,
}

/// How far background body indexing has got for one account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexProgress {
    /// Messages in the local mirror.
    pub messages: usize,
    /// Messages whose body is stored and searchable.
    pub indexed: usize,
    /// Messages skipped after repeated fetch failures or for their size;
    /// they still hydrate when opened and are found by server search.
    pub skipped: usize,
}

impl IndexProgress {
    /// Messages still waiting for a body.
    pub fn pending(&self) -> usize {
        self.messages
            .saturating_sub(self.indexed)
            .saturating_sub(self.skipped)
    }
}

/// Received attachment surfaced to the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedAttachment {
    /// Local attachment identifier.
    pub id: i64,
    /// Display filename from the MIME part.
    pub filename: String,
    /// MIME content type of the part.
    pub content_type: String,
    /// Size of the stored blob in bytes.
    pub size_bytes: u64,
    /// Path of the stored blob relative to the store's attachments directory.
    pub storage_path: String,
}

/// Message snapshot written into the local store by a sync adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSnapshot {
    /// Stable server-side message identifier, unique within a mailbox.
    ///
    /// JMAP adapters use the email object id; IMAP adapters use the UID.
    pub remote_id: String,
    /// Identity email address used for the message.
    pub identity_email: String,
    /// Sender display string.
    pub sender: String,
    /// Reply-To header value, when the message carries one.
    pub reply_to: Option<String>,
    /// Recipient summary line.
    pub recipients: String,
    /// Subject line.
    pub subject: String,
    /// Message preview excerpt.
    pub body_preview: String,
    /// Full plain-text body when available.
    pub body_text: Option<String>,
    /// Sanitized HTML body when plain text is unavailable.
    pub sanitized_html: Option<String>,
    /// The original `text/html` part, kept even when a plain-text part
    /// exists, for HTML renderers (ADR 0006).
    pub raw_html: Option<String>,
    /// Whether the source body referenced remote content.
    pub has_remote_content: bool,
    /// Whether the message is read.
    pub is_read: bool,
    /// Whether the message is flagged.
    pub is_flagged: bool,
    /// Received timestamp in ISO-8601 form.
    pub received_at: String,
    /// Attachments persisted with this message.
    pub attachments: Vec<MessageAttachment>,
}

/// Upstream inputs recorded for this bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamPins {
    /// Zed commit used as the recorded reference point.
    pub zed_commit: String,
    /// Zed release tag the workspace's GPUI git dependency is pinned to.
    pub gpui_version: String,
}

/// Bootstrapped data passed into the GPUI shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppBootstrap {
    /// The app's version, as released (for Settings → About).
    pub app_version: String,
    /// Background runtime summary.
    pub runtime: RuntimeSummary,
    /// Active theme summary.
    pub theme: ThemeSummary,
    /// Inactive bundled theme summary used by the runtime toggle.
    pub alternate_theme: ThemeSummary,
    /// Upstream provenance.
    pub upstream: UpstreamPins,
}

#[cfg(test)]
mod tests {
    use super::{AccountColor, AuthKind, MailboxRole, ProtocolKind, mailbox_leaf_name};

    #[test]
    fn account_colours_round_trip_and_derive_in_palette_order() {
        for (index, color) in AccountColor::ALL.into_iter().enumerate() {
            assert_eq!(color.index(), index);
            assert_eq!(AccountColor::parse(color.as_str()), Some(color));
        }
        assert_eq!(AccountColor::parse("teal"), None);
        assert_eq!(AccountColor::derived(1), AccountColor::Blue);
        assert_eq!(AccountColor::derived(2), AccountColor::Green);
        assert_eq!(AccountColor::derived(9), AccountColor::Blue);
        assert_eq!(AccountColor::derived(0), AccountColor::Lime);
    }

    #[test]
    fn mailbox_roles_parse_their_storage_names_ignoring_case() {
        for role in MailboxRole::ALL {
            assert_eq!(MailboxRole::parse(role.as_str()), Some(role));
            assert_eq!(
                MailboxRole::parse(&role.as_str().to_uppercase()),
                Some(role)
            );
        }
        assert_eq!(MailboxRole::parse("important"), None);
        assert_eq!(MailboxRole::parse(""), None);
        let mut sorted = vec![MailboxRole::Trash, MailboxRole::Inbox, MailboxRole::Sent];
        sorted.sort();
        assert_eq!(
            sorted,
            [MailboxRole::Inbox, MailboxRole::Sent, MailboxRole::Trash],
            "roles sort in sidebar order"
        );
    }

    #[test]
    fn leaf_names_drop_the_parent_path() {
        assert_eq!(
            mailbox_leaf_name("Projects/Website", Some("Projects"), Some("/")),
            "Website"
        );
        assert_eq!(
            mailbox_leaf_name("INBOX.Lists.rust", Some("INBOX.Lists"), Some(".")),
            "rust"
        );
        assert_eq!(
            mailbox_leaf_name("[Gmail]/Sent Mail", Some("[Gmail]"), Some("/")),
            "Sent Mail"
        );
        // JMAP names are already leaves.
        assert_eq!(
            mailbox_leaf_name("Website", Some("Projects"), None),
            "Website"
        );
        assert_eq!(mailbox_leaf_name("Inbox", None, Some("/")), "Inbox");
        assert_eq!(mailbox_leaf_name("a/b", None, Some("")), "a/b");
        assert_eq!(mailbox_leaf_name("trailing/", None, Some("/")), "trailing/");
    }

    #[test]
    fn auth_kind_round_trips_through_its_database_representation() {
        for kind in [AuthKind::Password, AuthKind::OAuth2] {
            assert_eq!(AuthKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(AuthKind::Password.as_str(), "password");
        assert_eq!(AuthKind::OAuth2.as_str(), "oauth2");
        assert_eq!(AuthKind::parse("unknown"), None);
        assert_eq!(
            AuthKind::parse(ProtocolKind::Imap.as_str()),
            None,
            "auth kinds do not accept protocol names"
        );
    }

    #[test]
    fn auth_kind_defaults_to_password() {
        assert_eq!(AuthKind::default(), AuthKind::Password);
    }
}
