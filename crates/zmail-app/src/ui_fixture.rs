//! Seeded UI states for the screenshot matrix (`zmail-app launch-ui-fixture`).
//!
//! `scripts/capture-release-screenshot.sh --matrix` launches the app once per
//! state. Each launch gets an in-memory store seeded with synthetic mail, an
//! in-memory secret store and no sync, so the captures need no network, mail
//! server or keychain and look the same on every run apart from the relative
//! dates. Every name and address is made up and uses the reserved
//! `example.com`, `example.org` and `example.net` domains, apart from the
//! sender of the `reader-safety-danger` view, which has to imitate a real
//! domain (`paypa1.com`) to trigger the impersonation warning.

use anyhow::{Context, Result, bail};
use zmail_core::{
    AccountColor, AccountSyncSnapshot, AuthKind, CachePolicySummary, MailboxRole, MailboxSelection,
    MailboxSnapshot, MessageAttachment, MessageSnapshot, NewAccount, NewDraft, NewIdentity,
    OutgoingConfig, ProtocolKind, SyncPhase, ThemeAppearance, ThreadSnapshot, VirtualMailbox,
    participants::thread_participants,
    preferences::{Density, KeymapName, TEXT_SIZES},
    time::utc_timestamp,
};
use zmail_store::LocalStore;
use zmail_ui::{InitialView, OnboardingStep, SettingsSection, UnlockFixture};

/// The UI state a fixture launch opens in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixtureView {
    /// No accounts: the first-run onboarding's welcome step.
    Empty,
    /// A step of the first-run onboarding. The steps before the account
    /// is stored seed nothing; the colour and first-sync steps seed the
    /// usual account (mid-sync for the latter).
    Onboarding(OnboardingStep),
    /// The seeded inbox with its newest thread open.
    Inbox,
    /// The seeded inbox with compact (one-line) rows.
    InboxCompact,
    /// An empty inbox: "You're all caught up".
    InboxEmpty,
    /// The seeded inbox drawn as a Windows contrast theme (High Contrast
    /// Black) would draw it, on any platform.
    InboxHighContrast,
    /// An account in its first sync: skeleton rows.
    Syncing,
    /// The outbox with a message sending, one waiting for a retry and one
    /// that failed. (A queued message would be sent, and fail, as soon as
    /// the window opens.)
    Outbox,
    /// The reader on a longer conversation with an attachment: the read
    /// messages collapsed, the unread one expanded.
    Reader,
    /// The same conversation with every message expanded, and the quoted
    /// text folded.
    ReaderExpanded,
    /// The reader on one long message with a signature and quoted text.
    ReaderLong,
    /// The long message with a paragraph selected.
    ReaderSelection,
    /// An HTML message whose link shows one site and goes to another,
    /// with the open-link dialog warning about it.
    ReaderLinkWarning,
    /// The reader's conversation with an inline reply open below it.
    ReaderReply,
    /// A half-written reply in its compose window.
    Compose,
    /// A new message whose recipients include addresses that are not
    /// valid, highlighted as chips to fix.
    ComposeInvalid,
    /// A reply just sent, with the Undo Send toast showing.
    UndoSend,
    /// The Add Account window, opened over the seeded inbox.
    AccountSetup,
    /// The seeded inbox with keyboard focus in the sidebar, so a window
    /// too narrow for the sidebar pane shows it as an overlay.
    Sidebar,
    /// Three accounts on the Unified Inbox: nested folder trees, one
    /// account failing to sync and a failed message in the outbox.
    Accounts,
    /// The seeded inbox with its three newest threads selected, showing
    /// the bulk action bar and the reader's selection count.
    MultiSelect,
    /// The `accounts` view with a search across the Unified Inbox, so the
    /// scope control and account chips show.
    Search,
    /// The `search` view with filter chips on (unread, a date range) and
    /// the query's matches highlighted in the rows.
    SearchFilters,
    /// The reader on a message whose Reply-To is a subdomain of the
    /// sender's: the info sender-safety banner.
    ReaderSafetyInfo,
    /// The reader on a message whose Reply-To is an unrelated domain: the
    /// caution banner.
    ReaderSafetyCaution,
    /// The reader on a lookalike sender with blocked remote content: the
    /// danger banner and the remote-content notice.
    ReaderSafetyDanger,
    /// The reader on a message with several attachments of different
    /// types, one of them a program.
    ReaderAttachments,
    /// The `accounts` view with the command palette open and "arch" typed:
    /// commands and folders across accounts, matched letters highlighted.
    CommandPalette,
    /// The inbox with the Gmail keymap on and the keyboard shortcut cheat
    /// sheet open.
    CheatSheet,
    /// The Settings window on Accounts, opened by Fix… on the account
    /// that fails to sync, with three accounts: one with its own colour
    /// and one with a cache budget.
    SettingsAccounts,
    /// Settings → Accounts on a Gmail account signed in with its own
    /// OAuth client, whose sign-in Google stopped accepting
    /// (`invalid_grant`, as a Testing app's does after 7 days).
    SettingsAccountsExpired,
    /// The Settings window on Appearance.
    SettingsAppearance,
    /// The Settings window on Keyboard, with the Gmail keymap on.
    SettingsKeyboard,
    /// The Settings window on Privacy.
    SettingsPrivacy,
    /// The Settings window on Sending.
    SettingsSending,
    /// The Settings window on About & Diagnostics, with three accounts,
    /// outbox jobs and two changes the server refused.
    SettingsAbout,
    /// The window shown before the store opens (ADR 0005): encrypting an
    /// older store, a locked keychain, or a store whose key is gone. No
    /// store is opened.
    Unlock(UnlockFixture),
}

/// The `search-filters` view's query: words to highlight and two chips.
const SEARCH_FILTERS_QUERY: &str = "search is:unread after:30d";

impl FixtureView {
    /// Whether the view simulates a Windows contrast theme.
    pub(crate) fn high_contrast(self) -> bool {
        self == Self::InboxHighContrast
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "empty" => Self::Empty,
            "inbox" => Self::Inbox,
            "inbox-compact" => Self::InboxCompact,
            "inbox-empty" => Self::InboxEmpty,
            "inbox-high-contrast" => Self::InboxHighContrast,
            "syncing" => Self::Syncing,
            "outbox" => Self::Outbox,
            "reader" => Self::Reader,
            "reader-expanded" => Self::ReaderExpanded,
            "reader-long" => Self::ReaderLong,
            "reader-selection" => Self::ReaderSelection,
            "reader-link-warning" => Self::ReaderLinkWarning,
            "reader-reply" => Self::ReaderReply,
            "compose" => Self::Compose,
            "compose-invalid" => Self::ComposeInvalid,
            "undo-send" => Self::UndoSend,
            "account-setup" => Self::AccountSetup,
            "onboarding-provider" => Self::Onboarding(OnboardingStep::Provider),
            "onboarding-credentials" => Self::Onboarding(OnboardingStep::Credentials),
            "onboarding-google-guide" => Self::Onboarding(OnboardingStep::GoogleGuide),
            "onboarding-google-guide-invalid" => {
                Self::Onboarding(OnboardingStep::GoogleGuideInvalid)
            }
            "onboarding-discovering" => Self::Onboarding(OnboardingStep::Discovering),
            "onboarding-consent" => Self::Onboarding(OnboardingStep::Consent),
            "onboarding-result" => Self::Onboarding(OnboardingStep::Result),
            "onboarding-manual" => Self::Onboarding(OnboardingStep::Manual),
            "onboarding-manual-cleartext" => Self::Onboarding(OnboardingStep::ManualCleartext),
            "onboarding-colour" => Self::Onboarding(OnboardingStep::Colour),
            "onboarding-syncing" => Self::Onboarding(OnboardingStep::Syncing),
            "sidebar" => Self::Sidebar,
            "accounts" => Self::Accounts,
            "multi-select" => Self::MultiSelect,
            "search" => Self::Search,
            "search-filters" => Self::SearchFilters,
            "reader-safety-info" => Self::ReaderSafetyInfo,
            "reader-safety-caution" => Self::ReaderSafetyCaution,
            "reader-safety-danger" => Self::ReaderSafetyDanger,
            "reader-attachments" => Self::ReaderAttachments,
            "command-palette" => Self::CommandPalette,
            "cheat-sheet" => Self::CheatSheet,
            "settings-accounts" => Self::SettingsAccounts,
            "settings-accounts-expired" => Self::SettingsAccountsExpired,
            "settings-appearance" => Self::SettingsAppearance,
            "settings-keyboard" => Self::SettingsKeyboard,
            "settings-privacy" => Self::SettingsPrivacy,
            "settings-sending" => Self::SettingsSending,
            "settings-about" => Self::SettingsAbout,
            "unlock-encrypting" => Self::Unlock(UnlockFixture::Encrypting),
            "unlock-keychain" => Self::Unlock(UnlockFixture::Keychain),
            "unlock-unreadable" => Self::Unlock(UnlockFixture::Unreadable),
            other => bail!(
                "unknown fixture view {other}; expected empty, inbox, inbox-compact, inbox-empty, inbox-high-contrast, syncing, outbox, reader, reader-expanded, reader-long, reader-reply, reader-selection, reader-link-warning, compose, compose-invalid, undo-send, account-setup, onboarding-provider, onboarding-credentials, onboarding-google-guide, onboarding-google-guide-invalid, onboarding-discovering, onboarding-consent, onboarding-result, onboarding-manual, onboarding-manual-cleartext, onboarding-colour, onboarding-syncing, sidebar, accounts, multi-select, search, search-filters, reader-safety-info, reader-safety-caution, reader-safety-danger, reader-attachments, command-palette, cheat-sheet, settings-accounts, settings-accounts-expired, settings-appearance, settings-keyboard, settings-privacy, settings-sending, settings-about, unlock-encrypting, unlock-keychain or unlock-unreadable"
            ),
        })
    }
}

/// Parses the optional theme argument: `dark`, `light`, or `system` to follow
/// the platform appearance like a normal launch.
pub(crate) fn parse_theme(value: Option<&str>) -> Result<Option<ThemeAppearance>> {
    Ok(match value {
        None | Some("system") => None,
        Some("dark") => Some(ThemeAppearance::Dark),
        Some("light") => Some(ThemeAppearance::Light),
        Some(other) => bail!("unknown fixture theme {other}; expected dark, light or system"),
    })
}

/// Parses the optional text size argument, in percent: one of the sizes
/// Settings → Appearance offers.
pub(crate) fn parse_text_size(value: Option<&str>) -> Result<Option<u16>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value.parse::<u16>() {
        Ok(size) if TEXT_SIZES.contains(&size) => Ok(Some(size)),
        _ => bail!("unknown fixture text size {value}; expected one of {TEXT_SIZES:?}"),
    }
}

/// Identity the seeded account sends as.
const OWN_NAME: &str = "Sam Taylor";
const OWN_EMAIL: &str = "sam.taylor@example.com";

/// Remote id of the conversation the `reader` view opens.
const READER_THREAD: &str = "t-roadmap";

/// The Unix time message dates count back from: 10:00 local time today.
///
/// Anchoring to a fixed time of day rather than the current time keeps the
/// times shown in the list and reader the same on every run, and every date
/// tier (today, yesterday, this week, this year, older) is still exercised.
/// Today's messages may lie a little in the future; they still show as
/// today's times.
pub(crate) fn anchor_time() -> u64 {
    let now = chrono::Local::now();
    let anchor = now
        .date_naive()
        .and_hms_opt(10, 0, 0)
        .and_then(|time| time.and_local_timezone(chrono::Local).earliest())
        .unwrap_or(now);
    u64::try_from(anchor.timestamp()).unwrap_or_default()
}

/// Seeds `store` for `view` and returns the view the window should open on.
///
/// Message dates are offsets back from `now` (see [`anchor_time`]).
pub(crate) fn seed(store: &LocalStore, view: FixtureView, now: u64) -> Result<InitialView> {
    match view {
        FixtureView::Empty | FixtureView::Unlock(_) => return Ok(InitialView::Inbox),
        // Nothing is stored until the end of the server settings.
        FixtureView::Onboarding(step)
            if !matches!(step, OnboardingStep::Colour | OnboardingStep::Syncing) =>
        {
            return Ok(InitialView::Onboarding(step));
        }
        _ => {}
    }

    let account_id = store
        .insert_account(&fixture_account())
        .context("failed to insert the fixture account")?;
    if view == FixtureView::Syncing {
        // Nothing synced yet, and the first sync under way.
        store
            .begin_sync_attempt(account_id, SyncPhase::Syncing, "Fetching mail")
            .context("failed to record the fixture sync phase")?;
        return Ok(InitialView::Inbox);
    }
    let mut snapshot = fixture_snapshot(now);
    if view == FixtureView::InboxEmpty {
        snapshot.mailboxes[0].threads.clear();
    }
    if let Some(thread) = safety_thread(view, now) {
        snapshot.mailboxes[0].threads.insert(0, thread);
    }
    if view == FixtureView::ReaderAttachments {
        snapshot.mailboxes[0]
            .threads
            .insert(0, attachments_thread(now));
    }
    store
        .replace_account_snapshot(account_id, &snapshot)
        .context("failed to seed the fixture mailboxes")?;
    store
        .record_sync_success(account_id, &snapshot.detail, &snapshot.checkpoint)
        .context("failed to record the fixture sync")?;

    Ok(match view {
        FixtureView::Empty
        | FixtureView::Unlock(_)
        | FixtureView::Inbox
        | FixtureView::InboxHighContrast
        | FixtureView::InboxEmpty
        | FixtureView::Syncing => InitialView::Inbox,
        FixtureView::Outbox => {
            seed_outbox(store, account_id)?;
            InitialView::Outbox
        }
        FixtureView::InboxCompact => {
            store
                .update_ui_preferences(|preferences| preferences.density = Density::Compact)
                .context("failed to save the fixture density")?;
            InitialView::Inbox
        }
        FixtureView::Accounts => {
            seed_more_accounts(store, account_id, now)?;
            InitialView::Inbox
        }
        FixtureView::Search => {
            seed_more_accounts(store, account_id, now)?;
            InitialView::Search("release")
        }
        FixtureView::CommandPalette => {
            seed_more_accounts(store, account_id, now)?;
            InitialView::CommandPalette("arch")
        }
        FixtureView::CheatSheet => {
            store
                .update_ui_preferences(|preferences| preferences.keymap = KeymapName::Gmail)
                .context("failed to save the fixture keymap")?;
            InitialView::KeyboardShortcuts
        }
        FixtureView::SearchFilters => {
            seed_more_accounts(store, account_id, now)?;
            InitialView::Search(SEARCH_FILTERS_QUERY)
        }
        FixtureView::AccountSetup => InitialView::AddAccount,
        FixtureView::Onboarding(step) => {
            if step == OnboardingStep::Syncing {
                // The folders are in; the first sync is still filling one.
                store
                    .begin_sync_attempt(account_id, SyncPhase::Syncing, "Fetching Archive")
                    .context("failed to record the fixture sync phase")?;
            }
            InitialView::Onboarding(step)
        }
        FixtureView::SettingsAccounts => {
            seed_more_accounts(store, account_id, now)?;
            let accounts = store.list_accounts()?;
            store.set_account_color(accounts[1].id, Some(AccountColor::Violet))?;
            store.set_account_disk_budget(account_id, Some(1024))?;
            let failing = accounts
                .iter()
                .find(|account| account.sync_status.phase == SyncPhase::Failed)
                .context("one fixture account fails to sync")?;
            InitialView::SettingsAccount(failing.id)
        }
        FixtureView::SettingsAccountsExpired => {
            InitialView::SettingsAccount(seed_expired_google_account(store, now)?)
        }
        FixtureView::SettingsAppearance => InitialView::Settings(SettingsSection::Appearance),
        FixtureView::SettingsKeyboard => {
            store
                .update_ui_preferences(|preferences| preferences.keymap = KeymapName::Gmail)
                .context("failed to save the fixture keymap")?;
            InitialView::Settings(SettingsSection::Keyboard)
        }
        FixtureView::SettingsPrivacy => InitialView::Settings(SettingsSection::Privacy),
        FixtureView::SettingsSending => InitialView::Settings(SettingsSection::Sending),
        FixtureView::SettingsAbout => {
            seed_more_accounts(store, account_id, now)?;
            seed_outbox(store, account_id)?;
            seed_dead_ops(store, account_id)?;
            InitialView::Settings(SettingsSection::About)
        }
        FixtureView::Sidebar => InitialView::Sidebar,
        FixtureView::MultiSelect => InitialView::MultiSelect,
        FixtureView::Reader => {
            InitialView::Thread(thread_id(store, account_id, "inbox", "Q4 roadmap review")?)
        }
        FixtureView::ReaderExpanded => {
            InitialView::ThreadExpanded(thread_id(store, account_id, "inbox", "Q4 roadmap review")?)
        }
        FixtureView::ReaderLong => InitialView::Thread(thread_id(
            store,
            account_id,
            "archive",
            LONG_THREAD_SUBJECT,
        )?),
        FixtureView::ReaderSelection => InitialView::ThreadSelection(thread_id(
            store,
            account_id,
            "archive",
            LONG_THREAD_SUBJECT,
        )?),
        FixtureView::ReaderLinkWarning => InitialView::ThreadLinkPrompt(thread_id(
            store,
            account_id,
            "archive",
            LINK_THREAD_SUBJECT,
        )?),
        FixtureView::ReaderSafetyInfo
        | FixtureView::ReaderSafetyCaution
        | FixtureView::ReaderSafetyDanger => {
            InitialView::Thread(thread_id(store, account_id, "inbox", SAFETY_SUBJECT)?)
        }
        FixtureView::ReaderAttachments => {
            InitialView::Thread(thread_id(store, account_id, "inbox", ATTACHMENTS_SUBJECT)?)
        }
        FixtureView::ReaderReply => {
            InitialView::Reply(thread_id(store, account_id, "inbox", "Q4 roadmap review")?)
        }
        FixtureView::Compose => {
            let identity = store
                .list_identities(account_id)?
                .into_iter()
                .next()
                .context("the fixture account has no identity")?;
            let draft_id = store.create_draft(&NewDraft {
                account_id,
                identity_id: identity.id,
                to: vec!["Priya Raman <priya.raman@example.org>".to_owned()],
                cc: vec!["Marcus Chen <marcus.chen@example.net>".to_owned()],
                bcc: Vec::new(),
                subject: "Re: Q4 roadmap review".to_owned(),
                body: "Hi Priya,\n\nThanks for pulling this together. The offline \
                       search milestone looks right to me; I'd move the settings \
                       redesign after it so we can reuse the new components.\n\n\
                       I'll bring the estimates to Thursday's review.\n\nSam"
                    .to_owned(),
            })?;
            InitialView::Draft(draft_id)
        }
        FixtureView::UndoSend => {
            let identity = store
                .list_identities(account_id)?
                .into_iter()
                .next()
                .context("the fixture account has no identity")?;
            let draft_id = store.create_draft(&NewDraft {
                account_id,
                identity_id: identity.id,
                to: vec!["Priya Raman <priya.raman@example.org>".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Re: Q4 roadmap review".to_owned(),
                body: "Thanks Priya, see you Thursday.\n\nSam".to_owned(),
            })?;
            InitialView::Sending(draft_id)
        }
        FixtureView::ComposeInvalid => {
            let identity = store
                .list_identities(account_id)?
                .into_iter()
                .next()
                .context("the fixture account has no identity")?;
            let draft_id = store.create_draft(&NewDraft {
                account_id,
                identity_id: identity.id,
                to: vec![
                    "Priya Raman <priya.raman@example.org>".to_owned(),
                    "tomas.ortega@example..org".to_owned(),
                    "Marcus Chen <marcus.chen@example.net>".to_owned(),
                ],
                cc: vec!["design team".to_owned()],
                bcc: Vec::new(),
                subject: "Offline search demo".to_owned(),
                body: "Hi all,\n\nThe offline search build is ready to try.\n\nSam".to_owned(),
            })?;
            InitialView::Draft(draft_id)
        }
    })
}

/// The id of the thread called `subject` in the mailbox with `role`.
fn thread_id(store: &LocalStore, account_id: i64, role: &str, subject: &str) -> Result<i64> {
    let mailbox = store
        .mailbox_id_by_role(account_id, role)?
        .with_context(|| format!("the fixture {role} is missing"))?;
    store
        .list_threads(mailbox, 0..100)?
        .into_iter()
        .find(|thread| thread.subject == subject)
        .map(|thread| thread.id)
        .with_context(|| format!("the fixture thread {subject:?} is missing"))
}

/// Subject of the `reader-attachments` view's thread.
const ATTACHMENTS_SUBJECT: &str = "Files for Thursday's design review";

/// The `reader-attachments` view's thread: one message with a document,
/// a spreadsheet, images, an archive, a calendar invite and a program,
/// at sizes from bytes to megabytes. (Only the metadata is seeded; there
/// are no blobs to open or save.)
fn attachments_thread(now: u64) -> ThreadSnapshot {
    const FILES: &[(&str, &str, u64)] = &[
        ("Design review notes.pdf", "application/pdf", 1_258_291),
        (
            "Q4 estimates.xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            48_640,
        ),
        ("settings-mockup.png", "image/png", 3_407_872),
        ("sidebar-icons.zip", "application/zip", 21_495_808),
        ("review.ics", "text/calendar", 912),
        ("install-preview.exe", "application/octet-stream", 8_912_896),
    ];
    let mut thread = thread_snapshot(
        &Thread {
            id: "t-attachments",
            subject: ATTACHMENTS_SUBJECT,
            flagged: false,
            messages: &[Message {
                sender: "Priya Raman <priya.raman@example.org>",
                minutes_ago: 20 * MINUTE,
                is_read: false,
                body: "Hi Sam,\n\nEverything for Thursday is attached: the notes, the \
                       estimates, the mock-up and the icon set. The calendar invite has \
                       the room.\n\nPriya",
                attachments: &[],
            }],
        },
        now,
    );
    thread.has_attachments = true;
    thread.messages[0].attachments = FILES
        .iter()
        .map(|(filename, content_type, size_bytes)| MessageAttachment {
            filename: (*filename).to_owned(),
            content_type: (*content_type).to_owned(),
            size_bytes: *size_bytes,
            storage_path: format!("fixture/{filename}"),
            content_id: None,
            inline_only: false,
        })
        .collect();
    thread
}

/// Subject of the sender-safety views' thread.
const SAFETY_SUBJECT: &str = "Action needed: confirm your payment details";

/// The newest Inbox thread of a sender-safety view: its sender, Reply-To
/// and whether it has blocked remote content.
fn safety_thread(view: FixtureView, now: u64) -> Option<ThreadSnapshot> {
    let (sender, reply_to, remote_content) = match view {
        FixtureView::ReaderSafetyInfo => (
            "Example Billing <billing@example.com>",
            "Billing replies <replies@billing.example.com>",
            false,
        ),
        FixtureView::ReaderSafetyCaution => (
            "Example Billing <billing@example.com>",
            "Accounts <accounts@example.net>",
            false,
        ),
        FixtureView::ReaderSafetyDanger => (
            "PayPal Service <service@paypa1.com>",
            "Accounts <accounts@example.net>",
            true,
        ),
        _ => return None,
    };
    let mut thread = thread_snapshot(
        &Thread {
            id: "t-safety",
            subject: SAFETY_SUBJECT,
            flagged: false,
            messages: &[Message {
                sender: "",
                minutes_ago: 5 * MINUTE,
                is_read: false,
                body: "We could not process your last payment. Reply to this message \
                       with your card details within 24 hours to keep your account open.",
                attachments: &[],
            }],
        },
        now,
    );
    thread.participants = sender.to_owned();
    for message in &mut thread.messages {
        message.sender = sender.to_owned();
        message.reply_to = Some(reply_to.to_owned());
        message.has_remote_content = remote_content;
    }
    Some(thread)
}

/// Queues three replies and moves them to the sending, retrying and
/// failed states.
/// Two inbox changes the server refused for good: marked read, then
/// flagged, both dead-lettered after their retries.
fn seed_dead_ops(store: &LocalStore, account_id: i64) -> Result<()> {
    let inbox = store
        .mailbox_id_by_role(account_id, "inbox")?
        .context("the fixture account has an inbox")?;
    let threads = store.list_threads(inbox, 0..2)?;
    let [first, second] = threads.as_slice() else {
        bail!("the fixture inbox has two threads");
    };
    store.set_thread_read(first.id, true)?;
    store.set_thread_flagged(second.id, true)?;
    let errors = [
        "NO [CANNOT] The message no longer exists on the server",
        "NO [LIMIT] Too many flags on this mailbox",
    ];
    for (op, error) in store.list_pending_ops(account_id)?.iter().zip(errors) {
        store.mark_pending_op_dead(op.id, error)?;
    }
    Ok(())
}

fn seed_outbox(store: &LocalStore, account_id: i64) -> Result<()> {
    let identity = store
        .list_identities(account_id)?
        .into_iter()
        .next()
        .context("the fixture account has no identity")?;
    let queue = |to: &str, subject: &str| -> Result<i64> {
        let draft_id = store.create_draft(&NewDraft {
            account_id,
            identity_id: identity.id,
            to: vec![to.to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_owned(),
            body: "Thanks, see you there.\n\nSam".to_owned(),
        })?;
        store.queue_draft(draft_id)
    };
    // Claimed first, so it is the one sending.
    queue(
        "Priya Raman <priya.raman@example.org>",
        "Re: Q4 roadmap review",
    )?;
    store
        .claim_due_outbox_job(Some(account_id))?
        .context("the fixture job was not claimed")?;
    let retrying = queue(
        "Tomás Ortega <tomas.ortega@example.org>",
        "Re: Team lunch on Friday?",
    )?;
    store.record_outbox_retry(
        retrying,
        "421 4.7.0 Try again later",
        std::time::Duration::from_secs(5 * 60),
    )?;
    let failed = queue(
        "Programme Committee <talks@example.net>",
        "Re: Your talk proposal was accepted",
    )?;
    store.record_outbox_failure(failed, "550 5.1.1 Mailbox unavailable")?;
    // Stopped because the contact card pins another key than Proton serves.
    let key_changed = queue(
        "Ines Duarte <ines.duarte@example.org>",
        "Contract draft for review",
    )?;
    let change = zmail_core::key_change::PinnedKeyChanged::new(
        "ines.duarte@example.org",
        ["3F2A9C41D07E58B6A1C4E2F09B7D3A5C8E1F4062".to_owned()],
        ["B81D47E2A6C093F5D2E8417A0C9B6F3E5D2A7C18".to_owned()],
    );
    store.record_outbox_failure(key_changed, change.to_string())?;
    Ok(())
}

fn fixture_account() -> NewAccount {
    NewAccount {
        display_name: "Work".to_owned(),
        protocol: ProtocolKind::Imap,
        auth: AuthKind::Password,
        endpoint: "imaps://imap.example.com:993".to_owned(),
        username: OWN_EMAIL.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: Some(OutgoingConfig::smtp_default("smtp.example.com".to_owned())),
        identities: vec![NewIdentity {
            display_name: OWN_NAME.to_owned(),
            email_address: OWN_EMAIL.to_owned(),
            signature: format!("Regards,\n{OWN_NAME}"),
            is_primary: true,
        }],
    }
}

/// A Gmail account signed in with its own OAuth client, whose last sync
/// failed because Google refused to refresh its sign-in. Returns its id.
fn seed_expired_google_account(store: &LocalStore, now: u64) -> Result<i64> {
    let account_id = store.insert_account(&NewAccount {
        display_name: "Gmail".to_owned(),
        auth: AuthKind::OAuth2,
        endpoint: "imaps://imap.gmail.com:993".to_owned(),
        username: "sam.taylor@gmail.example".to_owned(),
        outgoing: Some(OutgoingConfig {
            port: Some(465),
            security: Some(zmail_core::SubmissionSecurity::ImplicitTls),
            username: Some("sam.taylor@gmail.example".to_owned()),
            ..OutgoingConfig::smtp_default("smtp.gmail.com".to_owned())
        }),
        ..fixture_account()
    })?;
    // A made-up client; the token endpoint is Google's, which is what
    // marks the account as a Gmail one.
    store.save_account_oauth2_client(
        account_id,
        "https://oauth2.googleapis.com/token",
        "123456789012-fixture0example.apps.googleusercontent.com",
    )?;
    store.set_account_oauth2_expiry(account_id, i64::try_from(now).ok())?;
    let expired = zmail_core::reauth::ReauthRequired::new(
        zmail_core::reauth::ReauthReason::GrantExpired,
        "invalid_grant: Token has been expired or revoked.",
    );
    store.record_sync_failure(account_id, "Sync failed after retry", expired.to_string())?;
    Ok(account_id)
}

/// One message of a seeded thread: sender, minutes before now, read state,
/// body and attachment filenames.
struct Message {
    sender: &'static str,
    minutes_ago: u64,
    is_read: bool,
    body: &'static str,
    attachments: &'static [&'static str],
}

/// One seeded thread: subject, flag and its messages, oldest first.
struct Thread {
    id: &'static str,
    subject: &'static str,
    flagged: bool,
    messages: &'static [Message],
}

const MINUTE: u64 = 1;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

const INBOX: &[Thread] = &[
    Thread {
        id: "t-standup",
        subject: "Standup notes: search indexing and release checklist",
        flagged: false,
        messages: &[Message {
            sender: "Aoife Byrne <aoife.byrne@example.com>",
            minutes_ago: 12 * MINUTE,
            is_read: false,
            body: "Morning all,\n\nQuick notes from today's standup:\n\n\
                   - Search indexing now runs in the background and no longer blocks the list.\n\
                   - The release checklist has two open items: the Windows installer smoke test \
                   and the changelog review.\n\
                   - Marcus is out on Friday.\n\nAoife",
            attachments: &[],
        }],
    },
    Thread {
        id: READER_THREAD,
        subject: "Q4 roadmap review",
        flagged: true,
        messages: &[
            Message {
                sender: "Priya Raman <priya.raman@example.org>",
                minutes_ago: 3 * HOUR + 40 * MINUTE,
                is_read: true,
                body: "Hi both,\n\nAttached is the draft Q4 roadmap. The three themes are \
                       offline search, a calmer reading experience and faster account setup. \
                       Could you mark anything that looks too ambitious before Thursday's \
                       review?\n\nThanks,\nPriya",
                attachments: &["q4-roadmap-draft.pdf"],
            },
            Message {
                sender: "Marcus Chen <marcus.chen@example.net>",
                minutes_ago: 2 * HOUR + 5 * MINUTE,
                is_read: true,
                body: "Looks good overall. Offline search depends on the new index format, \
                       so I'd plan it for November rather than October. Everything else \
                       seems realistic.\n\nMarcus\n\n\
                       On Tue, Priya Raman <priya.raman@example.org> wrote:\n\
                       > Hi both,\n>\n\
                       > Attached is the draft Q4 roadmap. Could you mark anything that \
                       looks too ambitious before Thursday's review?\n>\n> Priya",
                attachments: &[],
            },
            Message {
                sender: "Priya Raman <priya.raman@example.org>",
                minutes_ago: 48 * MINUTE,
                is_read: false,
                body: "Thanks Marcus, I've moved offline search to November and updated the \
                       timeline on page 2. Sam, do you have estimates for the settings \
                       redesign yet?\n\nPriya",
                attachments: &[],
            },
        ],
    },
    Thread {
        id: "t-invoice",
        subject: "Invoice INV-20431 for September",
        flagged: false,
        messages: &[Message {
            sender: "Northwind Hosting <billing@example.net>",
            minutes_ago: 5 * HOUR,
            is_read: true,
            body: "Hello Sam,\n\nYour invoice for September is attached. The total of \
                   EUR 184.00 will be collected on 1 November.\n\nNorthwind Hosting Billing",
            attachments: &["INV-20431.pdf"],
        }],
    },
    Thread {
        id: "t-design",
        subject: "Updated icons for the sidebar",
        flagged: false,
        messages: &[
            Message {
                sender: "Lena Fischer <lena.fischer@example.com>",
                minutes_ago: DAY + 2 * HOUR,
                is_read: true,
                body: "Here are the updated sidebar icons at 16 and 20 px. The archive and \
                       junk glyphs are new.\n\nLena",
                attachments: &["sidebar-icons.zip"],
            },
            Message {
                sender: "Sam Taylor <sam.taylor@example.com>",
                minutes_ago: DAY + HOUR,
                is_read: true,
                body: "These look great, thanks Lena. Could the junk icon be a little \
                       lighter so it doesn't stand out more than the inbox?\n\nSam",
                attachments: &[],
            },
        ],
    },
    Thread {
        id: "t-lunch",
        subject: "Team lunch on Friday?",
        flagged: false,
        messages: &[Message {
            sender: "Tomás Ortega <tomas.ortega@example.org>",
            minutes_ago: DAY + 6 * HOUR,
            is_read: false,
            body: "Anyone up for lunch on Friday to celebrate the release? I was thinking \
                   of the new place by the canal, around 12:30.\n\nTomás",
            attachments: &[],
        }],
    },
    Thread {
        id: "t-security",
        subject: "Security review: attachment sandboxing",
        flagged: true,
        messages: &[Message {
            sender: "Grace Okafor <grace.okafor@example.com>",
            minutes_ago: 3 * DAY,
            is_read: true,
            body: "I've finished the review of attachment handling. Two findings, both \
                   low severity, are written up in the tracker. Nothing blocks the \
                   release.\n\nGrace",
            attachments: &[],
        }],
    },
    Thread {
        id: "t-newsletter",
        subject: "This week in open source mail clients",
        flagged: false,
        messages: &[Message {
            sender: "Mail Dev Weekly <newsletter@example.org>",
            minutes_ago: 4 * DAY + 3 * HOUR,
            is_read: true,
            body: "This week: JMAP push in more servers, a look at IMAP NOTIFY support, \
                   and a guide to accessible keyboard navigation in desktop apps.",
            attachments: &[],
        }],
    },
    Thread {
        id: "t-conference",
        subject: "Your talk proposal was accepted",
        flagged: false,
        messages: &[Message {
            sender: "Programme Committee <talks@example.net>",
            minutes_ago: 12 * DAY,
            is_read: true,
            body: "Dear Sam,\n\nWe're happy to tell you that your talk \"Local-first \
                   email on the desktop\" has been accepted. Speaker details follow \
                   in a separate message.\n\nThe Programme Committee",
            attachments: &[],
        }],
    },
    Thread {
        id: "t-offsite",
        subject: "Offsite agenda and travel details",
        flagged: false,
        messages: &[Message {
            sender: "Aoife Byrne <aoife.byrne@example.com>",
            minutes_ago: 40 * DAY,
            is_read: true,
            body: "The offsite agenda and travel details are below. Please book trains \
                   by the end of the week so we can share taxis from the station.\n\nAoife",
            attachments: &[],
        }],
    },
    Thread {
        id: "t-welcome",
        subject: "Welcome to the team!",
        flagged: false,
        messages: &[Message {
            sender: "Priya Raman <priya.raman@example.org>",
            minutes_ago: 400 * DAY,
            is_read: true,
            body: "Welcome aboard, Sam! Your laptop arrives on Monday and your first \
                   week's schedule is in the shared calendar.\n\nPriya",
            attachments: &[],
        }],
    },
];

/// The subject of the `reader-long` conversation.
const LONG_THREAD_SUBJECT: &str = "Draft release notes for 0.13";

/// The subject of the `reader-link-warning` conversation.
const LINK_THREAD_SUBJECT: &str = "Unusual sign-in to your account";

const ARCHIVE: &[Thread] = &[
    Thread {
        id: "t-archived",
        subject: "Contract renewal signed",
        flagged: false,
        messages: &[Message {
            sender: "Legal Team <legal@example.com>",
            minutes_ago: 60 * DAY,
            is_read: true,
            body: "The renewal is signed and filed. No action needed.",
            attachments: &[],
        }],
    },
    Thread {
        id: "t-release-notes",
        subject: LONG_THREAD_SUBJECT,
        flagged: false,
        messages: &[Message {
            sender: "Grace Okafor <grace.okafor@example.com>",
            minutes_ago: 2 * DAY + 4 * HOUR,
            is_read: true,
            body: "Hi Sam,\n\n\
                   Here is a first draft of the release notes for 0.13. I kept them short \
                   and grouped the changes by what people will notice, not by crate.\n\n\
                   Reading mail\n\
                   Conversations now open with the messages you have read folded into a \
                   single line, so the newest reply is what you see first. Quoted text and \
                   signatures fold behind a small toggle, and n and p move between \
                   messages without reaching for the mouse.\n\n\
                   Search\n\
                   Searches can be scoped to the folder, the account or every account, and \
                   the scope is remembered per window. Results from a server search are \
                   marked so it is clear which ones are not in the local mirror yet.\n\n\
                   Message list\n\
                   Rows have a comfortable and a compact density, hover actions for \
                   archive, delete, flag and read, and account colours in the Unified \
                   Inbox. Selecting several conversations shows a bar for acting on all \
                   of them at once, with undo.\n\n\
                   Fixes\n\
                   - The reader no longer wraps its action bar onto two rows.\n\
                   - Dates in the reader use the same relative format as the list.\n\
                   - Moving a conversation keeps the list's scroll position.\n\n\
                   The full changelog is at https://example.com/zmail/changelog and the \
                   tracking issue has the screenshots. Let me know what you would cut; I \
                   think the search section could lose a sentence.\n\n\
                   Thanks,\nGrace\n\n\
                   --\nGrace Okafor\nRelease manager, Example Ltd\n\n\
                   On Mon, Sam Taylor <sam.taylor@example.com> wrote:\n\
                   > Could you draft the release notes for 0.13? Nothing long, the \
                   highlights are enough.\n>\n> Sam",
            attachments: &[],
        }],
    },
    Thread {
        id: "t-sign-in",
        subject: LINK_THREAD_SUBJECT,
        flagged: false,
        messages: &[Message {
            sender: "Account Security <security@bank-example.net>",
            minutes_ago: 9 * DAY,
            is_read: true,
            // HTML (see `thread_snapshot`): the link shows one site and
            // goes to another.
            body: "<p>Hello Sam,</p><p>We noticed an unusual sign-in to your account. To \
                   keep it safe, confirm your details within 24 hours:</p>\
                   <p><a href=\"https://bank-example.net/verify?id=8812\">\
                   https://bank.example.com/login</a></p><p>Account Security</p>",
            attachments: &[],
        }],
    },
];

fn fixture_snapshot(now: u64) -> AccountSyncSnapshot {
    let mailbox = |name: &str, role: Option<&str>, threads: &[Thread]| MailboxSnapshot {
        name: name.to_owned(),
        role: role.map(str::to_owned),
        threads: threads
            .iter()
            .map(|thread| thread_snapshot(thread, now))
            .collect(),
        ..Default::default()
    };
    AccountSyncSnapshot {
        checkpoint: "fixture:1".to_owned(),
        detail: "Up to date".to_owned(),
        identities: None,
        mailboxes: vec![
            mailbox("Inbox", Some("inbox"), INBOX),
            mailbox("Drafts", Some("drafts"), &[]),
            mailbox("Sent", Some("sent"), &[]),
            mailbox("Archive", Some("archive"), ARCHIVE),
            mailbox("Junk", Some("junk"), &[]),
            mailbox("Trash", Some("trash"), &[]),
            MailboxSnapshot {
                name: "Projects/Website".to_owned(),
                delimiter: Some("/".to_owned()),
                // "Projects" holds no mail, so the tree shows it as a
                // parent without a mailbox.
                parent: Some("Projects".to_owned()),
                ..Default::default()
            },
        ],
    }
}

/// Personal Inbox threads for the `accounts` view.
const PERSONAL_INBOX: &[Thread] = &[
    Thread {
        id: "p-holiday",
        subject: "Ferry tickets for August",
        flagged: true,
        messages: &[Message {
            sender: "Harbour Ferries <bookings@example.net>",
            minutes_ago: 30 * MINUTE,
            is_read: false,
            body: "Your booking is confirmed: two adults and a car, sailing on 14 August at \
                   09:30. Please arrive 45 minutes before departure.",
            attachments: &["booking-confirmation.pdf"],
        }],
    },
    Thread {
        id: "p-book-club",
        subject: "Book club: next month's pick",
        flagged: false,
        messages: &[Message {
            sender: "Nora Walsh <nora.walsh@example.org>",
            minutes_ago: 7 * HOUR,
            is_read: false,
            body: "Three votes for the lighthouse novel, so that's next month's book. \
                   We meet at mine on the 3rd.",
            attachments: &[],
        }],
    },
    Thread {
        id: "p-allotment",
        subject: "Allotment rota",
        flagged: false,
        messages: &[Message {
            sender: "Allotment Society <rota@example.org>",
            minutes_ago: 2 * DAY,
            is_read: true,
            body: "This month's watering rota is attached. Swap with a neighbour if \
                   your week does not suit.",
            attachments: &[],
        }],
    },
];

/// Side-project Inbox threads for the `accounts` view.
const SIDE_PROJECT_INBOX: &[Thread] = &[Thread {
    id: "s-release",
    subject: "Release 2.1 is out",
    flagged: false,
    messages: &[Message {
        sender: "Package Bot <bot@example.net>",
        minutes_ago: 3 * HOUR,
        is_read: false,
        body: "Version 2.1 was published. The changelog lists 14 fixes and one new \
               configuration option.",
        attachments: &[],
    }],
}];

/// The `accounts` view: two more accounts (a slash-delimited tree three
/// levels deep, and a dot-delimited one that fails to sync), a failed
/// message in the outbox, Work's tree collapsed and the Unified Inbox
/// open.
fn seed_more_accounts(store: &LocalStore, work_id: i64, now: u64) -> Result<()> {
    let account = |name: &str, email: &str| NewAccount {
        display_name: name.to_owned(),
        username: email.to_owned(),
        identities: vec![NewIdentity {
            display_name: OWN_NAME.to_owned(),
            email_address: email.to_owned(),
            signature: String::new(),
            is_primary: true,
        }],
        ..fixture_account()
    };
    let folder =
        |name: &str, role: Option<&str>, delimiter: &str, threads: &[Thread]| MailboxSnapshot {
            name: name.to_owned(),
            role: role.map(str::to_owned),
            delimiter: Some(delimiter.to_owned()),
            parent: name
                .rsplit_once(delimiter)
                .map(|(parent, _)| parent.to_owned()),
            threads: threads
                .iter()
                .map(|thread| thread_snapshot(thread, now))
                .collect(),
        };
    let snapshot = |mailboxes| AccountSyncSnapshot {
        checkpoint: "fixture:1".to_owned(),
        detail: "Up to date".to_owned(),
        identities: None,
        mailboxes,
    };

    let personal = store.insert_account(&account("Personal", "sam@example.org"))?;
    let personal_snapshot = snapshot(vec![
        folder("INBOX", Some("inbox"), "/", PERSONAL_INBOX),
        folder("Sent", Some("sent"), "/", &[]),
        folder("Trash", Some("trash"), "/", &[]),
        folder("Family", None, "/", &[]),
        folder("Family/Holidays", None, "/", &[]),
        folder("Family/Holidays/2026", None, "/", &[]),
        folder("Family/School", None, "/", &[]),
        folder("Receipts", None, "/", &[]),
    ]);
    store.replace_account_snapshot(personal, &personal_snapshot)?;
    store.record_sync_success(personal, "Up to date", "fixture:1")?;

    let side = store.insert_account(&account("Side project", "sam@example.net"))?;
    store.replace_account_snapshot(
        side,
        &snapshot(vec![
            folder("INBOX", Some("inbox"), ".", SIDE_PROJECT_INBOX),
            folder("INBOX.Sent", Some("sent"), ".", &[]),
            folder("INBOX.Lists", None, ".", &[]),
            folder("INBOX.Lists.announce", None, ".", &[]),
        ]),
    )?;
    store.record_sync_failure(
        side,
        "Sync failed",
        "authentication failed: the app password was revoked",
    )?;

    let identity = store
        .list_identities(work_id)?
        .into_iter()
        .next()
        .context("the fixture account has no identity")?;
    let draft_id = store.create_draft(&NewDraft {
        account_id: work_id,
        identity_id: identity.id,
        to: vec!["Priya Raman <priya.raman@example.org>".to_owned()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "Estimates for the settings redesign".to_owned(),
        body: "Hi Priya,\n\nEstimates attached.\n\nSam".to_owned(),
    })?;
    let job = store.queue_draft(draft_id)?;
    store.record_outbox_failure(job, "connection refused")?;

    store.update_ui_preferences(|preferences| {
        // Work's tree is collapsed so the other accounts' trees fit.
        preferences.sidebar_tree.collapsed_accounts = vec![work_id];
        preferences.last_mailbox = Some(MailboxSelection::Virtual(VirtualMailbox::Unified(
            MailboxRole::Inbox,
        )));
    })?;
    Ok(())
}

fn thread_snapshot(thread: &Thread, now: u64) -> ThreadSnapshot {
    let messages = thread
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            // A body starting with a tag is HTML, stored sanitized as the
            // adapters store an HTML-only message.
            let html = message
                .body
                .starts_with('<')
                .then(|| zmail_security::sanitize_html_for_display(message.body));
            let preview = html
                .as_ref()
                .map_or(message.body, |html| html.text.as_str())
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            MessageSnapshot {
                remote_id: format!("{}-{index}", thread.id),
                identity_email: OWN_EMAIL.to_owned(),
                sender: message.sender.to_owned(),
                reply_to: None,
                recipients: format!("{OWN_NAME} <{OWN_EMAIL}>"),
                subject: thread.subject.to_owned(),
                body_preview: preview.chars().take(160).collect(),
                body_text: html.is_none().then(|| message.body.to_owned()),
                raw_html: html.is_some().then(|| message.body.to_owned()),
                sanitized_html: html.map(|html| html.html),
                has_remote_content: false,
                is_read: message.is_read,
                is_flagged: thread.flagged,
                received_at: utc_timestamp(now.saturating_sub(message.minutes_ago * 60)),
                attachments: message
                    .attachments
                    .iter()
                    .map(|filename| MessageAttachment {
                        filename: (*filename).to_owned(),
                        content_type: content_type(filename).to_owned(),
                        size_bytes: 248_312,
                        storage_path: format!("fixture/{filename}"),
                        content_id: None,
                        inline_only: false,
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>();
    let latest = messages
        .iter()
        .max_by(|left, right| left.received_at.cmp(&right.received_at))
        .expect("fixture threads have messages");

    ThreadSnapshot {
        remote_thread_id: thread.id.to_owned(),
        subject: thread.subject.to_owned(),
        participants: thread_participants(&messages),
        preview: latest.body_preview.clone(),
        is_flagged: thread.flagged,
        has_attachments: messages
            .iter()
            .any(|message| !message.attachments.is_empty()),
        unread_count: messages.iter().filter(|message| !message.is_read).count(),
        latest_message_at: latest.received_at.clone(),
        messages,
    }
}

fn content_type(filename: &str) -> &'static str {
    match filename.rsplit_once('.').map(|(_, extension)| extension) {
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_000_000;

    fn seeded(view: FixtureView) -> Result<(LocalStore, InitialView)> {
        let store = LocalStore::open_ephemeral()?;
        let initial_view = seed(&store, view, NOW)?;
        Ok((store, initial_view))
    }

    #[test]
    fn every_view_and_theme_parses() -> Result<()> {
        for name in [
            "empty",
            "inbox",
            "inbox-compact",
            "inbox-empty",
            "inbox-high-contrast",
            "syncing",
            "outbox",
            "reader",
            "reader-expanded",
            "reader-long",
            "reader-selection",
            "reader-link-warning",
            "reader-reply",
            "compose",
            "compose-invalid",
            "undo-send",
            "account-setup",
            "sidebar",
            "accounts",
            "multi-select",
            "search",
            "search-filters",
            "reader-safety-info",
            "reader-safety-caution",
            "reader-safety-danger",
            "reader-attachments",
            "command-palette",
            "cheat-sheet",
            "settings-accounts",
            "settings-accounts-expired",
            "onboarding-google-guide",
            "onboarding-google-guide-invalid",
            "settings-appearance",
            "settings-keyboard",
            "settings-privacy",
            "settings-sending",
            "settings-about",
        ] {
            FixtureView::parse(name)?;
        }
        assert!(FixtureView::parse("settings").is_err());
        assert_eq!(parse_theme(None)?, None);
        assert_eq!(parse_theme(Some("system"))?, None);
        assert_eq!(parse_theme(Some("light"))?, Some(ThemeAppearance::Light));
        assert_eq!(parse_theme(Some("dark"))?, Some(ThemeAppearance::Dark));
        assert!(parse_theme(Some("sepia")).is_err());
        assert_eq!(parse_text_size(None)?, None);
        assert_eq!(parse_text_size(Some("200"))?, Some(200));
        assert!(parse_text_size(Some("120")).is_err());
        assert!(parse_text_size(Some("big")).is_err());
        Ok(())
    }

    #[test]
    fn settings_accounts_expired_opens_on_an_expired_google_sign_in() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::SettingsAccountsExpired)?;
        let InitialView::SettingsAccount(account_id) = initial_view else {
            bail!("expected Settings on an account, got {initial_view:?}");
        };
        let account = store
            .list_accounts()?
            .into_iter()
            .find(|account| account.id == account_id)
            .context("the expired account exists")?;
        assert_eq!(account.sync_status.phase, SyncPhase::Failed);
        assert_eq!(
            account
                .sync_status
                .last_error_message
                .as_deref()
                .and_then(zmail_core::reauth::ReauthReason::of_message),
            Some(zmail_core::reauth::ReauthReason::GrantExpired)
        );
        let target = store.sync_target(account_id)?.context("a sync target")?;
        assert_eq!(target.auth, AuthKind::OAuth2);
        assert_eq!(
            target.oauth2.map(|config| config.token_endpoint).as_deref(),
            Some("https://oauth2.googleapis.com/token")
        );
        Ok(())
    }

    #[test]
    fn settings_about_has_refused_changes_to_show() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::SettingsAbout)?;
        assert_eq!(
            initial_view,
            InitialView::Settings(zmail_ui::SettingsSection::About)
        );
        let account_id = store.list_accounts()?[0].id;
        assert_eq!(store.dead_op_count(account_id)?, 2);
        assert_eq!(store.list_accounts()?.len(), 3);
        Ok(())
    }

    #[test]
    fn reader_attachments_opens_a_message_with_several_attachments() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::ReaderAttachments)?;
        let InitialView::Thread(thread_id) = initial_view else {
            bail!("reader-attachments should open a thread");
        };
        let thread = store
            .thread_view(thread_id)?
            .context("the attachments thread should exist")?;
        assert_eq!(thread.subject, ATTACHMENTS_SUBJECT);
        assert!(thread.messages[0].attachments.len() >= 5);
        Ok(())
    }

    #[test]
    fn safety_views_open_a_message_at_each_tier() -> Result<()> {
        use zmail_security::sender_safety::{SafetyTier, analyze_sender_identity};

        for (view, tier, remote_content) in [
            (FixtureView::ReaderSafetyInfo, SafetyTier::Info, false),
            (FixtureView::ReaderSafetyCaution, SafetyTier::Caution, false),
            (FixtureView::ReaderSafetyDanger, SafetyTier::Danger, true),
        ] {
            let (store, initial_view) = seeded(view)?;
            let InitialView::Thread(thread_id) = initial_view else {
                bail!("{view:?} should open a thread");
            };
            let thread = store
                .thread_view(thread_id)?
                .context("the safety thread should exist")?;
            assert_eq!(thread.subject, SAFETY_SUBJECT);
            assert_eq!(thread.has_remote_content, remote_content, "{view:?}");
            let message = &thread.messages[0];
            let report = analyze_sender_identity(&message.sender, message.reply_to.as_deref());
            assert_eq!(report.tier(), Some(tier), "{view:?}");
        }
        Ok(())
    }

    #[test]
    fn empty_view_seeds_nothing() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::Empty)?;
        assert_eq!(initial_view, InitialView::Inbox);
        assert!(store.list_accounts()?.is_empty());
        Ok(())
    }

    #[test]
    fn inbox_lists_the_newest_thread_first() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::Inbox)?;
        assert_eq!(initial_view, InitialView::Inbox);
        let account = store.list_accounts()?.remove(0);
        let inbox = store
            .mailbox_id_by_role(account.id, "inbox")?
            .context("inbox should exist")?;
        let threads = store.list_threads(inbox, 0..100)?;
        assert_eq!(threads.len(), INBOX.len());
        assert_eq!(threads[0].subject, INBOX[0].subject);
        Ok(())
    }

    #[test]
    fn inbox_compact_saves_compact_rows() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::InboxCompact)?;
        assert_eq!(initial_view, InitialView::Inbox);
        assert_eq!(store.ui_preferences()?.density, Density::Compact);
        let (store, _) = seeded(FixtureView::Inbox)?;
        assert_eq!(store.ui_preferences()?.density, Density::Comfortable);
        Ok(())
    }

    #[test]
    fn inbox_empty_has_no_inbox_threads() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::InboxEmpty)?;
        assert_eq!(initial_view, InitialView::Inbox);
        let account = store.list_accounts()?.remove(0);
        let inbox = store
            .mailbox_id_by_role(account.id, "inbox")?
            .context("inbox should exist")?;
        assert!(store.list_threads(inbox, 0..10)?.is_empty());
        Ok(())
    }

    #[test]
    fn syncing_is_a_first_sync() -> Result<()> {
        let (store, _) = seeded(FixtureView::Syncing)?;
        let account = store.list_accounts()?.remove(0);
        assert_eq!(account.sync_status.phase, SyncPhase::Syncing);
        assert_eq!(account.sync_status.last_success_at, None);
        assert!(store.list_mailboxes(account.id)?.is_empty());
        Ok(())
    }

    #[test]
    fn outbox_has_a_job_in_each_state() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::Outbox)?;
        assert_eq!(initial_view, InitialView::Outbox);
        let mut states = store
            .list_outbox_jobs(None)?
            .into_iter()
            .map(|job| job.state.as_str())
            .collect::<Vec<_>>();
        states.sort_unstable();
        assert_eq!(states, ["failed", "failed", "retrying", "sending"]);
        Ok(())
    }

    #[test]
    fn reader_opens_the_roadmap_thread_with_its_bodies() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::Reader)?;
        let InitialView::Thread(thread_id) = initial_view else {
            bail!("the reader fixture should open a thread, not {initial_view:?}");
        };
        let thread = store
            .thread_view(thread_id)?
            .context("thread should exist")?;
        assert_eq!(thread.subject, "Q4 roadmap review");
        assert_eq!(thread.messages.len(), 3);
        assert!(thread.has_attachments);
        // Bodies are seeded, so opening the thread never tries to fetch
        // them from a server.
        assert_eq!(store.messages_missing_bodies(thread_id)?, Some(Vec::new()));
        Ok(())
    }

    #[test]
    fn reader_expanded_opens_the_roadmap_thread_expanded() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::ReaderExpanded)?;
        let InitialView::ThreadExpanded(thread_id) = initial_view else {
            bail!("the reader-expanded fixture should open a thread, not {initial_view:?}");
        };
        let thread = store
            .thread_view(thread_id)?
            .context("thread should exist")?;
        assert_eq!(thread.subject, "Q4 roadmap review");
        Ok(())
    }

    #[test]
    fn reader_long_opens_the_archived_release_notes() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::ReaderLong)?;
        let InitialView::Thread(thread_id) = initial_view else {
            bail!("the reader-long fixture should open a thread, not {initial_view:?}");
        };
        let thread = store
            .thread_view(thread_id)?
            .context("thread should exist")?;
        assert_eq!(thread.subject, LONG_THREAD_SUBJECT);
        assert!(
            thread.messages[0]
                .rendered_body
                .contains("\n--\nGrace Okafor")
        );
        Ok(())
    }

    #[test]
    fn reader_link_warning_opens_an_html_message_with_a_link() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::ReaderLinkWarning)?;
        let InitialView::ThreadLinkPrompt(thread_id) = initial_view else {
            bail!("the reader-link-warning fixture should open a thread, not {initial_view:?}");
        };
        let thread = store
            .thread_view(thread_id)?
            .context("thread should exist")?;
        let message = &thread.messages[0];
        assert_eq!(message.body_text, None);
        let html = message
            .sanitized_html
            .as_deref()
            .context("the HTML is stored")?;
        assert!(
            html.contains("href=\"https://bank-example.net/verify?id=8812\""),
            "{html}"
        );
        assert!(
            message
                .rendered_body
                .contains("https://bank.example.com/login")
        );
        assert_eq!(
            message
                .body_links
                .iter()
                .map(|link| link.target.as_str())
                .collect::<Vec<_>>(),
            ["https://bank-example.net/verify?id=8812"]
        );
        Ok(())
    }

    #[test]
    fn compose_opens_a_filled_draft() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::Compose)?;
        let InitialView::Draft(draft_id) = initial_view else {
            bail!("the compose fixture should open a draft, not {initial_view:?}");
        };
        let draft = store
            .draft_detail(draft_id)?
            .context("draft should exist")?;
        assert_eq!(draft.subject, "Re: Q4 roadmap review");
        Ok(())
    }

    #[test]
    fn sidebar_opens_the_seeded_inbox_on_the_sidebar() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::Sidebar)?;
        assert_eq!(initial_view, InitialView::Sidebar);
        assert_eq!(store.list_accounts()?.len(), 1);
        Ok(())
    }

    #[test]
    fn search_finds_hits_in_several_accounts_unified_inbox() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::Search)?;
        let InitialView::Search(query) = initial_view else {
            bail!("the search fixture should open a search, not {initial_view:?}");
        };
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        let hits = store.search_threads_in(&zmail_core::SearchScope::Folder(unified), query, 50)?;
        let accounts = hits
            .iter()
            .map(|hit| hit.account_id)
            .collect::<std::collections::HashSet<_>>();
        assert!(accounts.len() > 1, "{hits:?}");
        Ok(())
    }

    #[test]
    fn search_filters_finds_unread_recent_hits() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::SearchFilters)?;
        assert_eq!(initial_view, InitialView::Search(SEARCH_FILTERS_QUERY));
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        let hits = store.search_threads_in(
            &zmail_core::SearchScope::Folder(unified),
            SEARCH_FILTERS_QUERY,
            50,
        )?;
        assert!(hits.len() > 1, "{hits:?}");
        assert!(hits.iter().all(|hit| hit.unread_count > 0));
        Ok(())
    }

    #[test]
    fn accounts_seeds_three_accounts_on_the_unified_inbox() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::Accounts)?;
        assert_eq!(initial_view, InitialView::Inbox);
        let accounts = store.list_accounts()?;
        assert_eq!(accounts.len(), 3);
        assert!(accounts[2].sync_status.failure_count > 0);
        let unified = MailboxSelection::Virtual(VirtualMailbox::Unified(MailboxRole::Inbox));
        assert_eq!(
            store.ui_preferences()?.last_mailbox,
            Some(unified),
            "the window opens on the Unified Inbox"
        );
        assert_eq!(
            store.selection_counts(&unified)?.total_threads,
            INBOX.len() + PERSONAL_INBOX.len() + SIDE_PROJECT_INBOX.len()
        );
        let nested = store
            .list_mailboxes(accounts[1].id)?
            .into_iter()
            .find(|mailbox| mailbox.name == "Family/Holidays/2026")
            .context("the nested folder exists")?;
        assert_eq!(nested.parent.as_deref(), Some("Family/Holidays"));
        assert_eq!(store.list_outbox_jobs(None)?.len(), 1);
        Ok(())
    }

    #[test]
    fn multi_select_opens_the_seeded_inbox_with_a_selection() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::MultiSelect)?;
        assert_eq!(initial_view, InitialView::MultiSelect);
        assert_eq!(store.list_accounts()?.len(), 1);
        Ok(())
    }

    #[test]
    fn account_setup_keeps_the_account() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::AccountSetup)?;
        assert_eq!(initial_view, InitialView::AddAccount);
        assert_eq!(store.list_accounts()?.len(), 1);
        Ok(())
    }

    #[test]
    fn onboarding_seeds_an_account_only_once_it_would_exist() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::parse("onboarding-manual-cleartext")?)?;
        assert_eq!(
            initial_view,
            InitialView::Onboarding(OnboardingStep::ManualCleartext)
        );
        assert!(store.list_accounts()?.is_empty());

        let (store, initial_view) = seeded(FixtureView::parse("onboarding-syncing")?)?;
        assert_eq!(
            initial_view,
            InitialView::Onboarding(OnboardingStep::Syncing)
        );
        let account = store.list_accounts()?.remove(0);
        assert_eq!(account.sync_status.phase, SyncPhase::Syncing);
        assert!(!store.list_mailboxes(account.id)?.is_empty());
        Ok(())
    }
}
