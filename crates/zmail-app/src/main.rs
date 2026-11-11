//! Zmail's entry point: the desktop app, the terminal UI and the CLI.

use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use zmail_assets::ZmailAssets;
use zmail_core::{
    AppBootstrap, AuthKind, CachePolicySummary, DraftUpdate, NewAccount, NewDraft, NewIdentity,
    OutgoingConfig, ProtocolKind, ThemeAppearance, ThemeSummary, UpstreamPins,
};
use zmail_security::{InMemorySecretStore, KeyringSecretStore, SecretStore};
use zmail_store::{LocalStore, MigrationProgress, StoreOpenError};
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_theme::ThemeFamily;
use zmail_ui::InitialView;

use cli::Command;
use output::line;
use prompts::{prompt_multiline_with_default, prompt_optional, prompt_recipients};
use prompts::{prompt_with_default, prompt_yes_no};

mod account_setup;
mod cli;
mod hardening;
mod mailto;
mod output;
mod prompts;
mod ui_fixture;

const PHASE0_DARK_THEME: &str = zmail_theme::bundled::DARK;
const PHASE0_LIGHT_THEME: &str = zmail_theme::bundled::LIGHT;
/// Zed commit GPUI is built from, read from `Cargo.lock` by `build.rs`.
const ZED_UPSTREAM_COMMIT: &str = env!("ZMAIL_ZED_COMMIT");
/// Zed stable release tag GPUI is pinned to (see docs/upstream/zed.md).
const GPUI_VERSION: &str = env!("ZMAIL_ZED_TAG");

/// The exit status of a mistake on the command line.
const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    // Before anything can hold a secret: no core file may carry the store
    // key or mail to disk (ADR 0005).
    hardening::forbid_core_dumps();
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                eprintln!(
                    "error: the argument {} is not valid UTF-8\n\
                     Run `zmail-app --help` for the commands.",
                    line(&arg.to_string_lossy())
                );
                return ExitCode::from(USAGE_ERROR);
            }
        }
    }
    let (command, force) = match cli::parse(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(USAGE_ERROR);
        }
    };
    if !command.opens_a_window() {
        hardening::make_undumpable();
    }
    match run(command, force) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The chain on one line; it can quote a server or mail.
            eprintln!("Error: {}", output::text(&format!("{error:#}")));
            ExitCode::FAILURE
        }
    }
}

impl Command {
    /// Whether the command opens the desktop app's window.
    fn opens_a_window(&self) -> bool {
        matches!(
            self,
            Self::Gui
                | Self::Mailto(_)
                | Self::LaunchUiFixture { .. }
                | Self::LaunchImapFixture { .. }
        )
    }
}

fn run(command: Command, force: bool) -> Result<()> {
    match command {
        Command::Gui => launch_gui(),
        Command::Help(spec) => {
            match spec {
                Some(spec) => print!("{}", cli::command_help(spec)),
                None => print!("{}", cli::general_help()),
            }
            Ok(())
        }
        Command::Version => {
            println!("zmail-app {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Tui => launch_tui(),
        Command::LaunchTuiFixture(view) => launch_tui_fixture(view),
        Command::Mailto(url) => launch_gui_with_mailto(&url),
        Command::LaunchImapFixture {
            endpoint,
            username,
            email,
        } => launch_imap_fixture(&endpoint, &username, &email),
        Command::LaunchUiFixture {
            view,
            theme,
            text_size,
        } => launch_ui_fixture(view, theme, text_size),
        Command::AddAccount {
            protocol,
            allow_cleartext,
        } => account_setup::add_account(protocol, allow_cleartext, force),
        Command::ListAccounts => list_accounts(),
        Command::Sync(account_id) => sync_accounts(account_id, force),
        Command::Search {
            account_id,
            query,
            server,
        } => search_threads(account_id, &query, server),
        Command::IndexAccount(account_id) => index_account(account_id, force),
        Command::ReadThread(thread_id) => read_thread(thread_id),
        Command::ListAttachments(thread_id) => list_attachments(thread_id),
        Command::SaveAttachment {
            attachment_id,
            destination,
            overwrite,
        } => save_attachment(attachment_id, destination.as_deref(), overwrite),
        Command::FlagThread(thread_id, flagged) => flag_thread(thread_id, flagged),
        Command::MarkThreadRead(thread_id, read) => mark_thread_read(thread_id, read),
        Command::MoveThread(thread_id, mailbox_id) => move_thread(thread_id, mailbox_id),
        Command::ArchiveThread(thread_id) => archive_thread(thread_id),
        Command::DeleteThread { thread_id, yes } => delete_thread(thread_id, yes),
        Command::Compose(account_id, identity_id) => compose_draft(account_id, identity_id, force),
        Command::ListDrafts(account_id) => list_drafts(account_id),
        Command::ShowDraft(draft_id) => show_draft(draft_id),
        Command::SendDraft(draft_id) => send_draft(draft_id, force),
        Command::ListOutbox(account_id) => list_outbox(account_id),
        Command::RetryOutbox(job_id) => retry_outbox_job(job_id, force),
        Command::AddAttachment(draft_id, path) => add_attachment(draft_id, &path),
        Command::RemoveAttachment(attachment_id) => remove_attachment(attachment_id),
        Command::ProcessOutbox(account_id) => process_outbox(account_id, force),
        Command::Backup { directory } => create_backup(directory.as_deref()),
        Command::ResetStore => reset_unreadable_store(),
        Command::RotateKey => rotate_store_key(),
        Command::ExportAccount { account_id, path } => export_account(account_id, &path),
        Command::DeadOps { account_id } => list_dead_ops(account_id),
        Command::RetryOp(op_id) => retry_dead_op(op_id),
        Command::DiscardOp(op_id) => discard_dead_op(op_id),
        Command::EmptyTrash { account_id, yes } => empty_trash(account_id, yes, force),
        Command::CacheBudget {
            account_id,
            budget_mb,
        } => set_cache_budget(account_id, budget_mb),
    }
}

/// Launches the GUI with a compose draft pre-filled from a mailto URL.
///
/// Malformed URLs fall back to a plain GUI launch with a warning, and a
/// missing account configuration surfaces the setup flow instead.
fn launch_gui_with_mailto(url: &str) -> Result<()> {
    match mailto::parse_mailto(url) {
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
                threading: Default::default(),
            })?;
            launch_gui_with_initial_draft(Some(draft_id))
        }
        None => {
            eprintln!("warning: ignoring malformed mailto URL {}", line(url));
            launch_gui()
        }
    }
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

/// `tui`: the terminal UI (ADR 0007) on the same store as the GUI,
/// unlocked as the CLI unlocks it.
#[cfg(feature = "tui")]
fn launch_tui() -> Result<()> {
    install_panic_log();
    // Over SSH there is often no keychain service to ask (ADR 0007,
    // "Headless machines"); say so plainly.
    let store = Arc::new(open_store_explaining(
        "the terminal UI unlocks the mail store with its key from the system keychain, as \
         the Zmail window does, and the keychain didn't answer. Over SSH, start or unlock \
         the keychain service on that machine (Secret Service on Linux) and try again; the \
         store never opens without its key",
    )?);
    let runtime = BackgroundRuntime::start(4)?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync =
        SyncService::with_sync_lock(runtime.handle().clone(), store.clone(), secrets.clone())?;
    #[cfg(feature = "proton")]
    sync.configure_proton(zmail_sync::proton_production_config(env!(
        "CARGO_PKG_VERSION"
    ))?)?;
    let result = zmail_tui::run(zmail_tui::Launch {
        store,
        sync: Some(sync),
        runtime: runtime.handle().clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        secrets: Some(secrets),
        keymap: Some(data_dir()?.join(zmail_tui::KEYMAP_FILE)),
    });
    drop(runtime);
    result
}

/// `launch-tui-fixture`: the terminal UI on an in-memory store seeded with
/// the GUI fixture's synthetic mail (`ui_fixture`), with no sync, for
/// trying it out and for checks in a real terminal.
#[cfg(feature = "tui")]
fn launch_tui_fixture(view: ui_fixture::FixtureView) -> Result<()> {
    let runtime = BackgroundRuntime::start(1)?;
    let store = Arc::new(LocalStore::open_ephemeral().context("failed to open the fixture store")?);
    ui_fixture::seed(&store, view, ui_fixture::anchor_time())?;
    let result = zmail_tui::run(zmail_tui::Launch {
        store,
        sync: None,
        runtime: runtime.handle().clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        // A keychain in memory, so Setup → Accounts can be tried on the
        // fixture without touching the real one.
        secrets: Some(InMemorySecretStore::shared()),
        // The user's keymap, as `tui` reads it, so it can be tried here.
        keymap: Some(data_dir()?.join(zmail_tui::KEYMAP_FILE)),
    });
    drop(runtime);
    result
}

#[cfg(not(feature = "tui"))]
fn launch_tui() -> Result<()> {
    bail!("this build of Zmail has no terminal UI")
}

#[cfg(not(feature = "tui"))]
fn launch_tui_fixture(_view: ui_fixture::FixtureView) -> Result<()> {
    bail!("this build of Zmail has no terminal UI")
}

/// The environment variable `launch-imap-fixture` reads the password
/// from, so it never shows in `ps` or the shell's history.
const FIXTURE_SECRET_ENV: &str = "ZMAIL_FIXTURE_SECRET";

/// `launch-imap-fixture`'s password: from [`FIXTURE_SECRET_ENV`], else
/// asked for without echo on a terminal, else one line of standard input.
fn fixture_secret() -> Result<String> {
    if let Some(secret) = std::env::var_os(FIXTURE_SECRET_ENV)
        && !secret.is_empty()
    {
        return secret
            .into_string()
            .map_err(|_| anyhow::anyhow!("{FIXTURE_SECRET_ENV} is not valid UTF-8"));
    }
    if io::stdin().is_terminal() {
        return prompts::prompt_secret("Password or app password");
    }
    let mut secret = String::new();
    io::stdin()
        .read_line(&mut secret)
        .context("failed to read the password from standard input")?;
    let secret = secret.trim_end_matches(['\r', '\n']);
    if secret.is_empty() {
        bail!("give the password in {FIXTURE_SECRET_ENV} or on standard input");
    }
    Ok(secret.to_owned())
}

fn launch_imap_fixture(endpoint: &str, username: &str, email: &str) -> Result<()> {
    install_panic_log();
    account_setup::validate_endpoint(ProtocolKind::Imap, endpoint, false)?;
    let secret = fixture_secret()?;

    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(LocalStore::open_ephemeral().context("failed to open the fixture store")?);
    let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
    let account_id = store
        .insert_account(&fixture_imap_account(endpoint, username, email))
        .context("failed to insert the IMAP fixture account")?;
    secrets
        .set_account_secret(account_id, &secret)
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

fn list_accounts() -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    output::write_accounts(&mut io::stdout().lock(), &store.list_accounts()?)?;
    Ok(())
}

fn sync_accounts(account_id: Option<i64>, force: bool) -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    sync_accounts_in(&runtime, store, secrets, account_id, force)
}

/// `sync`, on an open store.
fn sync_accounts_in(
    runtime: &BackgroundRuntime,
    store: Arc<LocalStore>,
    secrets: Arc<dyn SecretStore>,
    account_id: Option<i64>,
    force: bool,
) -> Result<()> {
    let sync_service = syncing_service(runtime, store, secrets, force, "sync")?;

    if let Some(account_id) = account_id {
        sync_service.sync_account_and_wait(account_id)?;
        println!("Finished syncing account {account_id}.");
    } else {
        sync_service.sync_all_and_wait()?;
        println!("Finished syncing all configured accounts.");
    }

    Ok(())
}

fn index_account(account_id: i64, force: bool) -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = syncing_service(&runtime, store.clone(), secrets, force, "index")?;

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
        // The server search only reads from the server, so it runs beside
        // another Zmail that syncs; mail it finds that the store lacks is
        // then left to that Zmail's sync.
        let sync_service =
            SyncService::for_command(runtime.handle().clone(), store.clone(), secrets)?;
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
        println!(
            "No matches for \"{}\" on account {account_id}.",
            line(query)
        );
        return Ok(());
    }
    output::write_search_hits(&mut io::stdout().lock(), &hits)?;
    Ok(())
}

fn read_thread(thread_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let thread = store
        .thread_view(thread_id)?
        .with_context(|| format!("thread {thread_id} does not exist"))?;
    output::write_thread(&mut io::stdout().lock(), &thread)?;
    Ok(())
}

fn list_attachments(thread_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let thread = store
        .thread_view(thread_id)?
        .with_context(|| format!("thread {thread_id} does not exist"))?;
    output::write_attachments(&mut io::stdout().lock(), &thread)?;
    Ok(())
}

fn save_attachment(attachment_id: i64, destination: Option<&Path>, overwrite: bool) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let path = save_attachment_in(
        &store,
        attachment_id,
        destination.unwrap_or(Path::new(".")),
        overwrite,
    )?;
    println!(
        "Saved attachment {attachment_id} to {}.",
        line(&path.display().to_string())
    );
    Ok(())
}

/// `save-attachment`, on an open store: saves under the attachment's
/// own (sanitised) name in `directory`, and never replaces a file there
/// unless `overwrite`. Returns where it went.
fn save_attachment_in(
    store: &LocalStore,
    attachment_id: i64,
    directory: &Path,
    overwrite: bool,
) -> Result<PathBuf> {
    let blob = store.attachment_blob(attachment_id)?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let destination = directory.join(zmail_security::attachment_safety::sanitize_filename(
        blob.filename(),
    ));
    if overwrite {
        blob.save_to(&destination)?;
    } else {
        // `save_new` creates the file only if nothing is there, so a file
        // that appears meanwhile is not replaced either.
        blob.save_new(&destination).map_err(|error| {
            if destination.exists() {
                anyhow::anyhow!(
                    "{} already exists; pass --overwrite to replace it, or --dest to save \
                     somewhere else",
                    destination.display()
                )
            } else {
                error
            }
        })?;
    }
    Ok(destination)
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

fn delete_thread(thread_id: i64, yes: bool) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    if deletes_for_good(&store, thread_id)?
        && !prompts::confirm(
            "This conversation is in the Trash, or its account has no Trash, so it is deleted \
             for good. Delete it",
            yes,
        )?
    {
        println!("Nothing was deleted.");
        return Ok(());
    }
    store.delete_thread(thread_id)?;
    println!("Thread {thread_id} deleted.");
    Ok(())
}

/// Whether deleting the thread destroys it rather than moving it to the
/// Trash: it is in the Trash already, or its account has none (as
/// [`LocalStore::delete_thread`] decides).
fn deletes_for_good(store: &LocalStore, thread_id: i64) -> Result<bool> {
    let thread = store
        .thread_view(thread_id)?
        .with_context(|| format!("thread {thread_id} does not exist"))?;
    let Some(mailbox) = store
        .list_all_mailboxes()?
        .into_iter()
        .find(|mailbox| mailbox.id == thread.mailbox_id)
    else {
        return Ok(false);
    };
    let trash = store.mailbox_id_by_role(mailbox.account_id, "trash")?;
    Ok(trash.is_none_or(|trash| trash == mailbox.id))
}

fn compose_draft(account_id: i64, identity_id: Option<i64>, force: bool) -> Result<()> {
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
        threading: Default::default(),
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
        println!(
            "{}",
            send_draft_in(&runtime, store, secrets, draft_id, force)?
        );
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
    output::write_drafts(&mut io::stdout().lock(), &drafts)?;
    Ok(())
}

fn show_draft(draft_id: i64) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let draft = store
        .draft_detail(draft_id)?
        .with_context(|| format!("draft {draft_id} does not exist"))?;
    output::write_draft(&mut io::stdout().lock(), &draft)?;
    Ok(())
}

fn send_draft(draft_id: i64, force: bool) -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    println!(
        "{}",
        send_draft_in(&runtime, store, secrets, draft_id, force)?
    );
    Ok(())
}

/// Queues a draft and sends it now, or, while another Zmail syncs the
/// store, leaves the sending to it (its outbox poll sends it within
/// seconds). Returns what to tell the user.
fn send_draft_in(
    runtime: &BackgroundRuntime,
    store: Arc<LocalStore>,
    secrets: Arc<dyn SecretStore>,
    draft_id: i64,
    force: bool,
) -> Result<String> {
    let account_id = store
        .draft_detail(draft_id)?
        .with_context(|| format!("draft {draft_id} does not exist"))?
        .account_id;
    let sync_service = command_sync_service(runtime, store, secrets, force)?;
    let job_id = sync_service.queue_draft_and_send(draft_id)?;
    if !sync_service.runs_sync() {
        return Ok(format!(
            "Draft {draft_id} queued as outbox job {job_id}. {SYNCED_ELSEWHERE}, and sends \
             it within seconds."
        ));
    }
    let _ = sync_service.process_outbox_and_wait(Some(account_id))?;
    Ok(format!("Draft {draft_id} queued as outbox job {job_id}."))
}

fn list_outbox(account_id: Option<i64>) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let jobs = store.list_outbox_jobs(account_id)?;
    if jobs.is_empty() {
        println!("No outbox jobs found.");
        return Ok(());
    }
    output::write_outbox(&mut io::stdout().lock(), &jobs)?;
    Ok(())
}

fn retry_outbox_job(job_id: i64, force: bool) -> Result<()> {
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let runtime = BackgroundRuntime::start(4)?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    println!(
        "{}",
        retry_outbox_job_in(&runtime, store, secrets, job_id, force)?
    );
    Ok(())
}

/// `retry-outbox`, on an open store: requeues a failed job and, when this
/// process syncs, sends it.
fn retry_outbox_job_in(
    runtime: &BackgroundRuntime,
    store: Arc<LocalStore>,
    secrets: Arc<dyn SecretStore>,
    job_id: i64,
    force: bool,
) -> Result<String> {
    store.retry_outbox_job(job_id)?;
    let sync_service = command_sync_service(runtime, store, secrets, force)?;
    if !sync_service.runs_sync() {
        return Ok(format!(
            "Outbox job {job_id} requeued. {SYNCED_ELSEWHERE}, and sends it within seconds."
        ));
    }
    let processed = sync_service.process_outbox_and_wait(None)?;
    Ok(format!(
        "Outbox job {job_id} requeued. Processed {processed} due job(s)."
    ))
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

fn process_outbox(account_id: Option<i64>, force: bool) -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let processed = process_outbox_in(&runtime, store, secrets, account_id, force)?;
    println!("Processed {processed} due outbox job(s).");
    Ok(())
}

/// `process-outbox`, on an open store: how many jobs it sent or failed.
fn process_outbox_in(
    runtime: &BackgroundRuntime,
    store: Arc<LocalStore>,
    secrets: Arc<dyn SecretStore>,
    account_id: Option<i64>,
    force: bool,
) -> Result<usize> {
    syncing_service(runtime, store, secrets, force, "process-outbox")?
        .process_outbox_and_wait(account_id)
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

    let mut out = io::stdout().lock();
    let mut printed = 0;
    for account_id in accounts {
        for op in store.list_dead_ops(account_id)? {
            output::write_dead_op(&mut out, &op)?;
            printed += 1;
        }
    }
    writeln!(out, "{printed} dead-lettered operation(s).")?;
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

fn empty_trash(account_id: i64, yes: bool, force: bool) -> Result<()> {
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let trash_name = store
        .mailbox_name_by_role(account_id, "trash")?
        .with_context(|| format!("account {account_id} has no trash mailbox"))?;
    let target = store
        .sync_target(account_id)?
        .with_context(|| format!("account {account_id} no longer exists"))?;

    let question = format!(
        "This expunges every message in the {} mailbox of account {account_id} ({}) on the \
         server, for good. Empty it",
        line(&trash_name),
        line(&target.display_name)
    );
    if !prompts::confirm(&question, yes)? {
        println!("Nothing was deleted.");
        return Ok(());
    }

    let runtime = BackgroundRuntime::start(4)?;
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = command_sync_service(&runtime, store.clone(), secrets, force)?;
    let secret = runtime
        .handle()
        .block_on(sync_service.connect_secret(&target))
        .context("failed to resolve the account credentials")?;
    let expunged =
        runtime
            .handle()
            .block_on(zmail_imap::empty_trash(&target, &secret, &trash_name))?;

    if !sync_service.runs_sync() {
        println!(
            "Expunged {expunged} message(s) from {}. {SYNCED_ELSEWHERE}, and its next sync \
             updates the local mirror.",
            line(&trash_name)
        );
        return Ok(());
    }
    println!(
        "Expunged {expunged} message(s) from {}; syncing the local mirror.",
        line(&trash_name)
    );
    let _ = sync_service.sync_account_and_wait(account_id);
    Ok(())
}

fn create_backup(directory: Option<&Path>) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let directory = match directory {
        Some(directory) => directory.to_owned(),
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

fn export_account(account_id: i64, path: &Path) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let exported = export_account_in(&store, account_id, path)?;
    println!("Exported {exported} message(s) to {}.", path.display());
    Ok(())
}

/// `export-account`, on an open store. The mbox holds the mail in
/// plaintext, so a new file is readable by its owner only (Unix).
fn export_account_in(store: &LocalStore, account_id: i64, path: &Path) -> Result<usize> {
    let mut writer = hardening::private_file()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("failed to create the export file {}", path.display()))?;
    store.export_account_mbox(account_id, &mut writer)
}

fn fixture_imap_account(endpoint: &str, username: &str, email: &str) -> NewAccount {
    NewAccount {
        display_name: "IMAP Fixture".to_owned(),
        protocol: ProtocolKind::Imap,
        auth: AuthKind::Password,
        endpoint: endpoint.to_owned(),
        username: username.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: Some(OutgoingConfig::smtp_default(account_setup::derive_host(
            endpoint,
        ))),
        identities: vec![NewIdentity {
            display_name: "Fixture Recipient".to_owned(),
            email_address: email.to_owned(),
            signature: "Regards,\nFixture Recipient".to_owned(),
            is_primary: true,
        }],
    }
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
            line(&identity.display_name),
            line(&identity.email_address),
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

/// What a command that leaves sync or sending to another Zmail says: only
/// one process syncs a store (ADR 0007).
const SYNCED_ELSEWHERE: &str =
    "Another Zmail (the desktop app or `zmail-app tui`) is syncing this mail store";

/// The sync service for a command that syncs or sends. With the store's
/// sync lock free, it takes the lock and holds it while the command runs,
/// so a desktop app or terminal UI started meanwhile leaves sync to it.
/// While another Zmail holds the lock, it leaves syncing and sending to
/// that process (`runs_sync()` is false); with `force` it syncs here
/// anyway, beside it (the store's outbox claims still keep a message from
/// going out twice).
fn command_sync_service(
    runtime: &BackgroundRuntime,
    store: Arc<LocalStore>,
    secrets: Arc<dyn SecretStore>,
    force: bool,
) -> Result<SyncService> {
    let handle = runtime.handle().clone();
    let service = SyncService::for_command(handle.clone(), store.clone(), secrets.clone())?;
    if service.runs_sync() || !force {
        return Ok(service);
    }
    eprintln!("warning: {SYNCED_ELSEWHERE}; --force syncs here too.");
    Ok(SyncService::new(handle, store, secrets))
}

/// The sync service for a command that does nothing but sync, index or
/// send (`command`), or why it won't run: another Zmail syncs the store
/// and `force` wasn't given.
fn syncing_service(
    runtime: &BackgroundRuntime,
    store: Arc<LocalStore>,
    secrets: Arc<dyn SecretStore>,
    force: bool,
    command: &str,
) -> Result<SyncService> {
    let service = command_sync_service(runtime, store, secrets, force)?;
    if !service.runs_sync() {
        bail!(
            "{SYNCED_ELSEWHERE}, and only one process syncs a store at a time; it syncs and \
             sends on its own. Pass --force to run `zmail-app {command}` beside it anyway."
        );
    }
    Ok(service)
}

/// Opens the encrypted store with its key from the OS keychain (ADR 0005),
/// encrypting a plaintext store first and printing its progress.
fn open_store() -> Result<LocalStore> {
    open_store_explaining(
        "Zmail needs the system keychain to unlock its mail store; unlock it and try again",
    )
}

/// [`open_store`], explaining a keychain that didn't answer with
/// `keychain_help`.
fn open_store_explaining(keychain_help: &'static str) -> Result<LocalStore> {
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
        Err(StoreOpenError::Keychain(error)) => Err(error.context(keychain_help)),
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

/// The store directory early development builds kept in the current
/// directory. It is no longer opened or moved: a `.zmail-dev` folder in
/// an archive or a cloned repository would otherwise become the user's
/// store.
const LEGACY_STORE_DIR: &str = ".zmail-dev";

/// Environment variable that overrides the store directory for development.
const DATA_DIR_ENV: &str = "ZMAIL_DATA_DIR";

/// The store's path, in the platform data directory (or
/// [`DATA_DIR_ENV`]), wherever the app was started from.
fn store_path() -> Result<PathBuf> {
    let dir = data_dir()?;
    let store = dir.join(STORE_FILE_NAME);
    if !store.exists() && Path::new(LEGACY_STORE_DIR).join(STORE_FILE_NAME).exists() {
        eprintln!(
            "note: ignoring the {LEGACY_STORE_DIR} store in the current folder; Zmail's store \
             is in {}",
            dir.display()
        );
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
    // Panic messages can quote mail, so the log is the user's only.
    if let Err(error) = hardening::create_private_dir_all(&log_dir) {
        eprintln!(
            "warning: no crash log will be kept: failed to create {}: {error}",
            log_dir.display()
        );
        return;
    }
    let log_path = log_dir.join(CRASH_LOG_FILE_NAME);
    rotate_crash_log(&log_path);
    hardening::make_private(&log_path);
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
        let _ = hardening::private_file()
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
    use zmail_core::SubmissionSecurity;

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

    /// A one-connection SMTP server that takes a message, slowly (half a
    /// second before it answers the message's end), and returns the
    /// commands and message lines it was sent.
    fn slow_smtp_server() -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader};

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("the mock binds");
        let port = listener.local_addr().expect("the mock has a port").port();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let Ok((stream, _)) = listener.accept() else {
                return seen;
            };
            let mut writer = stream.try_clone().expect("the stream clones");
            let mut reader = BufReader::new(stream);
            let _ = writer.write_all(b"220 mock.example.test ESMTP\r\n");
            let mut in_data = false;
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                let text = line.trim_end().to_owned();
                line.clear();
                let upper = text.to_ascii_uppercase();
                let reply: &[u8] = if in_data {
                    if text != "." {
                        seen.push(text);
                        continue;
                    }
                    in_data = false;
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    b"250 2.0.0 queued\r\n"
                } else if upper.starts_with("EHLO") {
                    b"250-mock.example.test\r\n250 AUTH PLAIN\r\n"
                } else if upper.starts_with("AUTH") {
                    b"235 2.7.0 accepted\r\n"
                } else if upper.starts_with("DATA") {
                    in_data = true;
                    b"354 go ahead\r\n"
                } else if upper.starts_with("QUIT") {
                    b"221 bye\r\n"
                } else {
                    b"250 ok\r\n"
                };
                seen.push(text);
                if writer.write_all(reply).is_err() || upper.starts_with("QUIT") {
                    break;
                }
            }
            seen
        });
        (port, server)
    }

    #[test]
    fn send_returns_once_the_message_has_gone_out() {
        let (port, server) = slow_smtp_server();
        let store = Arc::new(LocalStore::open_ephemeral().expect("the store opens"));
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "Test Account".to_owned(),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::Password,
                endpoint: "imaps://mail.example.test:993".to_owned(),
                username: "user@example.test".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: Some(OutgoingConfig {
                    kind: zmail_core::SubmissionKind::Smtp,
                    host: Some("127.0.0.1".to_owned()),
                    port: Some(port),
                    username: Some("user@example.test".to_owned()),
                    security: Some(SubmissionSecurity::Plain),
                    hello_name: "localhost".to_owned(),
                }),
                identities: vec![NewIdentity {
                    display_name: "Test Account".to_owned(),
                    email_address: "user@example.test".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("an account");
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        secrets
            .set_account_secret(account_id, "password")
            .expect("the password stores");
        let identity_id = store.list_identities(account_id).expect("identities")[0].id;
        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id,
                to: vec!["ada@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Lunch".to_owned(),
                body: "Noon?".to_owned(),
                threading: Default::default(),
            })
            .expect("a draft");

        let runtime = BackgroundRuntime::start(2).expect("a runtime");
        send_draft_in(&runtime, store.clone(), secrets, draft_id, false).expect("send runs");
        // The command exits (dropping the runtime) as soon as it returns,
        // so the message must be out by then, whichever pass sent it.
        let jobs = store.list_outbox_jobs(None).expect("the outbox");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, zmail_core::OutboxJobState::Sent, "{jobs:?}");
        drop(runtime);
        let seen = server.join().expect("the mock finishes");
        assert!(seen.iter().any(|line| line == "Noon?"), "{seen:?}");
    }

    #[test]
    fn retry_outbox_sends_an_interrupted_message_once() {
        let (port, server) = slow_smtp_server();
        let store = Arc::new(LocalStore::open_ephemeral().expect("the store opens"));
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "Test Account".to_owned(),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::Password,
                endpoint: "imaps://mail.example.test:993".to_owned(),
                username: "user@example.test".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: Some(OutgoingConfig {
                    kind: zmail_core::SubmissionKind::Smtp,
                    host: Some("127.0.0.1".to_owned()),
                    port: Some(port),
                    username: Some("user@example.test".to_owned()),
                    security: Some(SubmissionSecurity::Plain),
                    hello_name: "localhost".to_owned(),
                }),
                identities: vec![NewIdentity {
                    display_name: "Test Account".to_owned(),
                    email_address: "user@example.test".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("an account");
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        secrets
            .set_account_secret(account_id, "password")
            .expect("the password stores");
        let identity_id = store.list_identities(account_id).expect("identities")[0].id;
        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id,
                to: vec!["ada@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Lunch".to_owned(),
                body: "Noon?".to_owned(),
                threading: Default::default(),
            })
            .expect("a draft");
        // A send Zmail stopped in the middle of, as the process that syncs
        // records it for an SMTP account.
        let job_id = store.queue_draft(draft_id).expect("queued");
        store
            .claim_due_outbox_job(None)
            .expect("the claim runs")
            .expect("due");
        store
            .record_outbox_failure(job_id, zmail_core::INTERRUPTED_SEND_MESSAGE)
            .expect("failed");

        let runtime = BackgroundRuntime::start(2).expect("a runtime");
        let said = retry_outbox_job_in(&runtime, store.clone(), secrets, job_id, false)
            .expect("retry-outbox runs");
        assert!(
            said.starts_with(&format!("Outbox job {job_id} requeued. Processed ")),
            "{said}"
        );
        let jobs = store.list_outbox_jobs(None).expect("the outbox");
        assert_eq!(jobs[0].state, zmail_core::OutboxJobState::Sent, "{jobs:?}");
        assert_eq!(jobs[0].attempt_count, 2);
        drop(runtime);
        let seen = server.join().expect("the mock finishes");
        assert_eq!(
            seen.iter().filter(|line| *line == "Noon?").count(),
            1,
            "{seen:?}"
        );
    }

    /// Set in the child process that
    /// `a_command_beside_a_zmail_that_syncs_stops_or_leaves_it_the_work`
    /// starts: the store it opens.
    const CHILD_STORE: &str = "ZMAIL_APP_SYNC_LOCK_CHILD_STORE";

    /// The fixed key both processes open the store with.
    fn fixture_key() -> zmail_security::vault::StoreKey {
        zmail_security::vault::StoreKey::from_fixture(
            zmail_security::vault::KeyId::from_bytes([9; 16]),
            [24; 32],
        )
    }

    #[test]
    fn a_command_beside_a_zmail_that_syncs_stops_or_leaves_it_the_work() {
        let directory = temp_dir("sync-lock");
        let path = directory.join(STORE_FILE_NAME);
        let store = LocalStore::open(&path, fixture_key()).expect("the store opens");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("an account");
        let identity_id = store.list_identities(account_id).expect("identities")[0].id;
        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id,
                to: vec!["ada@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Lunch".to_owned(),
                body: "Noon?".to_owned(),
                threading: Default::default(),
            })
            .expect("a draft");
        // This process stands in for the desktop app: it holds the lock.
        let held = store
            .try_sync_lock()
            .expect("the lock file opens")
            .expect("the lock is free");

        let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args(["--exact", "tests::sync_lock_child", "--nocapture"])
            .env(CHILD_STORE, &path)
            .env("ZMAIL_TEST_DRAFT", draft_id.to_string())
            .output()
            .expect("the child runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "child failed: {stdout}");
        for expected in [
            "child: sync refused: Another Zmail (the desktop app or `zmail-app tui`) is \
             syncing this mail store, and only one process syncs a store at a time; it syncs \
             and sends on its own. Pass --force to run `zmail-app sync` beside it anyway.",
            "child: process-outbox refused: Another Zmail",
            "Pass --force to run `zmail-app process-outbox` beside it anyway.",
            "child: process-outbox --force: 0",
            "child: send: Draft",
            "Another Zmail (the desktop app or `zmail-app tui`) is syncing this mail store, \
             and sends it within seconds.",
        ] {
            assert!(stdout.contains(expected), "{expected:?} in {stdout}");
        }
        // The message waits in the store for the Zmail that syncs.
        let jobs = store.list_outbox_jobs(None).expect("the outbox");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].draft_id, draft_id);
        assert_eq!(jobs[0].state, zmail_core::OutboxJobState::Queued);
        assert_eq!(jobs[0].attempt_count, 0, "the child sent nothing");
        drop(held);
        drop(store);
        let _ = std::fs::remove_dir_all(directory);
    }

    /// The child half of
    /// `a_command_beside_a_zmail_that_syncs_stops_or_leaves_it_the_work`;
    /// does nothing in a normal test run.
    #[test]
    fn sync_lock_child() {
        let Some(path) = std::env::var_os(CHILD_STORE) else {
            return;
        };
        let draft_id = std::env::var("ZMAIL_TEST_DRAFT")
            .expect("the draft")
            .parse::<i64>()
            .expect("a draft id");
        let store = Arc::new(
            LocalStore::open(std::path::Path::new(&path), fixture_key()).expect("the store opens"),
        );
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        let runtime = BackgroundRuntime::start(1).expect("a runtime");
        match sync_accounts_in(&runtime, store.clone(), secrets.clone(), None, false) {
            Ok(()) => println!("child: sync ran"),
            Err(error) => println!("child: sync refused: {error:#}"),
        }
        match process_outbox_in(&runtime, store.clone(), secrets.clone(), None, false) {
            Ok(count) => println!("child: process-outbox ran: {count}"),
            Err(error) => println!("child: process-outbox refused: {error:#}"),
        }
        // Nothing is due yet, so the forced pass sends nothing.
        match process_outbox_in(&runtime, store.clone(), secrets.clone(), None, true) {
            Ok(count) => println!("child: process-outbox --force: {count}"),
            Err(error) => println!("child: process-outbox --force failed: {error:#}"),
        }
        match send_draft_in(&runtime, store, secrets, draft_id, false) {
            Ok(said) => println!("child: send: {said}"),
            Err(error) => println!("child: send failed: {error:#}"),
        }
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

    /// Escape sequences a hostile message carries: a clipboard write
    /// (OSC 52), a title change, a screen clear, an 8-bit CSI and a
    /// right-to-left override.
    const HOSTILE: &str =
        "\u{1b}]52;c;cHduZWQ=\u{07}\u{1b}]0;owned\u{07}\u{1b}[2J\u{9b}31m\u{202e}";

    /// Whether `text` holds anything a terminal would act on.
    fn has_terminal_controls(text: &str) -> bool {
        text.chars()
            .any(|ch| (ch.is_control() && ch != '\n') || ('\u{202a}'..='\u{202e}').contains(&ch))
    }

    /// An ephemeral store with one account whose mail is hostile: an
    /// Inbox conversation with an attachment (and its blob), and one in
    /// the Trash. Returns the store, the account, and the two threads.
    fn hostile_store() -> (LocalStore, i64, i64, i64) {
        use zmail_core::{
            AccountSyncSnapshot, MailboxSnapshot, MessageAttachment, MessageHeaders,
            MessageSnapshot, ThreadSnapshot,
        };

        let store = LocalStore::open_ephemeral().expect("the store opens");
        let account_id = store
            .insert_account(&NewAccount {
                display_name: format!("Work{HOSTILE}"),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::Password,
                endpoint: "imaps://mail.example.test:993".to_owned(),
                username: format!("user{HOSTILE}@example.test"),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: Some(OutgoingConfig::smtp_default("mail.example.test".to_owned())),
                identities: vec![NewIdentity {
                    display_name: "Sam".to_owned(),
                    email_address: "sam@example.test".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("an account");
        let blobs = store
            .sync_target(account_id)
            .expect("the target loads")
            .expect("the account exists")
            .blobs;
        let storage_path = blobs
            .write("hostile", b"attachment bytes")
            .expect("the blob stores");
        let thread = |id: &str, attachments: Vec<MessageAttachment>| ThreadSnapshot {
            remote_thread_id: id.to_owned(),
            subject: format!("Invoice{HOSTILE}"),
            participants: format!("Mallory{HOSTILE}"),
            preview: format!("Pay now{HOSTILE}"),
            is_flagged: false,
            has_attachments: !attachments.is_empty(),
            unread_count: 1,
            latest_message_at: "2026-11-01T09:00:00Z".to_owned(),
            messages: vec![MessageSnapshot {
                remote_id: format!("{id}-1"),
                identity_email: "sam@example.test".to_owned(),
                sender: format!("Mallory{HOSTILE} <mallory@example.test>"),
                reply_to: None,
                recipients: "Sam <sam@example.test>".to_owned(),
                subject: format!("Invoice{HOSTILE}"),
                body_preview: format!("Pay now{HOSTILE}"),
                body_text: Some(format!("Pay now{HOSTILE}\nSecond line\u{1b}[1A")),
                raw_html: None,
                sanitized_html: None,
                has_remote_content: false,
                is_read: false,
                is_flagged: false,
                received_at: "2026-11-01T09:00:00Z".to_owned(),
                attachments,
                headers: MessageHeaders::default(),
            }],
        };
        let snapshot = AccountSyncSnapshot {
            checkpoint: "test:1".to_owned(),
            detail: "Up to date".to_owned(),
            identities: None,
            mailboxes: vec![
                MailboxSnapshot {
                    name: format!("Inbox{HOSTILE}"),
                    role: Some("inbox".to_owned()),
                    threads: vec![thread(
                        "inbox-thread",
                        vec![MessageAttachment {
                            filename: format!("report{HOSTILE}.pdf"),
                            content_type: format!("application/pdf{HOSTILE}"),
                            size_bytes: 16,
                            storage_path,
                            content_id: None,
                            inline_only: false,
                        }],
                    )],
                    ..Default::default()
                },
                MailboxSnapshot {
                    name: "Trash".to_owned(),
                    role: Some("trash".to_owned()),
                    threads: vec![thread("trash-thread", Vec::new())],
                    ..Default::default()
                },
            ],
        };
        store
            .replace_account_snapshot(account_id, &snapshot)
            .expect("the snapshot stores");
        let mailboxes = store.list_mailboxes(account_id).expect("mailboxes");
        let thread_in = |role: zmail_core::MailboxRole| {
            let mailbox = mailboxes
                .iter()
                .find(|mailbox| mailbox.role == Some(role))
                .expect("the mailbox");
            store
                .list_threads(mailbox.id, 0..10)
                .expect("threads")
                .first()
                .expect("a thread")
                .id
        };
        let inbox_thread = thread_in(zmail_core::MailboxRole::Inbox);
        let trash_thread = thread_in(zmail_core::MailboxRole::Trash);
        (store, account_id, inbox_thread, trash_thread)
    }

    #[test]
    fn mail_strings_reach_the_terminal_filtered() {
        let (store, account_id, thread_id, _) = hostile_store();
        let thread = store
            .thread_view(thread_id)
            .expect("the thread loads")
            .expect("the thread exists");
        let mut printed = Vec::new();
        output::write_thread(&mut printed, &thread).expect("read prints");
        output::write_attachments(&mut printed, &thread).expect("attachments print");
        let hits = store
            .search_threads(account_id, "Invoice", 50)
            .expect("the search runs");
        assert!(!hits.is_empty(), "the search finds the conversation");
        output::write_search_hits(&mut printed, &hits).expect("search prints");
        output::write_accounts(&mut printed, &store.list_accounts().expect("accounts"))
            .expect("list-accounts prints");

        // A draft from a hostile mailto: link.
        let mailto = mailto::parse_mailto(
            "mailto:ada@example.com?subject=Hi%1B%5D52%3Bc%3BcHduZWQ%3D%07&body=Body%1B%5B2J",
        )
        .expect("the link parses");
        let identity_id = store.list_identities(account_id).expect("identities")[0].id;
        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id,
                to: mailto.to,
                cc: mailto.cc,
                bcc: mailto.bcc,
                subject: mailto.subject,
                body: mailto.body,
                threading: Default::default(),
            })
            .expect("a draft");
        let draft = store
            .draft_detail(draft_id)
            .expect("the draft loads")
            .expect("the draft exists");
        output::write_draft(&mut printed, &draft).expect("draft prints");
        output::write_drafts(&mut printed, &store.list_drafts(None).expect("drafts"))
            .expect("drafts print");

        let printed = String::from_utf8(printed).expect("UTF-8");
        assert!(!has_terminal_controls(&printed), "{printed:?}");
        // The text itself survives.
        for expected in [
            "Invoice",
            "Mallory",
            "Pay now",
            "Second line",
            "report",
            "Work",
            "Body",
        ] {
            assert!(printed.contains(expected), "{expected} in {printed}");
        }
    }

    #[test]
    fn save_attachment_never_replaces_a_file_unless_asked() {
        let (store, _, thread_id, _) = hostile_store();
        let attachment_id = store
            .thread_view(thread_id)
            .expect("the thread loads")
            .expect("the thread exists")
            .messages[0]
            .attachments[0]
            .id;
        let directory = temp_dir("save-attachment");

        let saved = save_attachment_in(&store, attachment_id, &directory, false).expect("saved");
        assert!(saved.starts_with(&directory), "{}", saved.display());
        assert!(
            !has_terminal_controls(&saved.display().to_string()),
            "{saved:?}"
        );
        assert_eq!(
            std::fs::read(&saved).expect("the file"),
            b"attachment bytes"
        );

        std::fs::write(&saved, b"the user's own file").expect("the user writes a file");
        let error =
            save_attachment_in(&store, attachment_id, &directory, false).expect_err("refused");
        assert!(format!("{error:#}").contains("--overwrite"), "{error:#}");
        assert_eq!(
            std::fs::read(&saved).expect("the file"),
            b"the user's own file"
        );

        save_attachment_in(&store, attachment_id, &directory, true).expect("overwritten");
        assert_eq!(
            std::fs::read(&saved).expect("the file"),
            b"attachment bytes"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn delete_knows_when_it_is_for_good() {
        let (store, _, inbox_thread, trash_thread) = hostile_store();
        assert!(!deletes_for_good(&store, inbox_thread).expect("checked"));
        assert!(deletes_for_good(&store, trash_thread).expect("checked"));
    }

    #[cfg(unix)]
    #[test]
    fn the_mbox_export_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let (store, account_id, _, _) = hostile_store();
        let directory = temp_dir("export");
        let path = directory.join("export.mbox");
        let exported = export_account_in(&store, account_id, &path).expect("exported");
        assert!(exported > 0);
        let mode = std::fs::metadata(&path)
            .expect("the file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(directory);
    }
}
