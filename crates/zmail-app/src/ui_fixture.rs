//! Seeded UI states for the screenshot matrix (`zmail-app launch-ui-fixture`).
//!
//! `scripts/capture-release-screenshot.sh --matrix` launches the app once per
//! state. Each launch gets an in-memory store seeded with synthetic mail, an
//! in-memory secret store and no sync, so the captures need no network, mail
//! server or keychain and look the same on every run apart from the relative
//! dates. Every name and address is made up and uses the reserved
//! `example.com`, `example.org` and `example.net` domains.

use anyhow::{Context, Result, bail};
use zmail_core::{
    AccountSyncSnapshot, AuthKind, CachePolicySummary, MailboxSnapshot, MessageAttachment,
    MessageSnapshot, NewAccount, NewDraft, NewIdentity, OutgoingConfig, ProtocolKind,
    ThemeAppearance, ThreadSnapshot, participants::thread_participants, time::utc_timestamp,
};
use zmail_store::LocalStore;
use zmail_ui::InitialView;

/// The UI state a fixture launch opens in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixtureView {
    /// No accounts: the first-run account setup.
    Empty,
    /// The seeded inbox with its newest thread open.
    Inbox,
    /// The reader on a longer conversation with an attachment.
    Reader,
    /// The compose pane on a half-written reply.
    Compose,
    /// The account setup pane with an account already configured.
    AccountSetup,
}

impl FixtureView {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "empty" => Self::Empty,
            "inbox" => Self::Inbox,
            "reader" => Self::Reader,
            "compose" => Self::Compose,
            "account-setup" => Self::AccountSetup,
            other => bail!(
                "unknown fixture view {other}; expected empty, inbox, reader, compose or account-setup"
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
    if view == FixtureView::Empty {
        return Ok(InitialView::Inbox);
    }

    let account_id = store
        .insert_account(&fixture_account())
        .context("failed to insert the fixture account")?;
    let snapshot = fixture_snapshot(now);
    store
        .replace_account_snapshot(account_id, &snapshot)
        .context("failed to seed the fixture mailboxes")?;
    store
        .record_sync_success(account_id, &snapshot.detail, &snapshot.checkpoint)
        .context("failed to record the fixture sync")?;

    Ok(match view {
        FixtureView::Empty | FixtureView::Inbox => InitialView::Inbox,
        FixtureView::AccountSetup => InitialView::AccountSetup,
        FixtureView::Reader => {
            let inbox = store
                .mailbox_id_by_role(account_id, "inbox")?
                .context("the fixture inbox is missing")?;
            let thread = store
                .list_threads(inbox, 0..100)?
                .into_iter()
                .find(|thread| thread.subject == "Q4 roadmap review")
                .context("the fixture reader thread is missing")?;
            InitialView::Thread(thread.id)
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
    })
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
                       seems realistic.\n\nMarcus",
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

const ARCHIVE: &[Thread] = &[Thread {
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
}];

fn fixture_snapshot(now: u64) -> AccountSyncSnapshot {
    let mailbox = |name: &str, role: Option<&str>, threads: &[Thread]| MailboxSnapshot {
        name: name.to_owned(),
        role: role.map(str::to_owned),
        threads: threads
            .iter()
            .map(|thread| thread_snapshot(thread, now))
            .collect(),
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
            mailbox("Projects/Website", None, &[]),
        ],
    }
}

fn thread_snapshot(thread: &Thread, now: u64) -> ThreadSnapshot {
    let messages = thread
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let preview = message
                .body
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
                body_text: Some(message.body.to_owned()),
                sanitized_html: None,
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
        for name in ["empty", "inbox", "reader", "compose", "account-setup"] {
            FixtureView::parse(name)?;
        }
        assert!(FixtureView::parse("settings").is_err());
        assert_eq!(parse_theme(None)?, None);
        assert_eq!(parse_theme(Some("system"))?, None);
        assert_eq!(parse_theme(Some("light"))?, Some(ThemeAppearance::Light));
        assert_eq!(parse_theme(Some("dark"))?, Some(ThemeAppearance::Dark));
        assert!(parse_theme(Some("sepia")).is_err());
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
    fn account_setup_keeps_the_account() -> Result<()> {
        let (store, initial_view) = seeded(FixtureView::AccountSetup)?;
        assert_eq!(initial_view, InitialView::AccountSetup);
        assert_eq!(store.list_accounts()?.len(), 1);
        Ok(())
    }
}
