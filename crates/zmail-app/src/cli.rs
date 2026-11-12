//! The command line: one table of commands ([`COMMANDS`]), each with its
//! arguments, options and a line for the help, and a parser that never
//! runs a command it doesn't fully understand.
//!
//! - `--help` or `-h` anywhere before `--` prints help instead of running
//!   anything: the command's own help when the line names one.
//! - An unknown option, a missing argument or one too many is a usage
//!   error (exit status 2), never ignored.
//! - `--` ends the options: every word after it is an argument.

use std::{fmt, path::PathBuf};

use zmail_core::{ProtocolKind, ThemeAppearance};

use crate::ui_fixture;

/// What the command line asks for.
pub(crate) enum Command {
    /// No command: the desktop app.
    Gui,
    /// The help: for one command, or all of them.
    Help(Option<&'static Spec>),
    Version,
    Tui,
    LaunchTuiFixture(ui_fixture::FixtureView),
    Mailto(String),
    LaunchImapFixture {
        endpoint: String,
        username: String,
        email: String,
    },
    LaunchUiFixture {
        view: ui_fixture::FixtureView,
        theme: Option<ThemeAppearance>,
        text_size: Option<u16>,
    },
    Backup {
        directory: Option<PathBuf>,
    },
    ResetStore,
    RotateKey,
    DeadOps {
        account_id: Option<i64>,
    },
    RetryOp(i64),
    DiscardOp(i64),
    EmptyTrash {
        account_id: i64,
        yes: bool,
    },
    CacheBudget {
        account_id: i64,
        budget_mb: Option<u64>,
    },
    ExportAccount {
        account_id: i64,
        path: PathBuf,
    },
    AddAccount {
        protocol: ProtocolKind,
        allow_cleartext: bool,
    },
    ListAccounts,
    /// `sync`: every account, or one.
    Sync(Option<i64>),
    Search {
        account_id: i64,
        query: String,
        server: bool,
    },
    IndexAccount(i64),
    ReadThread(i64),
    ListAttachments(i64),
    SaveAttachment {
        attachment_id: i64,
        destination: Option<PathBuf>,
        overwrite: bool,
    },
    FlagThread(i64, bool),
    MarkThreadRead(i64, bool),
    /// `move`: the thread, and the mailbox as given (id, role or name).
    MoveThread(i64, String),
    ListMailboxes(i64),
    ArchiveThread(i64),
    DeleteThread {
        thread_id: i64,
        yes: bool,
    },
    Compose(i64, Option<i64>),
    ListDrafts(Option<i64>),
    ShowDraft(i64),
    SendDraft(i64),
    ListOutbox(Option<i64>),
    RetryOutbox(i64),
    AddAttachment(i64, PathBuf),
    RemoveAttachment(i64),
    ProcessOutbox(Option<i64>),
}

/// Where a command is listed in the help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Mail,
    Writing,
    Accounts,
    Store,
    /// Fixture and developer commands, listed last.
    Developer,
}

impl Group {
    const ALL: [Self; 5] = [
        Self::Mail,
        Self::Writing,
        Self::Accounts,
        Self::Store,
        Self::Developer,
    ];

    fn heading(self) -> &'static str {
        match self {
            Self::Mail => "Reading and triage",
            Self::Writing => "Writing and sending",
            Self::Accounts => "Accounts",
            Self::Store => "The mail store",
            Self::Developer => "Fixtures and development",
        }
    }
}

/// An option a command takes.
pub(crate) struct Opt {
    pub(crate) name: &'static str,
    /// The value's placeholder, for an option that takes one.
    pub(crate) value: Option<&'static str>,
    pub(crate) help: &'static str,
}

/// One command: its name, arguments, options and help.
pub(crate) struct Spec {
    pub(crate) name: &'static str,
    /// The arguments' placeholders, in order.
    pub(crate) args: &'static [&'static str],
    /// How many of `args` are required; the rest are optional.
    pub(crate) required: usize,
    /// The last argument takes every word left (a search query). Words
    /// in it may start with one `-`.
    pub(crate) rest: bool,
    pub(crate) options: &'static [Opt],
    /// One line for the help.
    pub(crate) summary: &'static str,
    /// More for the command's own help, or nothing.
    pub(crate) details: &'static str,
    pub(crate) group: Group,
    /// Whether this build has the command (`tui` needs the `tui` feature).
    pub(crate) available: bool,
    build: fn(&Args) -> Result<Command, UsageError>,
}

impl Spec {
    /// `zmail-app NAME ARGS [OPTIONS]`.
    pub(crate) fn usage(&self) -> String {
        let mut usage = format!("zmail-app {}", self.name);
        for option in self
            .options
            .iter()
            .filter(|option| option.name == SERVER.name)
        {
            // `search [--server] ACCOUNT QUERY...` reads better with the
            // option first.
            usage.push_str(&format!(" [{}]", option.name));
        }
        usage.push_str(&self.argument_list());
        for option in self
            .options
            .iter()
            .filter(|option| option.name != SERVER.name)
        {
            match option.value {
                Some(value) => usage.push_str(&format!(" [{} {value}]", option.name)),
                None => usage.push_str(&format!(" [{}]", option.name)),
            }
        }
        usage
    }

    /// ` THREAD MAILBOX`, ` [ACCOUNT]`, ` ACCOUNT QUERY...`.
    fn argument_list(&self) -> String {
        let mut list = String::new();
        for (index, arg) in self.args.iter().enumerate() {
            let repeat = if self.rest && index + 1 == self.args.len() {
                "..."
            } else {
                ""
            };
            if index < self.required {
                list.push_str(&format!(" {arg}{repeat}"));
            } else {
                list.push_str(&format!(" [{arg}{repeat}]"));
            }
        }
        list
    }

    fn takes(&self, name: &str) -> bool {
        self.options.iter().any(|option| option.name == name)
    }
}

const FORCE: Opt = Opt {
    name: "--force",
    value: None,
    help: "Run here even while another Zmail syncs this store",
};
const YES: Opt = Opt {
    name: "--yes",
    value: None,
    help: "Don't ask first (needed off a terminal)",
};
const SERVER: Opt = Opt {
    name: "--server",
    value: None,
    help: "Also ask the server about mail not indexed yet",
};
const ALLOW_CLEARTEXT: Opt = Opt {
    name: "--allow-cleartext",
    value: None,
    help: "Allow a server on another machine without TLS",
};

/// Every command, in the order the help lists them.
pub(crate) static COMMANDS: &[Spec] = &[
    // Reading and triage.
    Spec {
        name: "tui",
        args: &[],
        required: 0,
        rest: false,
        options: &[],
        summary: "Read and write mail in the terminal",
        details: "",
        group: Group::Mail,
        available: cfg!(feature = "tui"),
        build: |_| Ok(Command::Tui),
    },
    Spec {
        name: "sync",
        args: &["all|ACCOUNT"],
        required: 0,
        rest: false,
        options: &[FORCE],
        summary: "Sync every account, or one",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| match args.arg(0) {
            None | Some("all") => Ok(Command::Sync(None)),
            Some(_) => Ok(Command::Sync(Some(args.id(0)?))),
        },
    },
    Spec {
        name: "mailboxes",
        args: &["ACCOUNT"],
        required: 1,
        rest: false,
        options: &[],
        summary: "List an account's mailboxes and their ids",
        details: "`move` takes a mailbox's id, role or name.",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::ListMailboxes(args.id(0)?)),
    },
    Spec {
        name: "search",
        args: &["ACCOUNT", "QUERY"],
        required: 2,
        rest: true,
        options: &[SERVER],
        summary: "Search an account's mail",
        details: "QUERY is the search text, such as `from:ada invoice`. Put `--` before \
                  query words that start with `--`.",
        group: Group::Mail,
        available: true,
        build: |args| {
            let query = args.rest(1);
            if query.trim().is_empty() {
                return Err(args.error("QUERY must not be empty"));
            }
            Ok(Command::Search {
                account_id: args.id(0)?,
                query,
                server: args.flag(SERVER.name),
            })
        },
    },
    Spec {
        name: "index",
        args: &["ACCOUNT"],
        required: 1,
        rest: false,
        options: &[FORCE],
        summary: "Fetch and index message bodies for search",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::IndexAccount(args.id(0)?)),
    },
    Spec {
        name: "read",
        args: &["THREAD"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Print a conversation",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::ReadThread(args.id(0)?)),
    },
    Spec {
        name: "attachments",
        args: &["THREAD"],
        required: 1,
        rest: false,
        options: &[],
        summary: "List a conversation's attachments",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::ListAttachments(args.id(0)?)),
    },
    Spec {
        name: "save-attachment",
        args: &["ATTACHMENT", "DIRECTORY"],
        required: 1,
        rest: false,
        options: &[
            Opt {
                name: "--dest",
                value: Some("DIRECTORY"),
                help: "The folder to save into (the current one by default)",
            },
            Opt {
                name: "--overwrite",
                value: None,
                help: "Replace a file of the same name",
            },
        ],
        summary: "Save an attachment into a folder",
        details: "It never replaces a file of the same name unless --overwrite is given.",
        group: Group::Mail,
        available: true,
        build: |args| {
            let destination = match (args.arg(1), args.value("--dest")) {
                (Some(_), Some(_)) => {
                    return Err(args.error("give the DIRECTORY or --dest DIRECTORY, not both"));
                }
                (Some(directory), None) | (None, Some(directory)) => Some(PathBuf::from(directory)),
                (None, None) => None,
            };
            Ok(Command::SaveAttachment {
                attachment_id: args.id(0)?,
                destination,
                overwrite: args.flag("--overwrite"),
            })
        },
    },
    Spec {
        name: "flag",
        args: &["THREAD"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Flag a conversation",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::FlagThread(args.id(0)?, true)),
    },
    Spec {
        name: "unflag",
        args: &["THREAD"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Remove a conversation's flag",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::FlagThread(args.id(0)?, false)),
    },
    Spec {
        name: "mark-read",
        args: &["THREAD"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Mark a conversation read",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::MarkThreadRead(args.id(0)?, true)),
    },
    Spec {
        name: "mark-unread",
        args: &["THREAD"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Mark a conversation unread",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::MarkThreadRead(args.id(0)?, false)),
    },
    Spec {
        name: "move",
        args: &["THREAD", "MAILBOX"],
        required: 2,
        rest: false,
        options: &[],
        summary: "Move a conversation to another mailbox",
        details: "MAILBOX is a mailbox's id, its role (inbox, drafts, sent, archive, \
                  junk or trash) or its name, as `zmail-app mailboxes ACCOUNT` lists \
                  them.",
        group: Group::Mail,
        available: true,
        build: |args| {
            Ok(Command::MoveThread(
                args.id(0)?,
                args.arg(1).unwrap_or_default().to_owned(),
            ))
        },
    },
    Spec {
        name: "archive",
        args: &["THREAD"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Move a conversation to the Archive",
        details: "",
        group: Group::Mail,
        available: true,
        build: |args| Ok(Command::ArchiveThread(args.id(0)?)),
    },
    Spec {
        name: "delete",
        args: &["THREAD"],
        required: 1,
        rest: false,
        options: &[YES],
        summary: "Move a conversation to the Trash",
        details: "A conversation already in the Trash, or in an account with no Trash, is \
                  deleted for good instead. That asks first in a terminal; elsewhere it \
                  needs --yes.",
        group: Group::Mail,
        available: true,
        build: |args| {
            Ok(Command::DeleteThread {
                thread_id: args.id(0)?,
                yes: args.flag(YES.name),
            })
        },
    },
    // Writing and sending.
    Spec {
        name: "compose",
        args: &["ACCOUNT", "IDENTITY"],
        required: 1,
        rest: false,
        options: &[FORCE],
        summary: "Write a draft at prompts, then send it or keep it",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| Ok(Command::Compose(args.id(0)?, args.optional_id(1)?)),
    },
    Spec {
        name: "drafts",
        args: &["ACCOUNT"],
        required: 0,
        rest: false,
        options: &[],
        summary: "List drafts",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| Ok(Command::ListDrafts(args.optional_id(0)?)),
    },
    Spec {
        name: "draft",
        args: &["DRAFT"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Print a draft",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| Ok(Command::ShowDraft(args.id(0)?)),
    },
    Spec {
        name: "attach",
        args: &["DRAFT", "PATH"],
        required: 2,
        rest: false,
        options: &[],
        summary: "Attach a file to a draft",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| {
            Ok(Command::AddAttachment(
                args.id(0)?,
                PathBuf::from(args.arg(1).unwrap_or_default()),
            ))
        },
    },
    Spec {
        name: "detach",
        args: &["ATTACHMENT"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Remove an attachment from a draft",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| Ok(Command::RemoveAttachment(args.id(0)?)),
    },
    Spec {
        name: "send",
        args: &["DRAFT"],
        required: 1,
        rest: false,
        options: &[FORCE],
        summary: "Send a draft",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| Ok(Command::SendDraft(args.id(0)?)),
    },
    Spec {
        name: "outbox",
        args: &["ACCOUNT"],
        required: 0,
        rest: false,
        options: &[],
        summary: "List the outbox's jobs",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| Ok(Command::ListOutbox(args.optional_id(0)?)),
    },
    Spec {
        name: "retry-outbox",
        args: &["JOB"],
        required: 1,
        rest: false,
        options: &[FORCE],
        summary: "Try a failed outbox job again",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| Ok(Command::RetryOutbox(args.id(0)?)),
    },
    Spec {
        name: "process-outbox",
        args: &["ACCOUNT"],
        required: 0,
        rest: false,
        options: &[FORCE],
        summary: "Send what is due in the outbox now",
        details: "",
        group: Group::Writing,
        available: true,
        build: |args| Ok(Command::ProcessOutbox(args.optional_id(0)?)),
    },
    Spec {
        name: "mailto",
        args: &["URL"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Open the window on a draft from a mailto: link",
        details: "URL is the link without its `mailto:`. The system's mail link handler \
                  runs `zmail-app mailto:...` with the whole link instead.",
        group: Group::Writing,
        available: true,
        build: |args| {
            let url = args.arg(0).unwrap_or_default();
            Ok(Command::Mailto(if url.starts_with("mailto:") {
                url.to_owned()
            } else {
                format!("mailto:{url}")
            }))
        },
    },
    // Accounts.
    Spec {
        name: "list-accounts",
        args: &[],
        required: 0,
        rest: false,
        options: &[],
        summary: "List accounts",
        details: "",
        group: Group::Accounts,
        available: true,
        build: |_| Ok(Command::ListAccounts),
    },
    Spec {
        name: "add-imap",
        args: &[],
        required: 0,
        rest: false,
        options: &[ALLOW_CLEARTEXT, FORCE],
        summary: "Add an IMAP account at prompts, and sync it",
        details: "A server without TLS (imap://, or SMTP security `plain`) is refused \
                  unless it is on this machine or --allow-cleartext is given.",
        group: Group::Accounts,
        available: true,
        build: |args| {
            Ok(Command::AddAccount {
                protocol: ProtocolKind::Imap,
                allow_cleartext: args.flag(ALLOW_CLEARTEXT.name),
            })
        },
    },
    Spec {
        name: "add-jmap",
        args: &[],
        required: 0,
        rest: false,
        options: &[ALLOW_CLEARTEXT, FORCE],
        summary: "Add a JMAP account at prompts, and sync it",
        details: "A server without TLS (http://) is refused unless it is on this machine \
                  or --allow-cleartext is given.",
        group: Group::Accounts,
        available: true,
        build: |args| {
            Ok(Command::AddAccount {
                protocol: ProtocolKind::Jmap,
                allow_cleartext: args.flag(ALLOW_CLEARTEXT.name),
            })
        },
    },
    Spec {
        name: "cache-budget",
        args: &["ACCOUNT", "MIB|unlimited"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Set or remove an account's disk budget for mail",
        details: "With no MIB, or `unlimited`, the budget is removed. A new budget applies \
                  at once.",
        group: Group::Accounts,
        available: true,
        build: |args| {
            let budget_mb = match args.arg(1) {
                None | Some("unlimited") => None,
                Some(value) => Some(value.parse::<u64>().map_err(|_| {
                    args.error(format!(
                        "MIB must be a number of MiB or `unlimited`, not {value:?}"
                    ))
                })?),
            };
            Ok(Command::CacheBudget {
                account_id: args.id(0)?,
                budget_mb,
            })
        },
    },
    Spec {
        name: "empty-trash",
        args: &["ACCOUNT"],
        required: 1,
        rest: false,
        options: &[YES, FORCE],
        summary: "Delete everything in an account's Trash for good",
        details: "Every message in the Trash is expunged on the server; this can't be \
                  undone. It asks first in a terminal; elsewhere it needs --yes.",
        group: Group::Accounts,
        available: true,
        build: |args| {
            Ok(Command::EmptyTrash {
                account_id: args.id(0)?,
                yes: args.flag(YES.name),
            })
        },
    },
    Spec {
        name: "dead-ops",
        args: &["ACCOUNT"],
        required: 0,
        rest: false,
        options: &[],
        summary: "List changes the server kept refusing",
        details: "",
        group: Group::Accounts,
        available: true,
        build: |args| {
            Ok(Command::DeadOps {
                account_id: args.optional_id(0)?,
            })
        },
    },
    Spec {
        name: "retry-op",
        args: &["OPERATION"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Queue a refused change again",
        details: "",
        group: Group::Accounts,
        available: true,
        build: |args| Ok(Command::RetryOp(args.id(0)?)),
    },
    Spec {
        name: "discard-op",
        args: &["OPERATION"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Drop a refused change",
        details: "",
        group: Group::Accounts,
        available: true,
        build: |args| Ok(Command::DiscardOp(args.id(0)?)),
    },
    Spec {
        name: "export-account",
        args: &["ACCOUNT", "PATH"],
        required: 2,
        rest: false,
        options: &[],
        summary: "Write an account's stored mail to an mbox file",
        details: "The mbox file is not encrypted. On Linux and macOS a new file is \
                  readable by you only.",
        group: Group::Accounts,
        available: true,
        build: |args| {
            Ok(Command::ExportAccount {
                account_id: args.id(0)?,
                path: PathBuf::from(args.arg(1).unwrap_or_default()),
            })
        },
    },
    // The mail store.
    Spec {
        name: "backup",
        args: &["DIRECTORY"],
        required: 0,
        rest: false,
        options: &[],
        summary: "Back up the mail store, keeping the 10 newest",
        details: "DIRECTORY defaults to `backups` beside the store. Backups are encrypted \
                  with the store's key.",
        group: Group::Store,
        available: true,
        build: |args| {
            Ok(Command::Backup {
                directory: args.arg(0).map(PathBuf::from),
            })
        },
    },
    Spec {
        name: "rotate-key",
        args: &[],
        required: 0,
        rest: false,
        options: &[],
        summary: "Re-encrypt the store (quit Zmail first)",
        details: "The store gets a new key and the old one is deleted, so backups made \
                  with it no longer open.",
        group: Group::Store,
        available: true,
        build: |_| Ok(Command::RotateKey),
    },
    Spec {
        name: "reset-store",
        args: &[],
        required: 0,
        rest: false,
        options: &[],
        summary: "Start over when the store's key is lost",
        details: "The unreadable store is moved aside, not deleted. It refuses while the \
                  store still unlocks.",
        group: Group::Store,
        available: true,
        build: |_| Ok(Command::ResetStore),
    },
    // Fixtures and development.
    Spec {
        name: "launch-ui-fixture",
        args: &["VIEW", "dark|light|system", "TEXT_SIZE"],
        required: 1,
        rest: false,
        options: &[],
        summary: "Open the window on synthetic mail in one UI state",
        details: "For the screenshot matrix: VIEW is a state such as empty, inbox, reader, \
                  compose, account-setup or onboarding-*, optionally at a text size in \
                  percent (100 to 200). Nothing is saved and nothing syncs.",
        group: Group::Developer,
        available: true,
        build: |args| {
            let view = ui_fixture::FixtureView::parse(args.arg(0).unwrap_or_default())
                .map_err(|error| args.error(format!("{error:#}")))?;
            let theme = ui_fixture::parse_theme(args.arg(1))
                .map_err(|error| args.error(format!("{error:#}")))?;
            let text_size = ui_fixture::parse_text_size(args.arg(2))
                .map_err(|error| args.error(format!("{error:#}")))?;
            Ok(Command::LaunchUiFixture {
                view,
                theme,
                text_size,
            })
        },
    },
    Spec {
        name: "launch-tui-fixture",
        args: &["VIEW"],
        required: 0,
        rest: false,
        options: &[],
        summary: "Open the terminal UI on synthetic mail",
        details: "VIEW is one of the desktop fixture's states (accounts by default). \
                  Nothing is saved and nothing syncs; the keychain is kept in memory.",
        group: Group::Developer,
        available: cfg!(feature = "tui"),
        build: |args| {
            let view = ui_fixture::FixtureView::parse(args.arg(0).unwrap_or("accounts"))
                .map_err(|error| args.error(format!("{error:#}")))?;
            Ok(Command::LaunchTuiFixture(view))
        },
    },
    Spec {
        name: "launch-imap-fixture",
        args: &["ENDPOINT", "USERNAME", "EMAIL"],
        required: 3,
        rest: false,
        options: &[],
        summary: "Open the window on a test IMAP server, in memory",
        details: "The password is read from the ZMAIL_FIXTURE_SECRET environment variable, \
                  or else from standard input; never from the command line. Mail and \
                  password stay in memory.",
        group: Group::Developer,
        available: true,
        build: |args| {
            Ok(Command::LaunchImapFixture {
                endpoint: args.arg(0).unwrap_or_default().to_owned(),
                username: args.arg(1).unwrap_or_default().to_owned(),
                email: args.arg(2).unwrap_or_default().to_owned(),
            })
        },
    },
];

/// The command named `name`.
pub(crate) fn find(name: &str) -> Option<&'static Spec> {
    COMMANDS.iter().find(|spec| spec.name == name)
}

/// A mistake on the command line: printed with the command's usage, exit
/// status 2.
pub(crate) struct UsageError {
    message: String,
    spec: Option<&'static Spec>,
}

impl UsageError {
    fn new(message: impl Into<String>, spec: Option<&'static Spec>) -> Self {
        Self {
            message: message.into(),
            spec,
        }
    }

    #[cfg(test)]
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Debug for UsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "error: {}", self.message)?;
        match self.spec {
            Some(spec) => {
                writeln!(formatter, "Usage: {}", spec.usage())?;
                write!(formatter, "Run `zmail-app {} --help` for more.", spec.name)
            }
            None => write!(formatter, "Run `zmail-app --help` for the commands."),
        }
    }
}

/// A command's arguments and options, as given.
pub(crate) struct Args {
    spec: &'static Spec,
    positional: Vec<String>,
    options: Vec<(&'static str, Option<String>)>,
}

impl Args {
    /// Sorts `words` (the line after the command's name) into arguments
    /// and options, refusing anything the command doesn't take.
    fn collect(spec: &'static Spec, words: &[String]) -> Result<Self, UsageError> {
        let mut args = Self {
            spec,
            positional: Vec::new(),
            options: Vec::new(),
        };
        let mut words = words.iter();
        let mut options_ended = false;
        while let Some(word) = words.next() {
            if options_ended {
                args.positional.push(word.clone());
                continue;
            }
            if word == "--" {
                options_ended = true;
                continue;
            }
            // Once a search query has started, `-word` is part of it.
            let in_rest = spec.rest && args.positional.len() + 1 >= spec.args.len();
            let is_option =
                word.starts_with("--") || (word.starts_with('-') && word.len() > 1 && !in_rest);
            if !is_option {
                args.positional.push(word.clone());
                continue;
            }
            let (name, inline) = match word.split_once('=') {
                Some((name, value)) if name.starts_with("--") => (name, Some(value)),
                _ => (word.as_str(), None),
            };
            let Some(option) = spec.options.iter().find(|option| option.name == name) else {
                if name == FORCE.name {
                    return Err(args.error(format!(
                        "--force is for the commands that sync or send ({})",
                        COMMANDS
                            .iter()
                            .filter(|spec| spec.takes(FORCE.name))
                            .map(|spec| spec.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                return Err(args.error(format!("{} takes no option {name:?}", spec.name)));
            };
            if args.options.iter().any(|(given, _)| *given == option.name) {
                return Err(args.error(format!("{} is given twice", option.name)));
            }
            let value =
                match (option.value, inline) {
                    (None, None) => None,
                    (None, Some(_)) => {
                        return Err(args.error(format!("{} takes no value", option.name)));
                    }
                    (Some(_), Some(value)) => Some(value.to_owned()),
                    (Some(placeholder), None) => Some(words.next().cloned().ok_or_else(|| {
                        args.error(format!("{} needs a {placeholder}", option.name))
                    })?),
                };
            args.options.push((option.name, value));
        }
        if let Some(missing) = spec.args.get(args.positional.len())
            && args.positional.len() < spec.required
        {
            return Err(args.error(format!("{} needs {missing}", spec.name)));
        }
        if !spec.rest
            && let Some(extra) = args.positional.get(spec.args.len())
        {
            return Err(args.error(format!("unexpected argument {extra:?}")));
        }
        Ok(args)
    }

    fn error(&self, message: impl Into<String>) -> UsageError {
        UsageError::new(message, Some(self.spec))
    }

    fn arg(&self, index: usize) -> Option<&str> {
        self.positional.get(index).map(String::as_str)
    }

    /// Every argument from `index` on, joined with spaces.
    fn rest(&self, index: usize) -> String {
        self.positional.get(index..).unwrap_or_default().join(" ")
    }

    fn flag(&self, name: &str) -> bool {
        self.options.iter().any(|(given, _)| *given == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(given, _)| *given == name)
            .and_then(|(_, value)| value.as_deref())
    }

    /// The id at `index`, which the command requires.
    fn id(&self, index: usize) -> Result<i64, UsageError> {
        self.optional_id(index)?.ok_or_else(|| {
            self.error(format!(
                "{} needs {}",
                self.spec.name,
                self.spec.args.get(index).copied().unwrap_or("an id")
            ))
        })
    }

    /// The id at `index`, if given: a positive whole number.
    fn optional_id(&self, index: usize) -> Result<Option<i64>, UsageError> {
        let Some(value) = self.arg(index) else {
            return Ok(None);
        };
        let label = self.spec.args.get(index).copied().unwrap_or("the id");
        let label = label.rsplit('|').next().unwrap_or(label);
        match value.parse::<i64>() {
            Ok(id) if id > 0 => Ok(Some(id)),
            _ => Err(self.error(format!("{label} must be a number, not {value:?}"))),
        }
    }
}

/// Parses the command line after the program's name: the command, and
/// whether `--force` was given.
pub(crate) fn parse(args: Vec<String>) -> Result<(Command, bool), UsageError> {
    let options_end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let before_options_end = &args[..options_end];
    if before_options_end
        .iter()
        .any(|arg| arg == "--help" || arg == "-h")
    {
        // Help wins wherever it is: `rotate-key --help` must never rotate.
        let spec = before_options_end.iter().find_map(|arg| find(arg));
        return Ok((Command::Help(spec), false));
    }
    let Some(name) = args.first() else {
        return Ok((Command::Gui, false));
    };
    match name.as_str() {
        "help" => {
            let spec =
                match args.get(1) {
                    None => None,
                    Some(name) => Some(find(name).ok_or_else(|| {
                        UsageError::new(format!("unknown command {name:?}"), None)
                    })?),
                };
            if let Some(extra) = args.get(2) {
                return Err(UsageError::new(
                    format!("unexpected argument {extra:?}"),
                    None,
                ));
            }
            return Ok((Command::Help(spec), false));
        }
        "--version" | "-V" => {
            if let Some(extra) = args.get(1) {
                return Err(UsageError::new(
                    format!("unexpected argument {extra:?}"),
                    None,
                ));
            }
            return Ok((Command::Version, false));
        }
        // The system's mail link handler: `zmail-app mailto:...`.
        url if url.starts_with("mailto:") => {
            if let Some(extra) = args.get(1) {
                return Err(UsageError::new(
                    format!("unexpected argument {extra:?} after the mailto: link"),
                    None,
                ));
            }
            return Ok((Command::Mailto(url.to_owned()), false));
        }
        _ => {}
    }
    let spec =
        find(name).ok_or_else(|| UsageError::new(format!("unknown command {name:?}"), None))?;
    if !spec.available {
        return Err(UsageError::new(
            format!("this build of Zmail has no `{}` command", spec.name),
            None,
        ));
    }
    let args = Args::collect(spec, &args[1..])?;
    let command = (spec.build)(&args)?;
    Ok((command, args.flag(FORCE.name)))
}

/// The column the help's descriptions start at.
const HELP_COLUMN: usize = 30;

/// `zmail-app --help`.
pub(crate) fn general_help() -> String {
    let mut help = format!(
        "Zmail {}: email on the desktop and in the terminal.\n\n\
         Usage: zmail-app [COMMAND] [ARGUMENTS] [OPTIONS]\n\
         With no command, zmail-app opens the desktop app.\n",
        env!("CARGO_PKG_VERSION")
    );
    for group in Group::ALL {
        help.push_str(&format!("\n{}:\n", group.heading()));
        for spec in COMMANDS
            .iter()
            .filter(|spec| spec.group == group && spec.available)
        {
            let name = format!("  {}{}", spec.name, spec.argument_list());
            push_entry(&mut help, &name, spec.summary);
        }
    }
    help.push_str("\nOptions:\n");
    push_entry(
        &mut help,
        "  -h, --help",
        "Show this help, or after a command, its help",
    );
    push_entry(&mut help, "  -V, --version", "Show the version");
    help.push_str(
        "\nOnly one Zmail process syncs a store at a time. While the desktop app or\n\
         `zmail-app tui` syncs it, sync, index and process-outbox stop with an error,\n\
         and send, retry-outbox, compose, add-imap, add-jmap and empty-trash do their\n\
         part and leave the syncing and sending to that Zmail. --force runs them\n\
         here anyway, beside it.\n",
    );
    help
}

/// `zmail-app COMMAND --help`.
pub(crate) fn command_help(spec: &Spec) -> String {
    let mut help = format!("Usage: {}\n\n{}.\n", spec.usage(), spec.summary);
    if !spec.details.is_empty() {
        help.push_str(&format!("{}\n", wrap(spec.details, 76)));
    }
    help.push_str("\nOptions:\n");
    for option in spec.options {
        let name = match option.value {
            Some(value) => format!("  {} {value}", option.name),
            None => format!("  {}", option.name),
        };
        push_entry(&mut help, &name, option.help);
    }
    push_entry(&mut help, "  -h, --help", "Show this help");
    help
}

/// One help line: `name` and `text` from [`HELP_COLUMN`], or on the next
/// line when `name` is too long.
fn push_entry(help: &mut String, name: &str, text: &str) {
    if name.len() + 2 <= HELP_COLUMN {
        help.push_str(&format!("{name:<HELP_COLUMN$}{text}\n"));
    } else {
        help.push_str(&format!("{name}\n{:HELP_COLUMN$}{text}\n", ""));
    }
}

/// `text` wrapped at `width` columns.
fn wrap(text: &str, width: usize) -> String {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn parse_line(line: &str) -> Result<(Command, bool), UsageError> {
        parse(args(line))
    }

    fn refused(line: &str) -> String {
        match parse_line(line) {
            Ok(_) => panic!("{line:?} parsed"),
            Err(error) => error.message().to_owned(),
        }
    }

    /// Arguments that satisfy `spec`: a valid value for each required one.
    fn minimal_line(spec: &Spec) -> String {
        let mut line = spec.name.to_owned();
        for arg in &spec.args[..spec.required] {
            line.push(' ');
            line.push_str(match *arg {
                "VIEW" => "inbox",
                "PATH" | "DIRECTORY" => "some-path",
                "ENDPOINT" => "imap://127.0.0.1:3143",
                "USERNAME" | "EMAIL" => "someone@example.com",
                "URL" => "someone@example.com",
                "QUERY" => "invoice",
                _ => "7",
            });
        }
        line
    }

    #[test]
    fn help_anywhere_never_runs_a_command() {
        for spec in COMMANDS.iter().filter(|spec| spec.available) {
            let line = minimal_line(spec);
            assert!(parse_line(&line).is_ok(), "{line:?} parses");
            for help in ["--help", "-h"] {
                // At the end, straight after the name, and among the
                // arguments: always the command's help.
                for line in [
                    format!("{line} {help}"),
                    format!("{} {help}", spec.name),
                    format!("{} {help} 1 2 3", spec.name),
                ] {
                    match parse_line(&line) {
                        Ok((Command::Help(Some(shown)), false)) => {
                            assert_eq!(shown.name, spec.name, "{line:?}")
                        }
                        _ => panic!("{line:?} is not the command's help"),
                    }
                }
            }
        }
        assert!(matches!(
            parse_line("--help"),
            Ok((Command::Help(None), false))
        ));
        assert!(matches!(
            parse_line("help empty-trash"),
            Ok((Command::Help(Some(spec)), false)) if spec.name == "empty-trash"
        ));
        // The cases from the audit, by name.
        for line in [
            "rotate-key --help",
            "reset-store --help",
            "empty-trash 1 --help",
            "backup --help",
            "tui -h",
            "add-imap --help",
        ] {
            assert!(
                matches!(parse_line(line), Ok((Command::Help(Some(_)), _))),
                "{line}"
            );
        }
    }

    #[test]
    fn every_command_refuses_surplus_arguments_and_unknown_options() {
        for spec in COMMANDS.iter().filter(|spec| spec.available) {
            let line = minimal_line(spec);
            let error = refused(&format!("{line} --no-such-option"));
            assert!(error.contains("--no-such-option"), "{line}: {error}");
            if spec.rest {
                continue;
            }
            let mut surplus = line.clone();
            for _ in spec.required..=spec.args.len() {
                surplus.push_str(" 9");
            }
            let error = refused(&surplus);
            assert!(error.contains("unexpected argument"), "{surplus}: {error}");
        }
    }

    #[test]
    fn every_command_with_arguments_requires_them() {
        for spec in COMMANDS
            .iter()
            .filter(|spec| spec.available && spec.required > 0)
        {
            let error = refused(spec.name);
            assert!(error.contains("needs"), "{}: {error}", spec.name);
        }
    }

    #[test]
    fn the_help_lists_every_command_once() {
        let help = general_help();
        for spec in COMMANDS.iter().filter(|spec| spec.available) {
            let entry = format!("\n  {}{}", spec.name, spec.argument_list());
            assert_eq!(help.matches(&entry).count(), 1, "{entry:?} in\n{help}");
            assert!(!spec.summary.is_empty(), "{} has a summary", spec.name);
            let own = command_help(spec);
            assert!(
                own.starts_with(&format!("Usage: {}", spec.usage())),
                "{own}"
            );
        }
        // Developer commands come last.
        let fixtures = help.find("Fixtures and development:").expect("the heading");
        assert!(help.find("\n  reset-store").expect("listed") < fixtures);
        assert!(help.find("\n  launch-ui-fixture").expect("listed") > fixtures);
        // No line is wider than a terminal.
        for line in help.lines() {
            assert!(line.chars().count() <= 80, "{line:?}");
        }
    }

    #[test]
    fn version_and_unknown_commands() {
        assert!(matches!(parse_line("--version"), Ok((Command::Version, _))));
        assert!(matches!(parse_line("-V"), Ok((Command::Version, _))));
        assert!(refused("-V extra").contains("unexpected argument"));
        assert!(refused("--frobnicate").contains("unknown command"));
        assert!(refused("frobnicate").contains("unknown command"));
        assert!(matches!(parse(Vec::new()), Ok((Command::Gui, false))));
    }

    #[test]
    fn force_is_taken_by_the_commands_that_sync_or_send_only() {
        let (command, force) = parse_line("sync --force all").expect("it parses");
        assert!(matches!(command, Command::Sync(None)));
        assert!(force);
        let (command, force) = parse_line("process-outbox 3 --force").expect("it parses");
        assert!(matches!(command, Command::ProcessOutbox(Some(3))));
        assert!(force);
        let (command, force) = parse_line("sync 2").expect("it parses");
        assert!(matches!(command, Command::Sync(Some(2))));
        assert!(!force);
        // Reading the store needs no lock, so there is nothing to force.
        assert!(refused("list-accounts --force").contains("--force is for the commands"));
        assert!(refused("search 1 --force invoice").contains("--force is for the commands"));
    }

    #[test]
    fn arguments_are_checked_with_plain_messages() {
        assert_eq!(refused("sync abc"), "ACCOUNT must be a number, not \"abc\"");
        assert_eq!(refused("read 0"), "THREAD must be a number, not \"0\"");
        assert_eq!(refused("move 1"), "move needs MAILBOX");
        assert_eq!(refused("read 1 2"), "unexpected argument \"2\"");
        assert!(refused("read -1").contains("takes no option \"-1\""));
        assert!(refused("cache-budget 1 lots").contains("MIB must be a number"));
        assert!(refused("save-attachment 1 here --dest there").contains("not both"));
        assert!(refused("save-attachment 1 --dest").contains("--dest needs a DIRECTORY"));
        assert!(refused("empty-trash 1 --yes --yes").contains("given twice"));
        assert!(refused("empty-trash 1 --yes=no").contains("takes no value"));
        assert!(refused("mailto:a@example.com extra").contains("unexpected argument"));
    }

    #[test]
    fn options_and_arguments_are_read() {
        let Ok((
            Command::SaveAttachment {
                attachment_id,
                destination,
                overwrite,
            },
            _,
        )) = parse_line("save-attachment 4 --dest=out --overwrite")
        else {
            panic!("save-attachment parses");
        };
        assert_eq!(attachment_id, 4);
        assert_eq!(destination, Some(PathBuf::from("out")));
        assert!(overwrite);
        assert!(matches!(
            parse_line("empty-trash 2"),
            Ok((
                Command::EmptyTrash {
                    account_id: 2,
                    yes: false
                },
                false
            ))
        ));
        assert!(matches!(
            parse_line("empty-trash --yes 2 --force"),
            Ok((
                Command::EmptyTrash {
                    account_id: 2,
                    yes: true
                },
                true
            ))
        ));
        assert!(matches!(
            parse_line("add-imap --allow-cleartext"),
            Ok((
                Command::AddAccount {
                    protocol: ProtocolKind::Imap,
                    allow_cleartext: true
                },
                false
            ))
        ));
        assert!(matches!(
            parse_line("mailto someone@example.com"),
            Ok((Command::Mailto(url), _)) if url == "mailto:someone@example.com"
        ));
        assert!(matches!(
            parse_line("mailto:someone@example.com?subject=-h"),
            Ok((Command::Mailto(_), _))
        ));
    }

    #[test]
    fn move_takes_a_mailbox_id_role_or_name() {
        for (line, expected) in [
            ("move 4 7", "7"),
            ("move 4 archive", "archive"),
            ("move 4 Projects/Website", "Projects/Website"),
        ] {
            let Ok((Command::MoveThread(4, mailbox), _)) = parse_line(line) else {
                panic!("{line} parses");
            };
            assert_eq!(mailbox, expected);
        }
        assert!(matches!(
            parse_line("mailboxes 2"),
            Ok((Command::ListMailboxes(2), _))
        ));
    }

    #[test]
    fn a_search_query_keeps_its_words() {
        let Ok((
            Command::Search {
                account_id,
                query,
                server,
            },
            _,
        )) = parse_line("search --server 3 from:ada -is:flagged invoice")
        else {
            panic!("search parses");
        };
        assert_eq!(account_id, 3);
        assert_eq!(query, "from:ada -is:flagged invoice");
        assert!(server);
        // After `--`, even `--help` is a query word.
        let Ok((Command::Search { query, server, .. }, _)) =
            parse_line("search 3 -- --help --server")
        else {
            panic!("search parses");
        };
        assert_eq!(query, "--help --server");
        assert!(!server);
        assert!(refused("search 3").contains("search needs QUERY"));
    }
}
