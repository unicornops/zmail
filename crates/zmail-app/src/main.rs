//! Zmail Phase 3 desktop bootstrap and account setup CLI.

use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use zmail_assets::ZmailAssets;
use zmail_core::{
    AppBootstrap, AuthKind, CachePolicySummary, DraftUpdate, NewAccount, NewDraft, NewIdentity,
    OutgoingConfig, ProtocolKind, SubmissionSecurity, ThemeAppearance, ThemeSummary, UpstreamPins,
};
use zmail_security::{InMemorySecretStore, KeyringSecretStore, SecretSlot, SecretStore};
use zmail_store::{LocalStore, MigrationProgress, StoreOpenError};
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_theme::ThemeFamily;
use zmail_ui::InitialView;

mod ui_fixture;

const PHASE0_DARK_THEME: &str = zmail_theme::bundled::DARK;
const PHASE0_LIGHT_THEME: &str = zmail_theme::bundled::LIGHT;
/// Zed commit GPUI is built from, read from `Cargo.lock` by `build.rs`.
const ZED_UPSTREAM_COMMIT: &str = env!("ZMAIL_ZED_COMMIT");
/// Zed stable release tag GPUI is pinned to (see docs/upstream/zed.md).
const GPUI_VERSION: &str = env!("ZMAIL_ZED_TAG");

fn main() -> Result<()> {
    match Command::from_env()? {
        Command::Gui => launch_gui(),
        Command::Mailto(url) => launch_gui_with_mailto(&url),
        Command::LaunchImapFixture {
            endpoint,
            username,
            secret,
            email,
        } => launch_imap_fixture(&endpoint, &username, &secret, &email),
        Command::LaunchUiFixture {
            view,
            theme,
            text_size,
        } => launch_ui_fixture(view, theme, text_size),
        Command::AddImap => add_account(ProtocolKind::Imap),
        Command::AddJmap => add_account(ProtocolKind::Jmap),
        Command::ListAccounts => list_accounts(),
        Command::SyncAll => sync_accounts(None),
        Command::SyncAccount(account_id) => sync_accounts(Some(account_id)),
        Command::Search {
            account_id,
            query,
            server,
        } => search_threads(account_id, &query, server),
        Command::IndexAccount(account_id) => index_account(account_id),
        Command::ReadThread(thread_id) => read_thread(thread_id),
        Command::ListAttachments(thread_id) => list_attachments(thread_id),
        Command::SaveAttachment {
            attachment_id,
            destination,
        } => save_attachment(attachment_id, destination.as_deref()),
        Command::FlagThread(thread_id, flagged) => flag_thread(thread_id, flagged),
        Command::MarkThreadRead(thread_id, read) => mark_thread_read(thread_id, read),
        Command::MoveThread(thread_id, mailbox_id) => move_thread(thread_id, mailbox_id),
        Command::ArchiveThread(thread_id) => archive_thread(thread_id),
        Command::DeleteThread(thread_id) => delete_thread(thread_id),
        Command::Compose(account_id, identity_id) => compose_draft(account_id, identity_id),
        Command::ListDrafts(account_id) => list_drafts(account_id),
        Command::ShowDraft(draft_id) => show_draft(draft_id),
        Command::SendDraft(draft_id) => send_draft(draft_id),
        Command::ListOutbox(account_id) => list_outbox(account_id),
        Command::RetryOutbox(job_id) => retry_outbox_job(job_id),
        Command::AddAttachment(draft_id, path) => add_attachment(draft_id, &path),
        Command::RemoveAttachment(attachment_id) => remove_attachment(attachment_id),
        Command::ProcessOutbox(account_id) => process_outbox(account_id),
        Command::Backup { directory } => create_backup(directory.as_deref()),
        Command::ResetStore => reset_unreadable_store(),
        Command::RotateKey => rotate_store_key(),
        Command::ExportAccount { account_id, path } => export_account(account_id, &path),
        Command::DeadOps { account_id } => list_dead_ops(account_id),
        Command::RetryOp(op_id) => retry_dead_op(op_id),
        Command::DiscardOp(op_id) => discard_dead_op(op_id),
        Command::EmptyTrash(account_id) => empty_trash(account_id),
        Command::CacheBudget {
            account_id,
            budget_mb,
        } => set_cache_budget(account_id, budget_mb),
        Command::Help => {
            print_help();
            Ok(())
        }
    }
}

enum Command {
    Gui,
    Mailto(String),
    LaunchImapFixture {
        endpoint: String,
        username: String,
        secret: String,
        email: String,
    },
    LaunchUiFixture {
        view: ui_fixture::FixtureView,
        theme: Option<ThemeAppearance>,
        text_size: Option<u16>,
    },
    Backup {
        directory: Option<String>,
    },
    ResetStore,
    RotateKey,
    DeadOps {
        account_id: Option<i64>,
    },
    RetryOp(i64),
    DiscardOp(i64),
    EmptyTrash(i64),
    CacheBudget {
        account_id: i64,
        budget_mb: Option<u64>,
    },
    ExportAccount {
        account_id: i64,
        path: String,
    },
    AddImap,
    AddJmap,
    ListAccounts,
    SyncAll,
    SyncAccount(i64),
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
        destination: Option<String>,
    },
    FlagThread(i64, bool),
    MarkThreadRead(i64, bool),
    MoveThread(i64, i64),
    ArchiveThread(i64),
    DeleteThread(i64),
    Compose(i64, Option<i64>),
    ListDrafts(Option<i64>),
    ShowDraft(i64),
    SendDraft(i64),
    ListOutbox(Option<i64>),
    RetryOutbox(i64),
    AddAttachment(i64, PathBuf),
    RemoveAttachment(i64),
    ProcessOutbox(Option<i64>),
    Help,
}

impl Command {
    fn from_env() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let Some(command) = args.next() else {
            return Ok(Self::Gui);
        };

        match command.as_str() {
            "launch-imap-fixture" => Ok(Self::LaunchImapFixture {
                endpoint: args
                    .next()
                    .context("launch-imap-fixture requires an IMAP endpoint")?,
                username: args
                    .next()
                    .context("launch-imap-fixture requires a username")?,
                secret: args
                    .next()
                    .context("launch-imap-fixture requires a password or app password")?,
                email: args
                    .next()
                    .context("launch-imap-fixture requires a primary identity email")?,
            }),
            "launch-ui-fixture" => {
                let view = ui_fixture::FixtureView::parse(
                    &args.next().context("launch-ui-fixture requires a view")?,
                )?;
                let theme = ui_fixture::parse_theme(args.next().as_deref())?;
                let text_size = ui_fixture::parse_text_size(args.next().as_deref())?;
                Ok(Self::LaunchUiFixture {
                    view,
                    theme,
                    text_size,
                })
            }
            "add-imap" => Ok(Self::AddImap),
            "add-jmap" => Ok(Self::AddJmap),
            "list-accounts" => Ok(Self::ListAccounts),
            "sync" => {
                let next = args.next();
                if let Some(next) = next {
                    if next == "all" {
                        Ok(Self::SyncAll)
                    } else {
                        Ok(Self::SyncAccount(
                            next.parse::<i64>()
                                .with_context(|| format!("invalid account id {next}"))?,
                        ))
                    }
                } else {
                    Ok(Self::SyncAll)
                }
            }
            "search" => {
                let mut account = args.next().context("search requires an account id")?;
                let server = account == "--server";
                if server {
                    account = args.next().context("search requires an account id")?;
                }
                let account_id = account
                    .parse::<i64>()
                    .context("search account id must be an integer")?;
                let query = args.collect::<Vec<_>>().join(" ");
                if query.trim().is_empty() {
                    bail!("search requires a query");
                }
                Ok(Self::Search {
                    account_id,
                    query,
                    server,
                })
            }
            "index" => Ok(Self::IndexAccount(
                args.next()
                    .context("index requires an account id")?
                    .parse::<i64>()
                    .context("index account id must be an integer")?,
            )),
            "read" => Ok(Self::ReadThread(
                args.next()
                    .context("read requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
            )),
            "attachments" => Ok(Self::ListAttachments(
                args.next()
                    .context("attachments requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
            )),
            "save-attachment" => {
                let attachment_id = args
                    .next()
                    .context("save-attachment requires an attachment id")?
                    .parse::<i64>()
                    .context("attachment id must be an integer")?;
                let destination = match args.next().as_deref() {
                    Some(path) if path != "--dest" => Some(path.to_owned()),
                    Some("--dest") => Some(
                        args.next()
                            .context("save-attachment --dest requires a directory")?,
                    ),
                    _ => None,
                };
                Ok(Self::SaveAttachment {
                    attachment_id,
                    destination,
                })
            }
            "flag" => Ok(Self::FlagThread(
                args.next()
                    .context("flag requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
                true,
            )),
            "unflag" => Ok(Self::FlagThread(
                args.next()
                    .context("unflag requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
                false,
            )),
            "mark-read" => Ok(Self::MarkThreadRead(
                args.next()
                    .context("mark-read requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
                true,
            )),
            "mark-unread" => Ok(Self::MarkThreadRead(
                args.next()
                    .context("mark-unread requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
                false,
            )),
            "move" => {
                let thread_id = args
                    .next()
                    .context("move requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?;
                let mailbox_id = args
                    .next()
                    .context("move requires a destination mailbox id")?
                    .parse::<i64>()
                    .context("destination mailbox id must be an integer")?;
                Ok(Self::MoveThread(thread_id, mailbox_id))
            }
            "archive" => Ok(Self::ArchiveThread(
                args.next()
                    .context("archive requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
            )),
            "delete" => Ok(Self::DeleteThread(
                args.next()
                    .context("delete requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
            )),
            "compose" => {
                let account_id = args
                    .next()
                    .context("compose requires an account id")?
                    .parse::<i64>()
                    .context("account id must be an integer")?;
                let identity_id = args
                    .next()
                    .map(|value| {
                        value
                            .parse::<i64>()
                            .context("identity id must be an integer")
                    })
                    .transpose()?;
                Ok(Self::Compose(account_id, identity_id))
            }
            "drafts" => Ok(Self::ListDrafts(parse_optional_id(
                args.next(),
                "account id",
            )?)),
            "draft" => Ok(Self::ShowDraft(
                args.next()
                    .context("draft requires a draft id")?
                    .parse::<i64>()
                    .context("draft id must be an integer")?,
            )),
            "send" => Ok(Self::SendDraft(
                args.next()
                    .context("send requires a draft id")?
                    .parse::<i64>()
                    .context("draft id must be an integer")?,
            )),
            "outbox" => Ok(Self::ListOutbox(parse_optional_id(
                args.next(),
                "account id",
            )?)),
            "retry-outbox" => Ok(Self::RetryOutbox(
                args.next()
                    .context("retry-outbox requires a job id")?
                    .parse::<i64>()
                    .context("job id must be an integer")?,
            )),
            "attach" => Ok(Self::AddAttachment(
                args.next()
                    .context("attach requires a draft id")?
                    .parse::<i64>()
                    .context("draft id must be an integer")?,
                PathBuf::from(args.next().context("attach requires a file path")?),
            )),
            "detach" => Ok(Self::RemoveAttachment(
                args.next()
                    .context("detach requires an attachment id")?
                    .parse::<i64>()
                    .context("attachment id must be an integer")?,
            )),
            "process-outbox" => Ok(Self::ProcessOutbox(parse_optional_id(
                args.next(),
                "account id",
            )?)),
            "backup" => Ok(Self::Backup {
                directory: args.next(),
            }),
            "reset-store" => Ok(Self::ResetStore),
            "rotate-key" => Ok(Self::RotateKey),
            "dead-ops" => Ok(Self::DeadOps {
                account_id: parse_optional_id(args.next(), "account id")?,
            }),
            "retry-op" => Ok(Self::RetryOp(
                args.next()
                    .context("retry-op requires a dead operation id")?
                    .parse::<i64>()
                    .context("operation id must be an integer")?,
            )),
            "discard-op" => Ok(Self::DiscardOp(
                args.next()
                    .context("discard-op requires a dead operation id")?
                    .parse::<i64>()
                    .context("operation id must be an integer")?,
            )),
            "empty-trash" => Ok(Self::EmptyTrash(
                args.next()
                    .context("empty-trash requires an account id")?
                    .parse::<i64>()
                    .context("account id must be an integer")?,
            )),
            "cache-budget" => Ok(Self::CacheBudget {
                account_id: args
                    .next()
                    .context("cache-budget requires an account id")?
                    .parse::<i64>()
                    .context("account id must be an integer")?,
                budget_mb: match args.next().as_deref() {
                    None | Some("unlimited") => None,
                    Some(value) => Some(
                        value
                            .parse::<u64>()
                            .context("the budget must be a number of MiB or 'unlimited'")?,
                    ),
                },
            }),
            "export-account" => Ok(Self::ExportAccount {
                account_id: args
                    .next()
                    .context("export-account requires an account id")?
                    .parse::<i64>()
                    .context("account id must be an integer")?,
                path: args
                    .next()
                    .context("export-account requires an output path")?,
            }),
            "mailto" => Ok(Self::Mailto(format!(
                "mailto:{}",
                args.next().context("mailto requires a URL body")?
            ))),
            other if other.starts_with("mailto:") => Ok(Self::Mailto(other.to_owned())),
            "help" | "--help" | "-h" => Ok(Self::Help),
            other => bail!("unknown command {other}"),
        }
    }
}

/// Launches the GUI with a compose draft pre-filled from a mailto URL.
///
/// Malformed URLs fall back to a plain GUI launch with a warning, and a
/// missing account configuration surfaces the setup flow instead.
fn launch_gui_with_mailto(url: &str) -> Result<()> {
    match parse_mailto(url) {
        Some(mailto) => {
            let store = open_store().context("failed to open the local store")?;
            let Some(target) = store.list_sync_targets()?.first().cloned() else {
                eprintln!("no configured account exists; opening the app without a draft");
                return launch_gui();
            };
            let identity = target
                .identities
                .iter()
                .find(|identity| identity.is_primary)
                .or_else(|| target.identities.first())
                .context("the configured account has no identities")?;
            let draft_id = store.create_draft(&NewDraft {
                account_id: target.id,
                identity_id: identity.id,
                to: mailto.to,
                cc: mailto.cc,
                bcc: mailto.bcc,
                subject: mailto.subject,
                body: mailto.body,
            })?;
            launch_gui_with_initial_draft(Some(draft_id))
        }
        None => {
            eprintln!("warning: ignoring malformed mailto URL {url}");
            launch_gui()
        }
    }
}

/// A mailto URL decomposed into draft fields (RFC 6068).
struct MailtoDraft {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: String,
    body: String,
}

/// Parses an RFC 6068 mailto URL into draft fields.
fn parse_mailto(raw: &str) -> Option<MailtoDraft> {
    let rest = raw.strip_prefix("mailto:")?;
    let (recipients, query) = match rest.split_once('?') {
        Some((recipients, query)) => (recipients, Some(query)),
        None => (rest, None),
    };

    let mut draft = MailtoDraft {
        to: split_addresses(recipients),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: String::new(),
        body: String::new(),
    };

    let Some(query) = query else {
        return Some(draft);
    };
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let value = percent_decode(value);
        match name.to_ascii_lowercase().as_str() {
            "to" => {
                let decoded = split_addresses(&value);
                draft.to.extend(decoded);
            }
            "cc" => draft.cc = split_addresses(&value),
            "bcc" => draft.bcc = split_addresses(&value),
            "subject" => draft.subject = value,
            "body" => draft.body = value,
            _ => {}
        }
    }
    Some(draft)
}

/// Splits a comma-separated recipient list, dropping empty entries.
fn split_addresses(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Percent-decodes a mailto component, mapping `+` to a space.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                if let Some(hex) = hex
                    && let Ok(byte) = u8::from_str_radix(hex, 16)
                {
                    decoded.push(byte);
                    index += 3;
                } else {
                    decoded.push(b'%');
                    index += 1;
                }
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn launch_gui() -> Result<()> {
    launch_gui_with_initial_draft(None)
}

fn launch_gui_with_initial_draft(initial_draft_id: Option<i64>) -> Result<()> {
    install_panic_log();
    let runtime = BackgroundRuntime::start(4)?;
    let handle = runtime.handle().clone();
    let bootstrap = build_bootstrap(runtime.summary())?;
    let initial_view = initial_draft_id.map_or(InitialView::Inbox, InitialView::Draft);
    // The window unlocks the encrypted store first (ADR 0005), showing the
    // migration's progress or what went wrong; sync starts once it is open.
    zmail_ui::run_unlocking(
        ZmailAssets,
        bootstrap,
        store_path()?,
        Arc::new(KeyringSecretStore),
        move |store| {
            let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
            // Only one process syncs a store (ADR 0007): when a terminal UI
            // already does, this window reads the store and leaves sync to
            // it, and takes sync over if it quits first.
            let sync_service = SyncService::with_sync_lock(handle.clone(), store, secrets.clone())?;
            // Proton accounts talk to Proton's production API with Bridge's
            // pins and app version, and a User-Agent naming Zmail (ADR 0004).
            #[cfg(feature = "proton")]
            sync_service.configure_proton(zmail_sync::proton_production_config(env!(
                "CARGO_PKG_VERSION"
            ))?)?;
            let _ = sync_service.bootstrap_existing_accounts();
            let _ = sync_service.start_mail_watcher();
            sync_service.start_background_indexing();
            Ok(zmail_ui::Started {
                sync_service,
                secrets,
                initial_view,
            })
        },
    );
    drop(runtime);
    Ok(())
}

fn launch_imap_fixture(endpoint: &str, username: &str, secret: &str, email: &str) -> Result<()> {
    install_panic_log();
    validate_endpoint(ProtocolKind::Imap, endpoint)?;

    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(LocalStore::open_ephemeral().context("failed to open the fixture store")?);
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    let account_id = store
        .insert_account(&fixture_imap_account(endpoint, username, email))
        .context("failed to insert the IMAP fixture account")?;
    secrets
        .set_account_secret(account_id, secret)
        .with_context(|| format!("failed to store fixture credentials for account {account_id}"))?;

    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
    sync_service
        .sync_account_and_wait(account_id)
        .with_context(|| format!("failed to sync the IMAP fixture account {account_id}"))?;

    let bootstrap = build_bootstrap(runtime.summary())?;
    zmail_ui::run(
        ZmailAssets,
        bootstrap,
        store,
        sync_service,
        secrets,
        InitialView::Inbox,
    );
    drop(runtime);
    Ok(())
}

/// Opens the window in one seeded UI state for the screenshot matrix.
///
/// The store is in memory and seeded with synthetic mail, secrets stay in
/// memory and nothing syncs, so the launch needs no network or keychain.
/// `theme` pins the light or dark variant instead of following the system
/// appearance, and `text_size` saves that text size (in percent).
fn launch_ui_fixture(
    view: ui_fixture::FixtureView,
    theme: Option<ThemeAppearance>,
    text_size: Option<u16>,
) -> Result<()> {
    install_panic_log();
    let runtime = BackgroundRuntime::start(4)?;
    if let ui_fixture::FixtureView::Unlock(state) = view {
        let bootstrap = fixture_bootstrap(runtime.summary(), theme)?;
        zmail_ui::run_unlock_fixture(ZmailAssets, bootstrap, state);
        return Ok(());
    }
    let store = Arc::new(LocalStore::open_ephemeral().context("failed to open the fixture store")?);
    let initial_view = ui_fixture::seed(&store, view, ui_fixture::anchor_time())?;
    if view.high_contrast() {
        zmail_ui::simulate_high_contrast();
    }
    if let Some(size) = text_size {
        store.update_ui_preferences(|preferences| preferences.text_size_percent = size)?;
    }
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());

    let bootstrap = fixture_bootstrap(runtime.summary(), theme)?;
    zmail_ui::run(
        ZmailAssets,
        bootstrap,
        store,
        sync_service,
        secrets,
        initial_view,
    );
    drop(runtime);
    Ok(())
}

/// The app bootstrap for a fixture launch, pinned to `theme`'s light or
/// dark variant when one is given.
fn fixture_bootstrap(
    runtime: zmail_core::RuntimeSummary,
    theme: Option<ThemeAppearance>,
) -> Result<zmail_core::AppBootstrap> {
    let mut bootstrap = build_bootstrap(runtime)?;
    if let Some(appearance) = theme {
        if bootstrap.theme.appearance != appearance {
            std::mem::swap(&mut bootstrap.theme, &mut bootstrap.alternate_theme);
        }
        // With both slots on the same variant, following the system
        // appearance has nothing to switch to.
        bootstrap.alternate_theme = bootstrap.theme.clone();
    }
    Ok(bootstrap)
}

fn add_account(protocol: ProtocolKind) -> Result<()> {
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let (account, secret) = prompt_account(protocol)?;
    let account_id = store.insert_account(&account)?;
    secret.store(store.as_ref(), secrets.as_ref(), account_id)?;
    let budget_mb = prompt_optional("Disk budget in MiB (empty for unlimited)")?
        .map(|value| {
            value
                .parse::<u64>()
                .context("the disk budget must be a number of MiB")
        })
        .transpose()?;
    if let Some(budget_mb) = budget_mb {
        store.set_account_disk_budget(account_id, Some(budget_mb))?;
    }

    let runtime = BackgroundRuntime::start(4)?;
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
    match sync_service.sync_account_and_wait(account_id) {
        Ok(()) => {
            println!(
                "Account {account_id} added and synced: {} ({})",
                account.display_name, account.protocol
            );
            Ok(())
        }
        Err(error) => {
            eprintln!("Account {account_id} was stored, but the initial sync failed: {error:#}");
            Err(error)
        }
    }
}

fn list_accounts() -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    for account in store.list_accounts()? {
        println!(
            "#{} {} [{}] {}",
            account.id, account.display_name, account.protocol, account.endpoint
        );
        println!("  user: {}", account.username);
        println!("  identities: {}", account.identity_count);
        println!(
            "  sync: {} ({})",
            account.sync_status.phase, account.sync_status.detail
        );
    }
    Ok(())
}

fn sync_accounts(account_id: Option<i64>) -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    if let Some(account_id) = account_id {
        sync_service.sync_account_and_wait(account_id)?;
        println!("Finished syncing account {account_id}.");
    } else {
        sync_service.sync_all_and_wait()?;
        println!("Finished syncing all configured accounts.");
    }

    Ok(())
}

fn index_account(account_id: i64) -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);

    let report = sync_service.index_account_and_wait(account_id)?;
    let progress = store.index_progress(account_id)?;
    println!(
        "Indexed {} bodies, skipped {}, evicted {} bytes.",
        report.indexed, report.skipped, report.evicted_bytes
    );
    if let Some(pause) = report.paused {
        println!("Paused: {pause:?}.");
    }
    println!(
        "Account {account_id}: {} of {} messages searchable, {} pending, {} skipped.",
        progress.indexed,
        progress.messages,
        progress.pending(),
        progress.skipped
    );
    Ok(())
}

fn search_threads(account_id: i64, query: &str, server: bool) -> Result<()> {
    let store = Arc::new(open_store().context("failed to open the local store")?);
    if server {
        let runtime = BackgroundRuntime::start(2)?;
        let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
        let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
        let report = runtime
            .handle()
            .block_on(sync_service.search_server_and_wait(account_id, query))?;
        if report.searched {
            println!(
                "Server search: {} hits, {} fetched by a sync, {} unresolved.",
                report.hits, report.fetched_by_sync, report.unresolved
            );
        } else {
            println!("The local index covers this query; the server was not asked.");
        }
    } else if store.search_needs_server(account_id, query)? {
        println!("Some bodies are not indexed yet; add --server to also search the server.");
    }
    let hits = store.search_threads(account_id, query, 50)?;
    if hits.is_empty() {
        println!("No matches for {query:?} on account {account_id}.");
        return Ok(());
    }

    for hit in hits {
        let origin = if hit.from_server { " (server)" } else { "" };
        println!(
            "#{} [{}] {}{origin}",
            hit.thread_id, hit.mailbox_name, hit.subject
        );
        println!("  {}", hit.participants);
        println!("  {}", hit.preview);
        println!(
            "  {} unread • flagged={} • {}",
            hit.unread_count, hit.is_flagged, hit.latest_message_at
        );
    }

    Ok(())
}

fn read_thread(thread_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let thread = store
        .thread_view(thread_id)?
        .with_context(|| format!("thread {thread_id} does not exist"))?;

    println!(
        "#{} [{}] {}",
        thread.id, thread.mailbox_name, thread.subject
    );
    println!(
        "{} msgs • {} unread • flagged={} • remote_content={}",
        thread.message_count, thread.unread_count, thread.is_flagged, thread.has_remote_content
    );
    println!("{}", thread.participants);

    for message in thread.messages {
        println!();
        println!(
            "{} -> {} [{}]",
            message.sender, message.recipients, message.render_mode
        );
        println!(
            "{} • read={} • flagged={} • remote_content={}",
            message.received_at, message.is_read, message.is_flagged, message.has_remote_content
        );
        for attachment in &message.attachments {
            println!(
                "  attachment #{} {} [{}] {} bytes",
                attachment.id, attachment.filename, attachment.content_type, attachment.size_bytes
            );
        }
        println!("{}", message.rendered_body);
    }

    Ok(())
}

fn list_attachments(thread_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let thread = store
        .thread_view(thread_id)?
        .with_context(|| format!("thread {thread_id} does not exist"))?;

    println!(
        "#{} [{}] {}",
        thread.id, thread.mailbox_name, thread.subject
    );
    let mut count = 0;
    for message in &thread.messages {
        for attachment in &message.attachments {
            println!(
                "#{} {} [{}] {} bytes (message {})",
                attachment.id,
                attachment.filename,
                attachment.content_type,
                attachment.size_bytes,
                message.id
            );
            count += 1;
        }
    }
    if count == 0 {
        println!("Thread {thread_id} has no attachments.");
    }
    Ok(())
}

fn save_attachment(attachment_id: i64, destination: Option<&str>) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let destination = destination.unwrap_or(".");
    let path = store.save_attachment(attachment_id, destination)?;
    println!("Saved attachment {attachment_id} to {}.", path.display());
    Ok(())
}

fn flag_thread(thread_id: i64, flagged: bool) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store.set_thread_flagged(thread_id, flagged)?;
    println!(
        "Thread {thread_id} {}.",
        if flagged { "flagged" } else { "unflagged" }
    );
    Ok(())
}

fn mark_thread_read(thread_id: i64, read: bool) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store.set_thread_read(thread_id, read)?;
    println!(
        "Thread {thread_id} marked {}.",
        if read { "read" } else { "unread" }
    );
    Ok(())
}

fn move_thread(thread_id: i64, mailbox_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store.move_thread(thread_id, mailbox_id)?;
    println!("Thread {thread_id} moved to mailbox {mailbox_id}.");
    Ok(())
}

fn archive_thread(thread_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store.archive_thread(thread_id)?;
    println!("Thread {thread_id} archived.");
    Ok(())
}

fn delete_thread(thread_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store.delete_thread(thread_id)?;
    println!("Thread {thread_id} deleted.");
    Ok(())
}

fn compose_draft(account_id: i64, identity_id: Option<i64>) -> Result<()> {
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let identities = store
        .list_identities(account_id)
        .with_context(|| format!("failed to load identities for account {account_id}"))?;
    if identities.is_empty() {
        bail!("account {account_id} has no configured identities");
    }

    let identity_id = identity_id.unwrap_or_else(|| {
        identities
            .iter()
            .find(|identity| identity.is_primary)
            .or_else(|| identities.first())
            .map(|identity| identity.id)
            .expect("identities is not empty")
    });
    let draft_id = store.create_draft(&NewDraft {
        account_id,
        identity_id,
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: String::new(),
        body: String::new(),
    })?;
    let mut draft = store
        .draft_detail(draft_id)?
        .with_context(|| format!("draft {draft_id} does not exist after creation"))?;

    println!("Composing draft #{draft_id} for account {account_id}.");
    println!("Enter a blank line to keep the current value.");

    let selected_identity_id =
        prompt_identity_selection(account_id, &identities, draft.identity_id)?;
    if selected_identity_id != draft.identity_id {
        store.update_draft(
            draft_id,
            &DraftUpdate {
                identity_id: selected_identity_id,
                to: draft.to.clone(),
                cc: draft.cc.clone(),
                bcc: draft.bcc.clone(),
                subject: draft.subject.clone(),
                body: draft.body.clone(),
            },
        )?;
        draft = store
            .draft_detail(draft_id)?
            .with_context(|| format!("draft {draft_id} disappeared during compose"))?;
    }

    draft.to = prompt_recipients("To", &draft.to)?;
    save_draft(&store, draft_id, &draft)?;
    draft.cc = prompt_recipients("Cc", &draft.cc)?;
    save_draft(&store, draft_id, &draft)?;
    draft.bcc = prompt_recipients("Bcc", &draft.bcc)?;
    save_draft(&store, draft_id, &draft)?;
    draft.subject = prompt_with_default("Subject", &draft.subject)?;
    save_draft(&store, draft_id, &draft)?;
    draft.body = prompt_multiline_with_default("Body", &draft.body)?;
    save_draft(&store, draft_id, &draft)?;

    if let Some(attachments) =
        prompt_optional("Attachment paths (comma-separated, optional; leave blank for none)")?
    {
        for path in attachments
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            store.add_draft_attachment(draft_id, path)?;
        }
        draft = store
            .draft_detail(draft_id)?
            .with_context(|| format!("draft {draft_id} disappeared after adding attachments"))?;
    }

    println!(
        "Draft #{draft_id} saved with {} attachment(s).",
        draft.attachments.len()
    );

    if prompt_yes_no("Queue this draft for send now", false)? {
        let runtime = BackgroundRuntime::start(4)?;
        let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
        let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
        let account_id = draft.account_id;
        let job_id = sync_service.queue_draft_and_send(draft_id)?;
        let _ = sync_service.process_outbox_and_wait(Some(account_id))?;
        println!("Draft {draft_id} queued as outbox job {job_id}.");
    }

    Ok(())
}

fn list_drafts(account_id: Option<i64>) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let drafts = store.list_drafts(account_id)?;
    if drafts.is_empty() {
        println!("No drafts found.");
        return Ok(());
    }

    for draft in drafts {
        println!(
            "#{} account={} identity={} [{}] {}",
            draft.id, draft.account_id, draft.identity_name, draft.state, draft.subject
        );
        println!(
            "  to: {} • attachments: {} • updated: {}",
            draft.recipient_summary, draft.attachment_count, draft.updated_at
        );
    }
    Ok(())
}

fn show_draft(draft_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let draft = store
        .draft_detail(draft_id)?
        .with_context(|| format!("draft {draft_id} does not exist"))?;

    println!(
        "#{} account={} identity={} <{}> [{}]",
        draft.id, draft.account_id, draft.identity_name, draft.identity_email, draft.state
    );
    println!("To: {}", join_or_placeholder(&draft.to));
    println!("Cc: {}", join_or_placeholder(&draft.cc));
    println!("Bcc: {}", join_or_placeholder(&draft.bcc));
    println!(
        "Subject: {}",
        if draft.subject.is_empty() {
            "(no subject)"
        } else {
            &draft.subject
        }
    );
    println!("Updated: {}", draft.updated_at);
    if let Some(queued_at) = draft.queued_at.as_deref() {
        println!("Queued: {queued_at}");
    }
    if let Some(sent_at) = draft.sent_at.as_deref() {
        println!("Sent: {sent_at}");
    }
    if let Some(error) = draft.last_error_message.as_deref() {
        println!("Last error: {error}");
    }
    println!();
    println!("{}", draft.body);
    if !draft.attachments.is_empty() {
        println!();
        println!("Attachments:");
        for attachment in draft.attachments {
            println!(
                "  #{} {} ({}, {} bytes)",
                attachment.id, attachment.filename, attachment.content_type, attachment.size_bytes
            );
        }
    }
    Ok(())
}

fn send_draft(draft_id: i64) -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    let account_id = store
        .draft_detail(draft_id)?
        .with_context(|| format!("draft {draft_id} does not exist"))?
        .account_id;
    let job_id = sync_service.queue_draft_and_send(draft_id)?;
    let _ = sync_service.process_outbox_and_wait(Some(account_id))?;
    println!("Draft {draft_id} queued as outbox job {job_id}.");
    Ok(())
}

fn list_outbox(account_id: Option<i64>) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let jobs = store.list_outbox_jobs(account_id)?;
    if jobs.is_empty() {
        println!("No outbox jobs found.");
        return Ok(());
    }

    for job in jobs {
        println!(
            "#{} draft={} account={} [{}] attempts={}",
            job.id, job.draft_id, job.account_id, job.state, job.attempt_count
        );
        if let Some(next_attempt_at) = job.next_attempt_at.as_deref() {
            println!("  next attempt: {next_attempt_at}");
        }
        if let Some(error) = job.last_error_message.as_deref() {
            println!("  last error: {error}");
        }
    }
    Ok(())
}

fn retry_outbox_job(job_id: i64) -> Result<()> {
    let store = Arc::new(open_store().context("failed to open the local store")?);
    store.retry_outbox_job(job_id)?;
    let runtime = BackgroundRuntime::start(4)?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    let processed = sync_service.process_outbox_and_wait(None)?;
    println!("Outbox job {job_id} requeued. Processed {processed} due job(s).");
    Ok(())
}

fn add_attachment(draft_id: i64, path: &std::path::Path) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let attachment = store.add_draft_attachment(draft_id, path)?;
    println!(
        "Added attachment #{} {} to draft {}.",
        attachment.id, attachment.filename, draft_id
    );
    Ok(())
}

fn remove_attachment(attachment_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store.remove_draft_attachment(attachment_id)?;
    println!("Removed attachment {attachment_id}.");
    Ok(())
}

fn process_outbox(account_id: Option<i64>) -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    let processed = sync_service.process_outbox_and_wait(account_id)?;
    println!("Processed {processed} due outbox job(s).");
    Ok(())
}

fn list_dead_ops(account_id: Option<i64>) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let accounts = match account_id {
        Some(account_id) => vec![account_id],
        None => store
            .list_sync_targets()?
            .into_iter()
            .map(|target| target.id)
            .collect(),
    };

    let mut printed = 0;
    for account_id in accounts {
        for op in store.list_dead_ops(account_id)? {
            println!(
                "op {} • account {} • {:?} {} in {} -> {} • attempts: {} • last error: {}",
                op.id,
                op.account_id,
                op.kind,
                op.remote_id,
                op.source_mailbox,
                op.target_mailbox.as_deref().unwrap_or("-"),
                op.attempts,
                op.last_error.as_deref().unwrap_or("none"),
            );
            printed += 1;
        }
    }
    println!("{printed} dead-lettered operation(s).");
    Ok(())
}

fn retry_dead_op(op_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store.retry_dead_op(op_id)?;
    println!("Operation {op_id} requeued; the next sync retries it.");
    Ok(())
}

fn discard_dead_op(op_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store.discard_dead_op(op_id)?;
    println!("Operation {op_id} discarded.");
    Ok(())
}

/// Sets (or clears) an account's disk budget and applies it immediately.
fn set_cache_budget(account_id: i64, budget_mb: Option<u64>) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    store
        .sync_target(account_id)?
        .with_context(|| format!("account {account_id} no longer exists"))?;
    store.set_account_disk_budget(account_id, budget_mb)?;
    match budget_mb {
        Some(budget_mb) => {
            // Apply right away so the new budget takes effect without
            // waiting for the next sync.
            let freed = store.evict_account_blobs(account_id, budget_mb * 1024 * 1024)?;
            println!(
                "Disk budget for account {account_id} set to {budget_mb} MiB; freed {freed} byte(s)."
            );
        }
        None => {
            println!("Disk budget for account {account_id} removed; blobs are kept without limit.")
        }
    }
    Ok(())
}

fn empty_trash(account_id: i64) -> Result<()> {
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let trash_name = store
        .mailbox_name_by_role(account_id, "trash")?
        .with_context(|| format!("account {account_id} has no trash mailbox"))?;

    println!(
        "This expunges every message in the {trash_name} mailbox of account {account_id} on the server."
    );

    let target = store
        .sync_target(account_id)?
        .with_context(|| format!("account {account_id} no longer exists"))?;

    let runtime = BackgroundRuntime::start(4)?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
    let secret = runtime
        .handle()
        .block_on(sync_service.connect_secret(&target))
        .context("failed to resolve the account credentials")?;
    let expunged =
        runtime
            .handle()
            .block_on(zmail_imap::empty_trash(&target, &secret, &trash_name))?;

    println!("Expunged {expunged} message(s) from {trash_name}; syncing the local mirror.");
    let _ = sync_service.sync_account_and_wait(account_id);
    Ok(())
}

fn create_backup(directory: Option<&str>) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let directory = match directory {
        Some(directory) => std::path::PathBuf::from(directory),
        None => store
            .path()
            .parent()
            .map(|parent| parent.join("backups"))
            .context("the store path has no parent directory")?,
    };
    let path = store.create_backup(&directory, 10)?;
    println!(
        "Backup written to {} (keeping the 10 most recent).",
        path.display()
    );
    Ok(())
}

fn export_account(account_id: i64, path: &str) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let mut writer = std::fs::File::create(path)
        .with_context(|| format!("failed to create the export file {path}"))?;
    let exported = store.export_account_mbox(account_id, &mut writer)?;
    println!("Exported {exported} message(s) to {path}.");
    Ok(())
}

/// Credentials collected while setting up a new account.
enum AccountSecret {
    /// Password or app password.
    Password(String),
    /// An OAuth2 access token pasted by the user (no refresh grant).
    PastedAccessToken(String),
    /// Tokens granted by the browser flow, with the client that ran it.
    OAuth2Flow {
        client: zmail_auth::OAuth2Client,
        tokens: zmail_auth::TokenSet,
    },
}

impl AccountSecret {
    /// Persists the credentials for a freshly inserted account.
    fn store(&self, store: &LocalStore, secrets: &dyn SecretStore, account_id: i64) -> Result<()> {
        match self {
            Self::Password(secret) => secrets
                .set_account_secret(account_id, secret)
                .with_context(|| format!("failed to store credentials for account {account_id}")),
            Self::PastedAccessToken(token) => secrets
                .set_account_secret_in_slot(account_id, SecretSlot::AccessToken, token)
                .with_context(|| {
                    format!("failed to store the access token for account {account_id}")
                }),
            Self::OAuth2Flow { client, tokens } => {
                // The full grant is persisted so background sync can refresh
                // the access token without user interaction.
                zmail_sync::persist_oauth2_grant(store, secrets, account_id, client, tokens)
            }
        }
    }
}

fn prompt_account(protocol: ProtocolKind) -> Result<(NewAccount, AccountSecret)> {
    if protocol == ProtocolKind::Proton {
        // Proton accounts need Proton sign-in and an encrypted local store
        // first (docs/adr/0004-proton-native.md, #139).
        bail!("Proton Mail accounts can't be added yet");
    }
    println!("Setting up a new {protocol} account.");
    let display_name = prompt("Display name")?;
    let endpoint = match protocol {
        ProtocolKind::Imap => prompt_with_default("IMAP endpoint", "imaps://mail.example.com:993")?,
        ProtocolKind::Jmap => prompt_with_default("JMAP base URL", "https://mail.example.com")?,
        ProtocolKind::Proton => unreachable!("Proton accounts are refused above"),
    };
    validate_endpoint(protocol, &endpoint)?;
    let username = prompt("Username or account email")?;
    let auth = match protocol {
        ProtocolKind::Imap => {
            if prompt_yes_no("Authenticate with an OAuth2 access token (XOAUTH2)?", false)? {
                AuthKind::OAuth2
            } else {
                AuthKind::Password
            }
        }
        ProtocolKind::Jmap => {
            if prompt_yes_no("Authenticate with an OAuth2 bearer token?", false)? {
                AuthKind::OAuth2
            } else {
                AuthKind::Password
            }
        }
        ProtocolKind::Proton => unreachable!("Proton accounts are refused above"),
    };
    let secret = match auth {
        AuthKind::Password => AccountSecret::Password(
            rpassword::prompt_password("Password or app password: ")
                .context("failed to read the account password")?,
        ),
        AuthKind::OAuth2 => {
            if prompt_yes_no("Run the browser authorization flow (PKCE)?", true)? {
                let client = prompt_oauth2_client(protocol)?;
                let tokens = client.authorize_interactively()?;
                println!(
                    "Received an access token{}.",
                    if tokens.refresh_token.is_some() {
                        " and a refresh token"
                    } else {
                        ""
                    }
                );
                AccountSecret::OAuth2Flow { client, tokens }
            } else {
                AccountSecret::PastedAccessToken(
                    rpassword::prompt_password("OAuth2 access token: ")
                        .context("failed to read the OAuth2 access token")?,
                )
            }
        }
        AuthKind::ProtonSession => unreachable!("Proton accounts are refused above"),
    };
    let identity_name = prompt_with_default("Primary identity name", &display_name)?;
    let identity_email = prompt_with_default("Primary identity email", &username)?;
    let signature =
        prompt_with_default("Primary signature", &format!("Regards,\n{identity_name}"))?;
    let alias_input = prompt_optional("Additional identity emails (comma-separated, optional)")?;
    let outgoing = match protocol {
        ProtocolKind::Imap => prompt_smtp_outgoing(&endpoint, &username)?,
        ProtocolKind::Jmap => OutgoingConfig::jmap_default(),
        ProtocolKind::Proton => unreachable!("Proton accounts are refused above"),
    };

    let mut identities = vec![NewIdentity {
        display_name: identity_name.clone(),
        email_address: identity_email.clone(),
        signature,
        is_primary: true,
    }];

    if let Some(alias_input) = alias_input {
        for (index, alias) in alias_input
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .enumerate()
        {
            identities.push(NewIdentity {
                display_name: format!("{identity_name} Alias {}", index + 1),
                email_address: alias.to_owned(),
                signature: String::new(),
                is_primary: false,
            });
        }
    }

    Ok((
        NewAccount {
            display_name,
            protocol,
            auth,
            endpoint,
            username,
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(outgoing),
            identities,
        },
        secret,
    ))
}

/// Prompts for the OAuth2 client configuration used by the browser flow,
/// with Gmail and Microsoft 365 presets pre-filling the endpoints.
fn prompt_oauth2_client(protocol: ProtocolKind) -> Result<zmail_auth::OAuth2Client> {
    // No consumer provider presets offer JMAP; JMAP accounts always use
    // custom provider endpoints.
    if protocol == ProtocolKind::Jmap {
        return prompt_custom_oauth2_client();
    }

    let preset = prompt_with_default(
        "Provider (1 = Gmail, 2 = Microsoft 365, 3 = custom endpoints)",
        "3",
    )?;
    let redirect_port: u16 = prompt_with_default("Local redirect port", "8357")?
        .parse()
        .context("the redirect port must be a number")?;

    match preset.trim() {
        "1" => {
            let client_id = prompt("Google OAuth2 client id")?;
            // Google's desktop clients need their secret at the token
            // exchange, so it is required here.
            let client_secret = prompt("Google OAuth2 client secret")?;
            Ok(zmail_auth::OAuth2Preset::Gmail.client(
                client_id,
                Some(client_secret),
                redirect_port,
            ))
        }
        "2" => {
            let client_id = prompt("Microsoft application (client) id")?;
            let client_secret = prompt_optional("Client secret (public clients leave this empty)")?;
            Ok(zmail_auth::OAuth2Preset::Microsoft365.client(
                client_id,
                client_secret,
                redirect_port,
            ))
        }
        _ => {
            let client_id = prompt("OAuth2 client id")?;
            let client_secret = prompt_optional("Client secret (public clients leave this empty)")?;
            let client = prompt_custom_oauth2_client()?;
            Ok(zmail_auth::OAuth2Client {
                client_id,
                client_secret,
                ..client
            })
        }
    }
}

/// Prompts for a provider-agnostic OAuth2 client configuration, used by JMAP
/// accounts (no consumer presets offer JMAP) and the custom-endpoint choice.
fn prompt_custom_oauth2_client() -> Result<zmail_auth::OAuth2Client> {
    let redirect_port: u16 = prompt_with_default("Local redirect port", "8357")?
        .parse()
        .context("the redirect port must be a number")?;
    let client_id = prompt("OAuth2 client id")?;
    let client_secret = prompt_optional("Client secret (public clients leave this empty)")?;
    let authorization_endpoint = prompt("Authorization endpoint URL")?;
    let token_endpoint = prompt("Token endpoint URL")?;
    let scopes = prompt_with_default("Scopes (space-separated)", "email")?
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(zmail_auth::OAuth2Client {
        client_id,
        client_secret,
        authorization_endpoint,
        token_endpoint,
        scopes,
        redirect_port,
        extra_authorize_params: Vec::new(),
    })
}

fn fixture_imap_account(endpoint: &str, username: &str, email: &str) -> NewAccount {
    NewAccount {
        display_name: "IMAP Fixture".to_owned(),
        protocol: ProtocolKind::Imap,
        auth: AuthKind::Password,
        endpoint: endpoint.to_owned(),
        username: username.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: Some(OutgoingConfig::smtp_default(derive_host(endpoint))),
        identities: vec![NewIdentity {
            display_name: "Fixture Recipient".to_owned(),
            email_address: email.to_owned(),
            signature: "Regards,\nFixture Recipient".to_owned(),
            is_primary: true,
        }],
    }
}

fn validate_endpoint(protocol: ProtocolKind, endpoint: &str) -> Result<()> {
    let parsed = url::Url::parse(endpoint)
        .with_context(|| format!("failed to parse endpoint {endpoint}"))?;
    match protocol {
        ProtocolKind::Imap => {
            if !matches!(parsed.scheme(), "imap" | "imaps") {
                bail!("IMAP endpoints must use imap:// or imaps://");
            }
        }
        ProtocolKind::Jmap => {
            if !matches!(parsed.scheme(), "http" | "https") {
                bail!("JMAP endpoints must use http:// or https://");
            }
        }
        ProtocolKind::Proton => {
            if parsed.scheme() != "https" {
                bail!("the Proton API is only reached over https://");
            }
        }
    }
    Ok(())
}

fn parse_optional_id(value: Option<String>, label: &str) -> Result<Option<i64>> {
    value
        .map(|value| {
            value
                .parse::<i64>()
                .with_context(|| format!("{label} must be an integer"))
        })
        .transpose()
}

fn prompt_smtp_outgoing(endpoint: &str, username: &str) -> Result<OutgoingConfig> {
    let mut outgoing = OutgoingConfig::smtp_default(derive_host(endpoint));
    outgoing.host = Some(prompt_with_default(
        "SMTP host",
        outgoing.host.as_deref().unwrap_or("localhost"),
    )?);
    outgoing.port = Some(
        prompt_with_default("SMTP port", &outgoing.port.unwrap_or(587).to_string())?
            .parse::<u16>()
            .context("SMTP port must be an integer between 0 and 65535")?,
    );
    let security = prompt_with_default("SMTP security [plain|starttls|implicit_tls]", "starttls")?;
    outgoing.security = Some(parse_submission_security(&security)?);
    outgoing.username = Some(prompt_with_default("SMTP username", username)?);
    outgoing.hello_name = prompt_with_default("SMTP EHLO hostname", "localhost")?;
    Ok(outgoing)
}

fn parse_submission_security(value: &str) -> Result<SubmissionSecurity> {
    SubmissionSecurity::parse(&value.trim().to_ascii_lowercase())
        .with_context(|| format!("unknown submission security mode {value}"))
}

fn derive_host(endpoint: &str) -> String {
    url::Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "localhost".to_owned())
}

fn build_bootstrap(runtime: zmail_core::RuntimeSummary) -> Result<AppBootstrap> {
    let theme = load_theme_summary(PHASE0_DARK_THEME, "dark")?;
    let alternate_theme = load_theme_summary(PHASE0_LIGHT_THEME, "light")?;

    Ok(AppBootstrap {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        runtime,
        theme,
        alternate_theme,
        upstream: UpstreamPins {
            zed_commit: ZED_UPSTREAM_COMMIT.to_owned(),
            gpui_version: GPUI_VERSION.to_owned(),
        },
    })
}

fn load_theme_summary(theme_json: &str, variant_label: &str) -> Result<ThemeSummary> {
    let theme_family = ThemeFamily::from_json_str(theme_json)
        .with_context(|| format!("failed to parse the bundled {variant_label} theme family"))?;
    let theme = theme_family
        .default_theme()
        .with_context(|| format!("the bundled {variant_label} theme family is empty"))?;
    Ok(theme_family.summary(theme))
}

fn prompt(label: &str) -> Result<String> {
    prompt_with_default(label, "")
}

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    let mut stdout = io::stdout();
    if default.is_empty() {
        write!(stdout, "{label}: ")?;
    } else {
        write!(stdout, "{label} [{default}]: ")?;
    }
    stdout.flush()?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read terminal input")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        if default.is_empty() {
            bail!("{label} must not be empty");
        }
        Ok(default.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

fn prompt_optional(label: &str) -> Result<Option<String>> {
    let mut stdout = io::stdout();
    write!(stdout, "{label}: ")?;
    stdout.flush()?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read terminal input")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_owned()))
    }
}

fn prompt_recipients(label: &str, current: &[String]) -> Result<Vec<String>> {
    let default = current.join(", ");
    let value = prompt_with_default(label, &default)?;
    Ok(value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect())
}

fn prompt_multiline_with_default(label: &str, default: &str) -> Result<String> {
    println!("{label} (finish with a single '.' on its own line):");
    if !default.is_empty() {
        println!("Current value:");
        println!("{default}");
    }

    let mut lines = Vec::new();
    loop {
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read terminal input")?;
        let trimmed = input.trim_end_matches(['\r', '\n']);
        if trimmed == "." {
            break;
        }
        if trimmed.is_empty() && lines.is_empty() {
            return Ok(default.to_owned());
        }
        lines.push(trimmed.to_owned());
    }

    Ok(lines.join("\n"))
}

fn prompt_yes_no(label: &str, default: bool) -> Result<bool> {
    let default_label = if default { "Y/n" } else { "y/N" };
    let response = prompt_with_default(label, default_label)?;
    match response.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        value if value == default_label.to_ascii_lowercase() => Ok(default),
        _ if response == default_label => Ok(default),
        other => bail!("expected yes or no, got {other}"),
    }
}

fn prompt_identity_selection(
    account_id: i64,
    identities: &[zmail_core::IdentitySummary],
    current_identity_id: i64,
) -> Result<i64> {
    println!("Available identities for account {account_id}:");
    for identity in identities {
        println!(
            "  {}: {} <{}>{}",
            identity.id,
            identity.display_name,
            identity.email_address,
            if identity.id == current_identity_id {
                " [current]"
            } else {
                ""
            }
        );
    }
    let selected = prompt_with_default("Identity id", &current_identity_id.to_string())?;
    selected
        .parse::<i64>()
        .context("identity id must be an integer")
}

fn save_draft(store: &LocalStore, draft_id: i64, draft: &zmail_core::DraftDetail) -> Result<()> {
    store.update_draft(
        draft_id,
        &DraftUpdate {
            identity_id: draft.identity_id,
            to: draft.to.clone(),
            cc: draft.cc.clone(),
            bcc: draft.bcc.clone(),
            subject: draft.subject.clone(),
            body: draft.body.clone(),
        },
    )
}

fn join_or_placeholder(values: &[String]) -> String {
    if values.is_empty() {
        "(none)".to_owned()
    } else {
        values.join(", ")
    }
}

fn print_help() {
    println!("Zmail commands:");
    println!("  zmail-app                 Launch the desktop app");
    println!("  zmail-app launch-imap-fixture ENDPOINT USERNAME SECRET EMAIL");
    println!("  zmail-app launch-ui-fixture VIEW [dark|light|system] [TEXT_SIZE]");
    println!("                            Open a seeded UI state for screenshots: empty,");
    println!("                            inbox, reader, compose, account-setup or onboarding-*,");
    println!("                            optionally at a text size in percent (100 to 200)");
    println!("  zmail-app add-imap        Add and sync a real IMAP account");
    println!("  zmail-app add-jmap        Add and sync a real JMAP account");
    println!("  zmail-app list-accounts   Show configured accounts");
    println!("  zmail-app sync [all|ID]   Sync all accounts or one account by id");
    println!("  zmail-app search [--server] ID QUERY");
    println!("                            Search local mail for an account; --server also");
    println!("                            asks the server about mail not indexed yet");
    println!("  zmail-app index ID        Fetch and index message bodies for an account");
    println!("  zmail-app read THREAD     Print the hydrated thread reader view");
    println!("  zmail-app flag THREAD     Flag a thread locally");
    println!("  zmail-app unflag THREAD   Remove a local flag from a thread");
    println!("  zmail-app mark-read THREAD");
    println!("  zmail-app mark-unread THREAD");
    println!("  zmail-app move THREAD MAILBOX");
    println!("  zmail-app archive THREAD");
    println!("  zmail-app delete THREAD");
    println!("  zmail-app compose ACCOUNT [IDENTITY]");
    println!("  zmail-app drafts [ACCOUNT]");
    println!("  zmail-app draft DRAFT");
    println!("  zmail-app send DRAFT");
    println!("  zmail-app attach DRAFT PATH");
    println!("  zmail-app detach ATTACHMENT");
    println!("  zmail-app outbox [ACCOUNT]");
    println!("  zmail-app retry-outbox JOB");
    println!("  zmail-app process-outbox [ACCOUNT]");
    println!("  zmail-app backup [DIRECTORY]");
    println!("  zmail-app reset-store     Move a store whose key is lost aside and start over");
    println!("  zmail-app rotate-key      Re-encrypt the store under a new key (quit Zmail first)");
    println!("  zmail-app export-account ACCOUNT PATH");
    println!("  zmail-app attachments THREAD");
    println!("  zmail-app save-attachment ATTACHMENT [--dest DIRECTORY]");
    println!("  zmail-app dead-ops [ACCOUNT]");
    println!("  zmail-app retry-op OPERATION");
    println!("  zmail-app discard-op OPERATION");
    println!("  zmail-app empty-trash ACCOUNT");
}

/// Opens the encrypted store with its key from the OS keychain (ADR 0005),
/// encrypting a plaintext store first and printing its progress.
fn open_store() -> Result<LocalStore> {
    let path = store_path()?;
    match LocalStore::unlock(&path, &KeyringSecretStore, &print_progress) {
        Ok((store, report)) => {
            if let Some(report) = report {
                eprintln!(
                    "The mail store is now encrypted. Blocks of the old plaintext files can \
                     stay on the disk until it reuses them; full-disk encryption covers that."
                );
                for backup in report.plaintext_backups {
                    eprintln!(
                        "note: the backup {} was made before encryption and is still plaintext.",
                        backup.display()
                    );
                }
            }
            Ok(store)
        }
        Err(error @ StoreOpenError::Unreadable { .. }) => Err(anyhow::Error::from(error).context(
            "the mail on your servers is safe, but drafts, unsent outbox mail and queued \
             changes that exist only in this store are lost. `zmail-app reset-store` moves \
             the unreadable store aside and starts a new one",
        )),
        Err(StoreOpenError::Keychain(error)) => Err(error.context(
            "Zmail needs the system keychain to unlock its mail store; unlock it and try again",
        )),
        Err(StoreOpenError::Other(error)) => Err(error.context("failed to open the local store")),
    }
}

/// Prints a migration's or a key rotation's progress on one line of
/// stderr.
fn print_progress(step: MigrationProgress) {
    match step {
        MigrationProgress::Database { written, total } => {
            let percent = (written * 100).checked_div(total).unwrap_or(100);
            eprint!("\rEncrypting the mail store: {percent}%");
        }
        MigrationProgress::Verifying => eprint!("\rEncrypting the mail store: checking the copy"),
        MigrationProgress::Attachments { done, total } => {
            eprint!("\rEncrypting attachments: {done} of {total}          ");
        }
        MigrationProgress::Finished => eprintln!("\rEncrypting the mail store: done.            "),
    }
}

/// `rotate-key`: re-encrypts the store under a new key and deletes the old
/// one (ADR 0005 §3). Refuses while the store is open anywhere.
fn rotate_store_key() -> Result<()> {
    let path = store_path()?;
    let new = LocalStore::rotate_key(&path, &KeyringSecretStore, &print_progress)
        .map_err(|error| anyhow::Error::from(error).context("the key was not changed"))?;
    println!(
        "The mail store is now encrypted with a new key ({new}); the old key is deleted. \
         Backups made with the old key no longer open."
    );
    Ok(())
}

/// `reset-store`: moves a store whose key is gone aside, so the next start
/// creates a new one. Refuses while the store can still be unlocked.
fn reset_unreadable_store() -> Result<()> {
    let path = store_path()?;
    match LocalStore::unlock(&path, &KeyringSecretStore, &|_| {}) {
        Ok(_) => bail!("the mail store unlocks with its key; there is nothing to reset"),
        Err(StoreOpenError::Unreadable { .. }) => {}
        Err(error) => return Err(anyhow::Error::from(error).context("not resetting the store")),
    }
    let aside = zmail_store::move_unreadable_store_aside(&path)?;
    println!(
        "Moved the unreadable store to {}. The next start creates a new store; add your \
         accounts again. Delete that directory once you are sure its key won't turn up.",
        aside.display()
    );
    Ok(())
}

/// Name of the SQLite database file inside the store directory.
const STORE_FILE_NAME: &str = "zmail.sqlite3";

/// Legacy store directory used before the platform data directory existed.
const LEGACY_STORE_DIR: &str = ".zmail-dev";

/// Environment variable that overrides the store directory for development.
const DATA_DIR_ENV: &str = "ZMAIL_DATA_DIR";

/// The resolved store path plus whether a legacy store was migrated into it.
/// Resolves the store path, migrating a legacy `.zmail-dev` store on first use.
///
/// The store lives in a platform data directory so that user data survives
/// regardless of the directory the app was launched from. A legacy
/// `.zmail-dev` store in the current directory is moved there once; if the
/// move fails the legacy store keeps working so no data is ever lost.
fn store_path() -> Result<PathBuf> {
    let dir = data_dir()?;
    let store = dir.join(STORE_FILE_NAME);

    let legacy = PathBuf::from(LEGACY_STORE_DIR);
    if !store.exists() && legacy.join(STORE_FILE_NAME).exists() {
        match migrate_legacy_store(&legacy, &dir) {
            Ok(()) => {
                eprintln!(
                    "migrated the legacy {} store to {}",
                    legacy.display(),
                    dir.display()
                );
                return Ok(store);
            }
            Err(error) => {
                eprintln!(
                    "warning: failed to migrate the legacy {} store to {}: {error:#}",
                    legacy.display(),
                    dir.display()
                );
                return Ok(legacy.join(STORE_FILE_NAME));
            }
        }
    }

    Ok(store)
}

/// Returns the directory that holds the local store and attachments.
fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV)
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }

    resolve_platform_data_dir(
        std::env::var_os("HOME"),
        std::env::var_os("XDG_DATA_HOME"),
        std::env::var_os("APPDATA"),
    )
}

/// Environment variable that overrides the crash-log directory for tests.
const LOG_DIR_ENV: &str = "ZMAIL_LOG_DIR";

/// Name of the crash log file inside the crash-log directory.
const CRASH_LOG_FILE_NAME: &str = "crash.log";

/// Installs a panic hook that appends crash reports to a log file.
///
/// GUI launches lose stderr: when the app is started from Finder or the
/// Dock there is no console attached, so a panic leaves no trace at all
/// and the crash only surfaces as a macOS abort report. The hook keeps
/// the default stderr behavior and additionally appends a line per panic
/// to the platform's crash-log directory, per the crash policy in
/// `docs/phase5-hardening.md`. Nothing is uploaded.
pub(crate) fn install_panic_log() {
    let log_dir = match panic_log_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("warning: no crash log will be kept: {error:#}");
            return;
        }
    };
    if let Err(error) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "warning: no crash log will be kept: failed to create {}: {error}",
            log_dir.display()
        );
        return;
    }
    let log_path = log_dir.join(CRASH_LOG_FILE_NAME);
    rotate_crash_log(&log_path);
    install_panic_hook(&log_path);

    eprintln!("Crash reports append to {}", log_path.display());
}

/// Installs the crash-log hook for an already resolved log file.
fn install_panic_hook(log_path: &std::path::Path) {
    let log_path = log_path.to_owned();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);

        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("panic with a non-string payload");
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown location".to_owned());
        let thread = std::thread::current()
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| "unnamed".to_owned());
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .and_then(|mut file| {
                std::io::Write::write_all(
                    &mut file,
                    format!("{timestamp} thread '{thread}' panicked at {location}: {message}\n")
                        .as_bytes(),
                )
            });
    }));
}

/// Returns the directory that receives crash log files.
fn panic_log_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(LOG_DIR_ENV)
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }

    resolve_platform_log_dir(
        std::env::var_os("HOME"),
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("LOCALAPPDATA"),
    )
}

/// Renames an oversized crash log aside so a single file never grows
/// unboundedly; the previous content stays available as `crash.log.old`.
fn rotate_crash_log(log_path: &std::path::Path) {
    const CRASH_LOG_MAX_BYTES: u64 = 1024 * 1024;
    let Ok(metadata) = std::fs::metadata(log_path) else {
        return;
    };
    if metadata.len() > CRASH_LOG_MAX_BYTES {
        let rotated = log_path.with_extension("log.old");
        let _ = std::fs::rename(log_path, rotated);
    }
}

/// The unused parameters keep a single signature across platforms so the
/// resolution rules stay testable without mutating process environment.
#[cfg(target_os = "macos")]
fn resolve_platform_log_dir(
    home: Option<std::ffi::OsString>,
    _xdg_state_home: Option<std::ffi::OsString>,
    _localappdata: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    let home = home.context("the HOME environment variable is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Logs")
        .join("zmail"))
}

/// The unused parameters keep a single signature across platforms so the
/// resolution rules stay testable without mutating process environment.
#[cfg(target_os = "windows")]
fn resolve_platform_log_dir(
    _home: Option<std::ffi::OsString>,
    _xdg_state_home: Option<std::ffi::OsString>,
    localappdata: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    let localappdata = localappdata.context("the LOCALAPPDATA environment variable is not set")?;
    Ok(PathBuf::from(localappdata).join("zmail").join("logs"))
}

/// The unused parameters keep a single signature across platforms so the
/// resolution rules stay testable without mutating process environment.
#[cfg(all(unix, not(target_os = "macos")))]
fn resolve_platform_log_dir(
    home: Option<std::ffi::OsString>,
    xdg_state_home: Option<std::ffi::OsString>,
    _localappdata: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    if let Some(xdg) = xdg_state_home
        && !xdg.is_empty()
        && PathBuf::from(&xdg).is_absolute()
    {
        return Ok(PathBuf::from(xdg).join("zmail"));
    }

    let home = home.context("neither XDG_STATE_HOME nor HOME is set")?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("zmail"))
}

/// Moves a legacy store directory to the platform data directory.
fn migrate_legacy_store(legacy: &PathBuf, target: &std::path::Path) -> Result<()> {
    if target.exists() {
        bail!(
            "cannot migrate the legacy {} store because {} already exists",
            legacy.display(),
            target.display()
        );
    }
    let parent = target
        .parent()
        .context("the target store directory has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;

    match std::fs::rename(legacy, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Cross-device moves are not possible; copy instead and leave
            // the legacy directory in place as a safety net.
            std::fs::create_dir_all(target)
                .with_context(|| format!("failed to create {}", target.display()))?;
            copy_dir_contents(legacy, target)?;
            eprintln!(
                "note: the legacy {} store was copied to {} and left in place",
                legacy.display(),
                target.display()
            );
            Ok(())
        }
    }
}

/// Recursively copies a directory's contents into `target`.
fn copy_dir_contents(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    for entry in
        std::fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry.with_context(|| format!("failed to list {}", source.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&destination)
                .with_context(|| format!("failed to create {}", destination.display()))?;
            copy_dir_contents(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), &destination).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    entry.path().display(),
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Resolves the platform data directory from the provided environment values.
///
/// The unused parameters keep a single signature across platforms so the
/// resolution rules stay testable without mutating process environment.
#[cfg(target_os = "macos")]
fn resolve_platform_data_dir(
    home: Option<std::ffi::OsString>,
    _xdg_data_home: Option<std::ffi::OsString>,
    _appdata: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    let home = home.context("the HOME environment variable is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("zmail"))
}

#[cfg(target_os = "windows")]
fn resolve_platform_data_dir(
    _home: Option<std::ffi::OsString>,
    _xdg_data_home: Option<std::ffi::OsString>,
    appdata: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    let appdata = appdata.context("the APPDATA environment variable is not set")?;
    Ok(PathBuf::from(appdata).join("zmail"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn resolve_platform_data_dir(
    home: Option<std::ffi::OsString>,
    xdg_data_home: Option<std::ffi::OsString>,
    _appdata: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    if let Some(xdg) = xdg_data_home
        && !xdg.is_empty()
        && PathBuf::from(&xdg).is_absolute()
    {
        return Ok(PathBuf::from(xdg).join("zmail"));
    }

    let home = home.context("neither XDG_DATA_HOME nor HOME is set")?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("zmail"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a unique temporary directory for a test.
    fn temp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "zmail-store-path-{label}-{}-{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("failed to create the test directory");
        dir
    }

    #[test]
    fn parse_mailto_extracts_recipients_and_headers() {
        let draft = parse_mailto("mailto:person@example.com").expect("plain recipient parses");
        assert_eq!(draft.to, vec!["person@example.com".to_owned()]);
        assert_eq!(draft.subject, "");

        let draft = parse_mailto(
            "mailto:one@example.com,two@example.com?subject=Hello%20there&body=Hi%20again",
        )
        .expect("recipients with headers parse");
        assert_eq!(
            draft.to,
            vec!["one@example.com".to_owned(), "two@example.com".to_owned()]
        );
        assert_eq!(draft.subject, "Hello there");
        assert_eq!(draft.body, "Hi again");

        let draft = parse_mailto(
            "mailto:team@example.com?cc=lead@example.com&bcc=audit@example.com&subject=Sprint&body=Plus+decoded+spaces",
        )
        .expect("cc, bcc, and plus decoding parse");
        assert_eq!(draft.cc, vec!["lead@example.com".to_owned()]);
        assert_eq!(draft.bcc, vec!["audit@example.com".to_owned()]);
        assert_eq!(draft.body, "Plus decoded spaces");

        assert!(
            parse_mailto("https://example.com").is_none(),
            "non-mailto URLs are rejected"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn xdg_data_home_takes_precedence() {
        let dir = resolve_platform_data_dir(
            Some("/home/tester".into()),
            Some("/custom/data".into()),
            None,
        )
        .expect("resolution should succeed");
        assert_eq!(dir, PathBuf::from("/custom/data/zmail"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn home_fallback_ignores_relative_xdg_data_home() {
        let dir =
            resolve_platform_data_dir(Some("/home/tester".into()), Some("relative".into()), None)
                .expect("resolution should succeed");
        assert_eq!(dir, PathBuf::from("/home/tester/.local/share/zmail"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn log_dir_prefers_absolute_xdg_state_home() {
        let dir = resolve_platform_log_dir(None, Some("/custom/state".into()), None)
            .expect("resolution should succeed");
        assert_eq!(dir, PathBuf::from("/custom/state/zmail"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn log_dir_falls_back_to_home_state() {
        let dir =
            resolve_platform_log_dir(Some("/home/tester".into()), Some("relative".into()), None)
                .expect("resolution should succeed");
        assert_eq!(dir, PathBuf::from("/home/tester/.local/state/zmail"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_log_dir_uses_library_logs() {
        let dir = resolve_platform_log_dir(Some("/Users/tester".into()), None, None)
            .expect("resolution should succeed");
        assert_eq!(dir, PathBuf::from("/Users/tester/Library/Logs/zmail"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_log_dir_uses_localappdata() {
        let dir =
            resolve_platform_log_dir(None, None, Some("C:\\Users\\tester\\AppData\\Local".into()))
                .expect("resolution should succeed");
        assert_eq!(
            dir,
            PathBuf::from("C:\\Users\\tester\\AppData\\Local")
                .join("zmail")
                .join("logs")
        );
    }

    #[test]
    fn panic_hook_appends_to_the_crash_log() {
        let log_dir = temp_dir("crash-log");
        let previous_hook = std::panic::take_hook();
        install_panic_hook(&log_dir.join(CRASH_LOG_FILE_NAME));

        std::thread::Builder::new()
            .name("crash-probe".to_string())
            .spawn(|| panic!("crash-log probe"))
            .expect("failed to spawn the probe thread")
            .join()
            .expect_err("the probe thread should panic");
        std::panic::set_hook(previous_hook);

        let report = std::fs::read_to_string(log_dir.join(CRASH_LOG_FILE_NAME))
            .expect("the crash log should exist");
        assert!(report.contains("thread 'crash-probe' panicked"), "{report}");
        assert!(report.contains("crash-log probe"), "{report}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_application_support() {
        let dir = resolve_platform_data_dir(Some("/Users/tester".into()), None, None)
            .expect("resolution should succeed");
        assert_eq!(
            dir,
            PathBuf::from("/Users/tester/Library/Application Support/zmail")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_uses_appdata() {
        let dir = resolve_platform_data_dir(
            None,
            None,
            Some("C:\\Users\\tester\\AppData\\Roaming".into()),
        )
        .expect("resolution should succeed");
        assert_eq!(
            dir,
            PathBuf::from("C:\\Users\\tester\\AppData\\Roaming").join("zmail")
        );
    }

    #[test]
    fn migration_moves_the_legacy_store() {
        let root = temp_dir("migrate");
        let legacy = root.join("legacy");
        std::fs::create_dir_all(legacy.join("attachments"))
            .expect("failed to create the legacy tree");
        std::fs::write(legacy.join(STORE_FILE_NAME), "store")
            .expect("failed to write the store file");
        std::fs::write(legacy.join("attachments").join("draft.txt"), "attachment")
            .expect("failed to write the attachment");

        let target = root.join("target");
        migrate_legacy_store(&legacy, &target).expect("migration should succeed");

        assert!(target.join(STORE_FILE_NAME).exists());
        assert!(target.join("attachments").join("draft.txt").exists());
        assert!(!legacy.exists(), "the legacy store should be renamed away");
    }

    #[test]
    fn migration_refuses_to_overwrite_an_existing_store() {
        let root = temp_dir("migrate-existing");
        let legacy = root.join("legacy");
        std::fs::create_dir_all(&legacy).expect("failed to create the legacy store");
        std::fs::write(legacy.join(STORE_FILE_NAME), "store")
            .expect("failed to write the store file");
        let target = root.join("target");
        std::fs::create_dir_all(&target).expect("failed to create the target store");

        assert!(migrate_legacy_store(&legacy, &target).is_err());
    }

    #[test]
    fn copy_dir_contents_copies_nested_files() {
        let root = temp_dir("copy");
        let source = root.join("source");
        std::fs::create_dir_all(source.join("attachments"))
            .expect("failed to create the source tree");
        std::fs::write(source.join(STORE_FILE_NAME), "store")
            .expect("failed to write the store file");
        std::fs::write(source.join("attachments").join("draft.txt"), "attachment")
            .expect("failed to write the attachment");

        let target = root.join("target");
        std::fs::create_dir_all(&target).expect("failed to create the target");
        copy_dir_contents(&source, &target).expect("copy should succeed");

        assert_eq!(
            std::fs::read_to_string(target.join(STORE_FILE_NAME)).expect("store file should exist"),
            "store"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("attachments").join("draft.txt"))
                .expect("attachment should exist"),
            "attachment"
        );
    }
}
