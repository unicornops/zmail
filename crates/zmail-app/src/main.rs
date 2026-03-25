//! Zmail Phase 3 desktop bootstrap and account setup CLI.

use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use zmail_core::{
    AppBootstrap, CachePolicySummary, DraftUpdate, NewAccount, NewDraft, NewIdentity,
    OutgoingConfig, ProtocolKind, SubmissionSecurity, ThemeSummary, UpstreamPins,
};
use zmail_security::{KeyringSecretStore, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::{BackgroundRuntime, SyncService};
use zmail_theme::{SemanticTheme, ThemeFamily};

const PHASE0_THEME: &str = include_str!("../../../themes/phase0-dark.json");
const ZED_UPSTREAM_COMMIT: &str = "30ceb9176cb751b0f4f60516a3248f4ded99b082";
const GPUI_VERSION: &str = "0.2.2";

fn main() -> Result<()> {
    match Command::from_env()? {
        Command::Gui => launch_gui(),
        Command::AddImap => add_account(ProtocolKind::Imap),
        Command::AddJmap => add_account(ProtocolKind::Jmap),
        Command::ListAccounts => list_accounts(),
        Command::SyncAll => sync_accounts(None),
        Command::SyncAccount(account_id) => sync_accounts(Some(account_id)),
        Command::Search(account_id, query) => search_threads(account_id, &query),
        Command::ReadThread(thread_id) => read_thread(thread_id),
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
        Command::Help => {
            print_help();
            Ok(())
        }
    }
}

enum Command {
    Gui,
    AddImap,
    AddJmap,
    ListAccounts,
    SyncAll,
    SyncAccount(i64),
    Search(i64, String),
    ReadThread(i64),
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
                let account_id = args
                    .next()
                    .context("search requires an account id")?
                    .parse::<i64>()
                    .context("search account id must be an integer")?;
                let query = args.collect::<Vec<_>>().join(" ");
                if query.trim().is_empty() {
                    bail!("search requires a query");
                }
                Ok(Self::Search(account_id, query))
            }
            "read" => Ok(Self::ReadThread(
                args.next()
                    .context("read requires a thread id")?
                    .parse::<i64>()
                    .context("thread id must be an integer")?,
            )),
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
            "help" | "--help" | "-h" => Ok(Self::Help),
            other => bail!("unknown command {other}"),
        }
    }
}

fn launch_gui() -> Result<()> {
    let runtime = BackgroundRuntime::start(4)?;
    let store = Arc::new(open_store().context("failed to open the Phase 4 local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
    let _ = sync_service.bootstrap_existing_accounts();

    let bootstrap = build_bootstrap(runtime.summary())?;
    zmail_ui::run(bootstrap, store, sync_service, secrets);
    drop(runtime);
    Ok(())
}

fn add_account(protocol: ProtocolKind) -> Result<()> {
    let store = Arc::new(open_store().context("failed to open the local store")?);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore);
    let (account, secret) = prompt_account(protocol)?;
    let account_id = store.insert_account(&account)?;
    secrets
        .set_account_secret(account_id, &secret)
        .with_context(|| format!("failed to store credentials for account {account_id}"))?;

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

fn search_threads(account_id: i64, query: &str) -> Result<()> {
    let store = open_store().context("failed to open the local store")?;
    let hits = store.search_threads(account_id, query, 50)?;
    if hits.is_empty() {
        println!("No matches for {query:?} on account {account_id}.");
        return Ok(());
    }

    for hit in hits {
        println!("#{} [{}] {}", hit.thread_id, hit.mailbox_name, hit.subject);
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
        println!("{}", message.rendered_body);
    }

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

fn prompt_account(protocol: ProtocolKind) -> Result<(NewAccount, String)> {
    println!("Setting up a new {protocol} account.");
    let display_name = prompt("Display name")?;
    let endpoint = match protocol {
        ProtocolKind::Imap => prompt_with_default("IMAP endpoint", "imaps://mail.example.com:993")?,
        ProtocolKind::Jmap => prompt_with_default("JMAP base URL", "https://mail.example.com")?,
    };
    validate_endpoint(protocol, &endpoint)?;
    let username = prompt("Username or account email")?;
    let secret = rpassword::prompt_password("Password or app password: ")
        .context("failed to read the account password")?;
    let identity_name = prompt_with_default("Primary identity name", &display_name)?;
    let identity_email = prompt_with_default("Primary identity email", &username)?;
    let signature =
        prompt_with_default("Primary signature", &format!("Regards,\n{identity_name}"))?;
    let alias_input = prompt_optional("Additional identity emails (comma-separated, optional)")?;
    let outgoing = match protocol {
        ProtocolKind::Imap => prompt_smtp_outgoing(&endpoint, &username)?,
        ProtocolKind::Jmap => OutgoingConfig::jmap_default(),
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
            endpoint,
            username,
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(outgoing),
            identities,
        },
        secret,
    ))
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
    let theme_family = ThemeFamily::from_json_str(PHASE0_THEME)
        .context("failed to parse the bundled Zed-format theme family")?;
    let theme = theme_family
        .default_theme()
        .context("the bundled theme family does not contain any theme entries")?;
    let semantic = SemanticTheme::from_theme(theme);

    Ok(AppBootstrap {
        runtime,
        theme: ThemeSummary {
            family_name: theme_family.name.clone(),
            theme_name: theme.name.clone(),
            background: semantic.background,
            surface: semantic.surface,
            border: semantic.border,
            foreground: semantic.foreground,
            accent: semantic.accent,
        },
        upstream: UpstreamPins {
            zed_commit: ZED_UPSTREAM_COMMIT.to_owned(),
            gpui_version: GPUI_VERSION.to_owned(),
        },
    })
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
    println!("  zmail-app add-imap        Add and sync a real IMAP account");
    println!("  zmail-app add-jmap        Add and sync a real JMAP account");
    println!("  zmail-app list-accounts   Show configured accounts");
    println!("  zmail-app sync [all|ID]   Sync all accounts or one account by id");
    println!("  zmail-app search ID QUERY Search local mail for an account");
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
}

fn open_store() -> Result<LocalStore> {
    LocalStore::open(data_path())
}

fn data_path() -> PathBuf {
    PathBuf::from(".zmail-dev").join("zmail.sqlite3")
}
