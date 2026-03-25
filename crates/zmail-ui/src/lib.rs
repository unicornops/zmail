//! Native GPUI shell for Zmail through Phase 4.

use std::{ops::Range, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail};
use gpui::{
    AnyElement, App, Application, Bounds, Context, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, Render, Stateful, StatefulInteractiveElement, Styled, Timer,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
    uniform_list,
};
use url::Url;
use zmail_core::{
    APP_NAME, AccountOverview, AccountSummary, AppBootstrap, CachePolicySummary, DraftAttachment,
    DraftDetail, DraftSummary, DraftUpdate, IdentitySummary, MailboxSummary, MessageRenderMode,
    NewAccount, NewDraft, NewIdentity, OutboxJobSummary, OutgoingConfig, PHASE_LABEL, ProtocolKind,
    SearchHit, StoreStats, SubmissionSecurity, SyncPhase, ThreadView,
};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

/// Runs the native Phase 4 shell.
pub fn run(
    bootstrap: AppBootstrap,
    store: Arc<LocalStore>,
    sync_service: SyncService,
    secrets: Arc<dyn SecretStore>,
) {
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1440.0), px(920.0)), cx);
        let bootstrap = bootstrap.clone();
        let store = store.clone();
        let sync_service = sync_service.clone();
        let secrets = secrets.clone();

        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, cx| {
                let bootstrap = bootstrap.clone();
                let store = store.clone();
                let sync_service = sync_service.clone();
                let secrets = secrets.clone();
                cx.new(|cx| WorkspaceView::new(bootstrap, store, sync_service, secrets, cx))
            },
        ) {
            panic!("failed to open the Phase 4 window: {error:#}");
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListMode {
    Mail,
    Search,
    Drafts,
    Outbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailMode {
    Reader,
    Compose,
    AccountSetup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorField {
    SearchQuery,
    ComposeTo,
    ComposeCc,
    ComposeBcc,
    ComposeSubject,
    ComposeBody,
    ComposeAttachmentPath,
    AccountDisplayName,
    AccountEndpoint,
    AccountUsername,
    AccountPassword,
    AccountIdentityName,
    AccountIdentityEmail,
    AccountSignature,
    AccountAliasEmails,
    AccountSmtpHost,
    AccountSmtpPort,
    AccountSmtpSecurity,
    AccountSmtpUsername,
}

#[derive(Debug, Clone)]
struct UiBuffer {
    text: String,
    cursor: usize,
    multiline: bool,
    secret: bool,
}

impl UiBuffer {
    fn single_line(value: impl Into<String>) -> Self {
        let text = value.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            multiline: false,
            secret: false,
        }
    }

    fn multiline(value: impl Into<String>) -> Self {
        let text = value.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            multiline: true,
            secret: false,
        }
    }

    fn secret(value: impl Into<String>) -> Self {
        let text = value.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            multiline: false,
            secret: true,
        }
    }

    fn set(&mut self, value: impl Into<String>) {
        self.text = value.into();
        self.cursor = self.text.len();
    }

    fn visible_text(&self) -> String {
        if self.secret {
            "*".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    fn display_text(&self, active: bool) -> String {
        let mut visible = self.visible_text();
        if !active {
            if visible.is_empty() {
                return if self.multiline {
                    "Click to edit...".to_owned()
                } else {
                    "(empty)".to_owned()
                };
            }
            return visible;
        }

        let cursor = self.cursor.min(if self.secret {
            visible.len()
        } else {
            self.text.len()
        });
        visible.insert(cursor, '|');
        visible
    }

    fn insert_text(&mut self, new_text: &str) {
        self.text.insert_str(self.cursor, new_text);
        self.cursor += new_text.len();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, pasted: Option<&str>) -> InputOutcome {
        if event.is_held {
            return InputOutcome::Ignored;
        }

        if event.keystroke.modifiers.platform && event.keystroke.key.eq_ignore_ascii_case("v") {
            if let Some(pasted) = pasted {
                let pasted = if self.multiline {
                    pasted.to_owned()
                } else {
                    pasted.replace('\n', " ")
                };
                self.insert_text(&pasted);
                return InputOutcome::Changed;
            }
            return InputOutcome::Ignored;
        }

        match event.keystroke.key.as_str() {
            "backspace" => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            "delete" => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            "left" => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            "right" => {
                if self.cursor < self.text.len() {
                    self.cursor += 1;
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            "home" => {
                self.cursor = 0;
                InputOutcome::Changed
            }
            "end" => {
                self.cursor = self.text.len();
                InputOutcome::Changed
            }
            "enter" => {
                if self.multiline {
                    self.insert_text("\n");
                    InputOutcome::Changed
                } else {
                    InputOutcome::Submitted
                }
            }
            _ => {
                if event.keystroke.modifiers.control || event.keystroke.modifiers.platform {
                    return InputOutcome::Ignored;
                }
                if let Some(text) = event
                    .keystroke
                    .key_char
                    .as_deref()
                    .filter(|text| !text.is_empty())
                {
                    self.insert_text(text);
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputOutcome {
    Changed,
    Submitted,
    Ignored,
}

#[derive(Debug, Clone)]
struct ComposeState {
    draft_id: i64,
    account_id: i64,
    identity_id: i64,
    identity_email: String,
    to: UiBuffer,
    cc: UiBuffer,
    bcc: UiBuffer,
    subject: UiBuffer,
    body: UiBuffer,
    attachment_path: UiBuffer,
    attachments: Vec<DraftAttachment>,
    state_label: String,
    last_error_message: Option<String>,
}

impl ComposeState {
    fn from_draft(draft: DraftDetail) -> Self {
        Self {
            draft_id: draft.id,
            account_id: draft.account_id,
            identity_id: draft.identity_id,
            identity_email: draft.identity_email,
            to: UiBuffer::single_line(draft.to.join(", ")),
            cc: UiBuffer::single_line(draft.cc.join(", ")),
            bcc: UiBuffer::single_line(draft.bcc.join(", ")),
            subject: UiBuffer::single_line(draft.subject),
            body: UiBuffer::multiline(draft.body),
            attachment_path: UiBuffer::single_line(String::new()),
            attachments: draft.attachments,
            state_label: draft.state.to_string(),
            last_error_message: draft.last_error_message,
        }
    }
}

#[derive(Debug, Clone)]
struct AccountSetupState {
    protocol: ProtocolKind,
    display_name: UiBuffer,
    endpoint: UiBuffer,
    username: UiBuffer,
    password: UiBuffer,
    identity_name: UiBuffer,
    identity_email: UiBuffer,
    signature: UiBuffer,
    alias_emails: UiBuffer,
    smtp_host: UiBuffer,
    smtp_port: UiBuffer,
    smtp_security: UiBuffer,
    smtp_username: UiBuffer,
}

impl Default for AccountSetupState {
    fn default() -> Self {
        Self {
            protocol: ProtocolKind::Imap,
            display_name: UiBuffer::single_line(String::new()),
            endpoint: UiBuffer::single_line("imaps://mail.example.com:993"),
            username: UiBuffer::single_line(String::new()),
            password: UiBuffer::secret(String::new()),
            identity_name: UiBuffer::single_line(String::new()),
            identity_email: UiBuffer::single_line(String::new()),
            signature: UiBuffer::multiline("Regards,"),
            alias_emails: UiBuffer::single_line(String::new()),
            smtp_host: UiBuffer::single_line("smtp.example.com"),
            smtp_port: UiBuffer::single_line("587"),
            smtp_security: UiBuffer::single_line("starttls"),
            smtp_username: UiBuffer::single_line(String::new()),
        }
    }
}

struct WorkspaceView {
    bootstrap: AppBootstrap,
    store: Arc<LocalStore>,
    sync_service: SyncService,
    secrets: Arc<dyn SecretStore>,
    focus_handle: FocusHandle,
    account: Option<AccountOverview>,
    accounts: Vec<AccountSummary>,
    identities: Vec<IdentitySummary>,
    stats: StoreStats,
    mailboxes: Vec<MailboxSummary>,
    drafts: Vec<DraftSummary>,
    outbox_jobs: Vec<OutboxJobSummary>,
    search_results: Vec<SearchHit>,
    selected_account_id: Option<i64>,
    selected_mailbox_id: Option<i64>,
    selected_thread_id: Option<i64>,
    selected_draft_id: Option<i64>,
    selected_outbox_job_id: Option<i64>,
    mailbox_scroll: UniformListScrollHandle,
    thread_scroll: UniformListScrollHandle,
    list_mode: ListMode,
    detail_mode: DetailMode,
    active_field: Option<EditorField>,
    search_query: UiBuffer,
    compose: Option<ComposeState>,
    account_setup: AccountSetupState,
    store_error: Option<String>,
    status_message: Option<String>,
    refresh_scheduled: bool,
}

impl WorkspaceView {
    fn new(
        bootstrap: AppBootstrap,
        store: Arc<LocalStore>,
        sync_service: SyncService,
        secrets: Arc<dyn SecretStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            bootstrap,
            store,
            sync_service,
            secrets,
            focus_handle: cx.focus_handle(),
            account: None,
            accounts: Vec::new(),
            identities: Vec::new(),
            stats: StoreStats {
                account_count: 0,
                mailbox_count: 0,
                thread_count: 0,
                message_count: 0,
            },
            mailboxes: Vec::new(),
            drafts: Vec::new(),
            outbox_jobs: Vec::new(),
            search_results: Vec::new(),
            selected_account_id: None,
            selected_mailbox_id: None,
            selected_thread_id: None,
            selected_draft_id: None,
            selected_outbox_job_id: None,
            mailbox_scroll: UniformListScrollHandle::new(),
            thread_scroll: UniformListScrollHandle::new(),
            list_mode: ListMode::Mail,
            detail_mode: DetailMode::Reader,
            active_field: None,
            search_query: UiBuffer::single_line(String::new()),
            compose: None,
            account_setup: AccountSetupState::default(),
            store_error: None,
            status_message: None,
            refresh_scheduled: false,
        };
        view.refresh_from_store();
        if view.accounts.is_empty() {
            view.detail_mode = DetailMode::AccountSetup;
            view.active_field = Some(EditorField::AccountDisplayName);
        }
        view
    }

    fn refresh_from_store(&mut self) {
        self.refresh_scheduled = false;
        match self.store.list_accounts() {
            Ok(accounts) => {
                self.accounts = accounts;
                self.store_error = None;
            }
            Err(error) => {
                self.accounts.clear();
                self.store_error = Some(error.to_string());
            }
        }

        self.stats = self.store.stats().unwrap_or(StoreStats {
            account_count: 0,
            mailbox_count: 0,
            thread_count: 0,
            message_count: 0,
        });

        self.selected_account_id = self
            .selected_account_id
            .filter(|selected_id| {
                self.accounts
                    .iter()
                    .any(|account| account.id == *selected_id)
            })
            .or_else(|| self.accounts.first().map(|account| account.id));

        self.account = self
            .selected_account_id
            .and_then(|account_id| self.store.account_overview(account_id).ok().flatten());
        self.identities = self
            .selected_account_id
            .and_then(|account_id| self.store.list_identities(account_id).ok())
            .unwrap_or_default();
        self.mailboxes = self
            .selected_account_id
            .and_then(|account_id| self.store.list_mailboxes(account_id).ok())
            .unwrap_or_default();
        self.drafts = self
            .store
            .list_drafts(self.selected_account_id)
            .unwrap_or_default();
        self.outbox_jobs = self
            .store
            .list_outbox_jobs(self.selected_account_id)
            .unwrap_or_default();

        self.selected_mailbox_id = self
            .selected_mailbox_id
            .filter(|selected_id| {
                self.mailboxes
                    .iter()
                    .any(|mailbox| mailbox.id == *selected_id)
            })
            .or_else(|| self.mailboxes.first().map(|mailbox| mailbox.id));
        self.selected_thread_id = self.selected_thread_id.filter(|selected_id| {
            self.selected_mailbox_id
                .and_then(|mailbox_id| self.store.list_threads(mailbox_id, 0..200).ok())
                .map(|threads| threads.into_iter().any(|thread| thread.id == *selected_id))
                .unwrap_or(false)
        });
        if self.selected_thread_id.is_none() {
            self.selected_thread_id = self
                .selected_mailbox_id
                .and_then(|mailbox_id| self.store.first_thread_id(mailbox_id).ok().flatten());
        }

        self.selected_draft_id = self
            .selected_draft_id
            .filter(|selected_id| self.drafts.iter().any(|draft| draft.id == *selected_id))
            .or_else(|| self.drafts.first().map(|draft| draft.id));
        self.selected_outbox_job_id = self
            .selected_outbox_job_id
            .filter(|selected_id| self.outbox_jobs.iter().any(|job| job.id == *selected_id))
            .or_else(|| self.outbox_jobs.first().map(|job| job.id));

        if let Some(compose) = self.compose.as_ref()
            && let Ok(Some(draft)) = self.store.draft_detail(compose.draft_id)
        {
            self.compose = Some(ComposeState::from_draft(draft));
        }
    }

    fn schedule_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refresh_scheduled {
            return;
        }
        self.refresh_scheduled = true;
        let view = cx.entity();
        window
            .spawn(cx, async move |cx| {
                Timer::after(Duration::from_millis(900)).await;
                let _ = view.update(cx, |this, cx| {
                    this.refresh_from_store();
                    cx.notify();
                });
            })
            .detach();
    }

    fn selected_account(&self) -> Option<&AccountSummary> {
        self.selected_account_id.and_then(|account_id| {
            self.accounts
                .iter()
                .find(|account| account.id == account_id)
        })
    }

    fn selected_mailbox(&self) -> Option<&MailboxSummary> {
        self.selected_mailbox_id.and_then(|mailbox_id| {
            self.mailboxes
                .iter()
                .find(|mailbox| mailbox.id == mailbox_id)
        })
    }

    fn selected_thread_view(&mut self) -> Option<ThreadView> {
        let thread_id = self.selected_thread_id?;

        match self.store.thread_view(thread_id) {
            Ok(thread) => {
                self.store_error = None;
                thread
            }
            Err(error) => {
                self.store_error = Some(error.to_string());
                None
            }
        }
    }

    fn select_account(&mut self, account_id: i64, cx: &mut Context<Self>) {
        self.selected_account_id = Some(account_id);
        self.list_mode = ListMode::Mail;
        self.detail_mode = if matches!(self.detail_mode, DetailMode::AccountSetup) {
            DetailMode::Reader
        } else {
            self.detail_mode
        };
        self.refresh_from_store();
        cx.notify();
    }

    fn select_mailbox(&mut self, mailbox_id: i64, cx: &mut Context<Self>) {
        self.selected_mailbox_id = Some(mailbox_id);
        self.selected_thread_id = match self.store.first_thread_id(mailbox_id) {
            Ok(thread_id) => {
                self.store_error = None;
                thread_id
            }
            Err(error) => {
                self.store_error = Some(error.to_string());
                None
            }
        };
        self.list_mode = ListMode::Mail;
        self.detail_mode = DetailMode::Reader;
        cx.notify();
    }

    fn select_thread(&mut self, thread_id: i64, mailbox_id: i64, cx: &mut Context<Self>) {
        self.selected_mailbox_id = Some(mailbox_id);
        self.selected_thread_id = Some(thread_id);
        self.detail_mode = DetailMode::Reader;
        cx.notify();
    }

    fn sync_selected_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.selected_account_id else {
            self.store_error = Some("Add an account first.".to_owned());
            cx.notify();
            return;
        };

        match self.sync_service.sync_account(account_id) {
            Ok(_) => {
                self.status_message = Some(format!("Sync started for account {account_id}."));
                self.refresh_from_store();
                self.schedule_refresh(window, cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn sync_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.sync_service.sync_all() {
            Ok(started) => {
                self.status_message = Some(format!("Started {started} account sync job(s)."));
                self.refresh_from_store();
                self.schedule_refresh(window, cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(account_id) = self.selected_account_id else {
            self.store_error = Some("Select or add an account before searching.".to_owned());
            cx.notify();
            return;
        };
        let query = self.search_query.text.trim();
        if query.is_empty() {
            self.search_results.clear();
            self.list_mode = ListMode::Mail;
            cx.notify();
            return;
        }

        match self.store.search_threads(account_id, query, 100) {
            Ok(results) => {
                self.search_results = results;
                self.list_mode = ListMode::Search;
                self.detail_mode = DetailMode::Reader;
                self.store_error = None;
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn open_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.selected_account_id else {
            self.open_account_setup(window, cx);
            return;
        };
        let Some(identity) = self
            .identities
            .iter()
            .find(|identity| identity.is_primary)
            .or_else(|| self.identities.first())
            .cloned()
        else {
            self.store_error = Some(format!("Account {account_id} has no identities."));
            cx.notify();
            return;
        };

        match self.store.create_draft(&NewDraft {
            account_id,
            identity_id: identity.id,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body: String::new(),
        }) {
            Ok(draft_id) => {
                self.selected_draft_id = Some(draft_id);
                self.load_draft_into_compose(draft_id);
                self.list_mode = ListMode::Drafts;
                self.detail_mode = DetailMode::Compose;
                self.active_field = Some(EditorField::ComposeTo);
                self.refresh_from_store();
                window.focus(&self.focus_handle);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn load_draft_into_compose(&mut self, draft_id: i64) {
        match self.store.draft_detail(draft_id) {
            Ok(Some(draft)) => {
                self.compose = Some(ComposeState::from_draft(draft));
                self.selected_draft_id = Some(draft_id);
                self.detail_mode = DetailMode::Compose;
                self.store_error = None;
            }
            Ok(None) => self.store_error = Some(format!("Draft {draft_id} no longer exists.")),
            Err(error) => self.store_error = Some(error.to_string()),
        }
    }

    fn save_compose(&mut self) -> Result<()> {
        let Some(compose) = self.compose.as_ref() else {
            return Ok(());
        };
        self.store.update_draft(
            compose.draft_id,
            &DraftUpdate {
                identity_id: compose.identity_id,
                to: split_addresses(&compose.to.text),
                cc: split_addresses(&compose.cc.text),
                bcc: split_addresses(&compose.bcc.text),
                subject: compose.subject.text.clone(),
                body: compose.body.text.clone(),
            },
        )?;
        Ok(())
    }

    fn send_current_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.save_compose() {
            self.store_error = Some(error.to_string());
            cx.notify();
            return;
        }

        let Some(draft_id) = self.compose.as_ref().map(|compose| compose.draft_id) else {
            self.store_error = Some("No draft is open.".to_owned());
            cx.notify();
            return;
        };

        match self.sync_service.queue_draft_and_send(draft_id) {
            Ok(job_id) => {
                self.status_message = Some(format!("Draft {draft_id} queued as job {job_id}."));
                let _ = self.sync_service.process_outbox(Some(
                    self.compose
                        .as_ref()
                        .map(|compose| compose.account_id)
                        .unwrap_or_default(),
                ));
                self.refresh_from_store();
                self.schedule_refresh(window, cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn add_current_attachment(&mut self, cx: &mut Context<Self>) {
        let Some((draft_id, path)) = self.compose.as_ref().map(|compose| {
            (
                compose.draft_id,
                compose.attachment_path.text.trim().to_owned(),
            )
        }) else {
            self.store_error = Some("No draft is open.".to_owned());
            cx.notify();
            return;
        };
        if let Err(error) = self.save_compose() {
            self.store_error = Some(error.to_string());
            cx.notify();
            return;
        }
        if path.is_empty() {
            self.store_error = Some("Enter an attachment path first.".to_owned());
            cx.notify();
            return;
        }

        match self.store.add_draft_attachment(draft_id, &path) {
            Ok(_) => {
                if let Some(compose) = self.compose.as_mut() {
                    compose.attachment_path.set(String::new());
                }
                self.load_draft_into_compose(draft_id);
                self.refresh_from_store();
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn remove_current_attachment(&mut self, attachment_id: i64, cx: &mut Context<Self>) {
        match self.store.remove_draft_attachment(attachment_id) {
            Ok(()) => {
                if let Some(draft_id) = self.compose.as_ref().map(|compose| compose.draft_id) {
                    self.load_draft_into_compose(draft_id);
                }
                self.refresh_from_store();
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn set_compose_identity(&mut self, identity_id: i64, cx: &mut Context<Self>) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        compose.identity_id = identity_id;
        if let Some(identity) = self
            .identities
            .iter()
            .find(|identity| identity.id == identity_id)
        {
            compose.identity_email = identity.email_address.clone();
        }
        if let Err(error) = self.save_compose() {
            self.store_error = Some(error.to_string());
        } else {
            self.refresh_from_store();
        }
        cx.notify();
    }

    fn retry_selected_outbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job_id) = self.selected_outbox_job_id else {
            self.store_error = Some("Select an outbox job first.".to_owned());
            cx.notify();
            return;
        };

        match self.store.retry_outbox_job(job_id) {
            Ok(()) => {
                let _ = self.sync_service.process_outbox(self.selected_account_id);
                self.status_message = Some(format!("Outbox job {job_id} requeued."));
                self.refresh_from_store();
                self.schedule_refresh(window, cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn open_account_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detail_mode = DetailMode::AccountSetup;
        self.active_field = Some(EditorField::AccountDisplayName);
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn submit_account_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.build_account_from_form() {
            Ok((account, secret)) => match self.store.insert_account(&account) {
                Ok(account_id) => {
                    if let Err(error) = self.secrets.set_account_secret(account_id, &secret) {
                        self.store_error = Some(error.to_string());
                        cx.notify();
                        return;
                    }
                    self.selected_account_id = Some(account_id);
                    self.detail_mode = DetailMode::Reader;
                    self.list_mode = ListMode::Mail;
                    self.account_setup = AccountSetupState::default();
                    self.active_field = None;
                    self.status_message = Some(format!("Account {account_id} added."));
                    let _ = self.sync_service.sync_account(account_id);
                    self.refresh_from_store();
                    self.schedule_refresh(window, cx);
                }
                Err(error) => self.store_error = Some(error.to_string()),
            },
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn build_account_from_form(&self) -> Result<(NewAccount, String)> {
        let form = &self.account_setup;
        let display_name = required_text("Display name", &form.display_name.text)?;
        let endpoint = required_text("Endpoint", &form.endpoint.text)?;
        validate_endpoint(form.protocol, &endpoint)?;
        let username = required_text("Username", &form.username.text)?;
        let secret = required_text("Password", &form.password.text)?;
        let identity_name = if form.identity_name.text.trim().is_empty() {
            display_name.clone()
        } else {
            form.identity_name.text.trim().to_owned()
        };
        let identity_email = if form.identity_email.text.trim().is_empty() {
            username.clone()
        } else {
            form.identity_email.text.trim().to_owned()
        };
        let mut identities = vec![NewIdentity {
            display_name: identity_name.clone(),
            email_address: identity_email,
            signature: form.signature.text.clone(),
            is_primary: true,
        }];
        for (index, alias) in split_addresses(&form.alias_emails.text)
            .into_iter()
            .enumerate()
        {
            identities.push(NewIdentity {
                display_name: format!("{identity_name} Alias {}", index + 1),
                email_address: alias,
                signature: String::new(),
                is_primary: false,
            });
        }

        let outgoing = match form.protocol {
            ProtocolKind::Imap => {
                let mut outgoing = OutgoingConfig::smtp_default(derive_host(&endpoint));
                outgoing.host = Some(required_text("SMTP host", &form.smtp_host.text)?);
                outgoing.port = Some(
                    required_text("SMTP port", &form.smtp_port.text)?
                        .parse::<u16>()
                        .context("SMTP port must be an integer")?,
                );
                outgoing.security = Some(parse_submission_security(&form.smtp_security.text)?);
                outgoing.username = Some(if form.smtp_username.text.trim().is_empty() {
                    username.clone()
                } else {
                    form.smtp_username.text.trim().to_owned()
                });
                outgoing
            }
            ProtocolKind::Jmap => OutgoingConfig::jmap_default(),
        };

        Ok((
            NewAccount {
                display_name,
                protocol: form.protocol,
                endpoint,
                username,
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: Some(outgoing),
                identities,
            },
            secret,
        ))
    }

    fn set_active_field(
        &mut self,
        field: EditorField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_field = Some(field);
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn active_buffer_mut(&mut self) -> Option<&mut UiBuffer> {
        match self.active_field? {
            EditorField::SearchQuery => Some(&mut self.search_query),
            EditorField::ComposeTo => self.compose.as_mut().map(|compose| &mut compose.to),
            EditorField::ComposeCc => self.compose.as_mut().map(|compose| &mut compose.cc),
            EditorField::ComposeBcc => self.compose.as_mut().map(|compose| &mut compose.bcc),
            EditorField::ComposeSubject => {
                self.compose.as_mut().map(|compose| &mut compose.subject)
            }
            EditorField::ComposeBody => self.compose.as_mut().map(|compose| &mut compose.body),
            EditorField::ComposeAttachmentPath => self
                .compose
                .as_mut()
                .map(|compose| &mut compose.attachment_path),
            EditorField::AccountDisplayName => Some(&mut self.account_setup.display_name),
            EditorField::AccountEndpoint => Some(&mut self.account_setup.endpoint),
            EditorField::AccountUsername => Some(&mut self.account_setup.username),
            EditorField::AccountPassword => Some(&mut self.account_setup.password),
            EditorField::AccountIdentityName => Some(&mut self.account_setup.identity_name),
            EditorField::AccountIdentityEmail => Some(&mut self.account_setup.identity_email),
            EditorField::AccountSignature => Some(&mut self.account_setup.signature),
            EditorField::AccountAliasEmails => Some(&mut self.account_setup.alias_emails),
            EditorField::AccountSmtpHost => Some(&mut self.account_setup.smtp_host),
            EditorField::AccountSmtpPort => Some(&mut self.account_setup.smtp_port),
            EditorField::AccountSmtpSecurity => Some(&mut self.account_setup.smtp_security),
            EditorField::AccountSmtpUsername => Some(&mut self.account_setup.smtp_username),
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pasted = cx.read_from_clipboard().and_then(|item| item.text());
        let Some(buffer) = self.active_buffer_mut() else {
            return;
        };
        let outcome = buffer.handle_key(event, pasted.as_deref());
        if matches!(outcome, InputOutcome::Ignored) {
            return;
        }

        let active_field = self.active_field.expect("active field is set");
        match outcome {
            InputOutcome::Changed => {
                if matches!(
                    active_field,
                    EditorField::ComposeTo
                        | EditorField::ComposeCc
                        | EditorField::ComposeBcc
                        | EditorField::ComposeSubject
                        | EditorField::ComposeBody
                ) {
                    if let Err(error) = self.save_compose() {
                        self.store_error = Some(error.to_string());
                    } else {
                        self.refresh_from_store();
                    }
                }
                cx.notify();
            }
            InputOutcome::Submitted => match active_field {
                EditorField::SearchQuery => self.run_search(cx),
                EditorField::ComposeAttachmentPath => self.add_current_attachment(cx),
                EditorField::AccountPassword
                | EditorField::AccountSignature
                | EditorField::AccountAliasEmails
                | EditorField::AccountSmtpUsername => self.submit_account_setup(window, cx),
                _ => cx.notify(),
            },
            InputOutcome::Ignored => {}
        }
    }

    fn set_list_mode(&mut self, list_mode: ListMode, cx: &mut Context<Self>) {
        self.list_mode = list_mode;
        if matches!(list_mode, ListMode::Mail) {
            self.detail_mode = DetailMode::Reader;
        }
        cx.notify();
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = hex_to_rgb(self.bootstrap.theme.background.as_deref(), 0x111827);
        let surface = hex_to_rgb(self.bootstrap.theme.surface.as_deref(), 0x162033);
        let border = hex_to_rgb(self.bootstrap.theme.border.as_deref(), 0x2D3B55);
        let foreground = hex_to_rgb(self.bootstrap.theme.foreground.as_deref(), 0xE5EEF9);
        let accent = hex_to_rgb(self.bootstrap.theme.accent.as_deref(), 0x5BA3F5);
        let muted = 0x9FB0C7;
        let selected_background = 0x203559;
        let warning_background = 0x253550;
        let button_background = 0x1C2E4B;
        let success_background = 0x162B20;

        let selected_account = self.selected_account().cloned();
        let selected_mailbox = self.selected_mailbox().cloned();
        let selected_thread = self.selected_thread_view();
        let thread_count = selected_mailbox
            .as_ref()
            .map(|mailbox| mailbox.total_threads)
            .unwrap_or_default();

        let account_name = selected_account
            .as_ref()
            .map(|account| account.display_name.clone())
            .unwrap_or_else(|| "No account selected".to_owned());
        let account_detail = selected_account
            .as_ref()
            .map(|account| {
                format!(
                    "{} • {} identities • {}",
                    account.protocol, account.identity_count, account.cache_policy.name
                )
            })
            .unwrap_or_else(|| "Add an IMAP or JMAP account to begin.".to_owned());
        let header_detail = format!(
            "{} accounts • {} mailboxes • {} threads • {} messages",
            self.stats.account_count,
            self.stats.mailbox_count,
            self.stats.thread_count,
            self.stats.message_count
        );
        let upstream_detail = format!(
            "GPUI {} • Zed {} • {} Tokio workers",
            self.bootstrap.upstream.gpui_version,
            short_sha(&self.bootstrap.upstream.zed_commit),
            self.bootstrap.runtime.worker_threads
        );
        let thread_header = selected_mailbox
            .as_ref()
            .map(|mailbox| {
                format!(
                    "{} • {} threads • {} unread",
                    mailbox.name, mailbox.total_threads, mailbox.unread_threads
                )
            })
            .unwrap_or_else(|| "No mailbox selected".to_owned());
        let list_title = match self.list_mode {
            ListMode::Mail => "Threads",
            ListMode::Search => "Search Results",
            ListMode::Drafts => "Drafts",
            ListMode::Outbox => "Outbox",
        };

        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .bg(rgb(background))
            .text_color(rgb(foreground))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(rgb(border))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(format!("{APP_NAME} {PHASE_LABEL}"))
                                    .child(div().text_sm().text_color(rgb(muted)).child(account_name))
                                    .child(div().text_sm().text_color(rgb(muted)).child(account_detail)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_end()
                                    .gap_1()
                                    .child(div().text_sm().child(header_detail))
                                    .child(div().text_sm().text_color(rgb(muted)).child(upstream_detail)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(rgb(border))
                            .child(toolbar_button(
                                "Add Account",
                                button_background,
                                border,
                                cx.listener(|this: &mut Self, _, window, cx| {
                                    this.open_account_setup(window, cx);
                                }),
                            ))
                            .child(toolbar_button(
                                "Compose",
                                button_background,
                                border,
                                cx.listener(|this: &mut Self, _, window, cx| {
                                    this.open_compose(window, cx);
                                }),
                            ))
                            .child(toolbar_button(
                                "Mail",
                                if matches!(self.list_mode, ListMode::Mail) {
                                    selected_background
                                } else {
                                    button_background
                                },
                                border,
                                cx.listener(|this: &mut Self, _, _, cx| {
                                    this.set_list_mode(ListMode::Mail, cx);
                                }),
                            ))
                            .child(toolbar_button(
                                "Drafts",
                                if matches!(self.list_mode, ListMode::Drafts) {
                                    selected_background
                                } else {
                                    button_background
                                },
                                border,
                                cx.listener(|this: &mut Self, _, _, cx| {
                                    this.set_list_mode(ListMode::Drafts, cx);
                                    this.detail_mode = DetailMode::Compose;
                                }),
                            ))
                            .child(toolbar_button(
                                "Outbox",
                                if matches!(self.list_mode, ListMode::Outbox) {
                                    selected_background
                                } else {
                                    button_background
                                },
                                border,
                                cx.listener(|this: &mut Self, _, _, cx| {
                                    this.set_list_mode(ListMode::Outbox, cx);
                                }),
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .child(
                                        input_card(
                                            "search-query-field",
                                            "Search",
                                            &self.search_query,
                                            self.active_field == Some(EditorField::SearchQuery),
                                            false,
                                            surface,
                                            border,
                                            accent,
                                        )
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.set_active_field(EditorField::SearchQuery, window, cx);
                                        })),
                                    ),
                            )
                            .child(toolbar_button(
                                "Run Search",
                                button_background,
                                border,
                                cx.listener(|this: &mut Self, _, _, cx| {
                                    this.run_search(cx);
                                }),
                            ))
                            .child(toolbar_button(
                                "Sync Selected",
                                button_background,
                                border,
                                cx.listener(|this: &mut Self, _, window, cx| {
                                    this.sync_selected_account(window, cx);
                                }),
                            ))
                            .child(toolbar_button(
                                "Sync All",
                                button_background,
                                border,
                                cx.listener(|this: &mut Self, _, window, cx| {
                                    this.sync_all(window, cx);
                                }),
                            ))
                            .child(toolbar_button(
                                "Refresh",
                                button_background,
                                border,
                                cx.listener(|this: &mut Self, _, _, cx| {
                                    this.refresh_from_store();
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .overflow_hidden()
                            .child(
                                div()
                                    .w(px(340.0))
                                    .flex()
                                    .flex_col()
                                    .bg(rgb(surface))
                                    .border_r_1()
                                    .border_color(rgb(border))
                                    .child(
                                        div()
                                            .px_3()
                                            .py_3()
                                            .border_b_1()
                                            .border_color(rgb(border))
                                            .child("Accounts")
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(muted))
                                                    .child("Account setup, mailbox selection, and manual sync live here."),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .border_b_1()
                                            .border_color(rgb(border))
                                            .children(self.accounts.iter().map(|account| {
                                                let account_id = account.id;
                                                let is_selected =
                                                    Some(account_id) == self.selected_account_id;
                                                let background_color = if is_selected {
                                                    rgb(selected_background)
                                                } else {
                                                    rgb(surface)
                                                };
                                                let (badge_color, badge_text) =
                                                    sync_badge(account.sync_status.phase, accent);
                                                div()
                                                    .id(("account", account_id as u64))
                                                    .px_3()
                                                    .py_3()
                                                    .cursor_pointer()
                                                    .border_b_1()
                                                    .border_color(rgb(border))
                                                    .bg(background_color)
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.select_account(account_id, cx);
                                                    }))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .justify_between()
                                                            .items_start()
                                                            .gap_2()
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .flex_col()
                                                                    .gap_1()
                                                                    .child(account.display_name.clone())
                                                                    .child(
                                                                        div()
                                                                            .text_sm()
                                                                            .text_color(rgb(muted))
                                                                            .child(format!(
                                                                                "{} • {} identities",
                                                                                account.protocol,
                                                                                account.identity_count
                                                                            )),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_sm()
                                                                            .text_color(rgb(muted))
                                                                            .truncate()
                                                                            .child(account.endpoint.clone()),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_sm()
                                                                            .truncate()
                                                                            .child(account.sync_status.detail.clone()),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .px_2()
                                                                    .py_1()
                                                                    .bg(rgb(button_background))
                                                                    .text_sm()
                                                                    .text_color(rgb(badge_color))
                                                                    .child(badge_text),
                                                            ),
                                                    )
                                            })),
                                    )
                                    .child(
                                        div()
                                            .px_3()
                                            .py_3()
                                            .border_b_1()
                                            .border_color(rgb(border))
                                            .child("Mailboxes")
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(muted))
                                                    .child("Local mirror for the selected account."),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .overflow_hidden()
                                            .child(
                                                uniform_list(
                                                    "mailboxes",
                                                    self.mailboxes.len(),
                                                    cx.processor(
                                                        move |this, range: Range<usize>, _window, cx| {
                                                            let mut items = Vec::new();
                                                            for index in range {
                                                                let Some(mailbox) =
                                                                    this.mailboxes.get(index).cloned()
                                                                else {
                                                                    continue;
                                                                };
                                                                let is_selected =
                                                                    Some(mailbox.id)
                                                                        == this.selected_mailbox_id;
                                                                let background_color = if is_selected {
                                                                    rgb(selected_background)
                                                                } else {
                                                                    rgb(surface)
                                                                };

                                                                items.push(
                                                                    div()
                                                                        .id(index)
                                                                        .w_full()
                                                                        .h(px(48.0))
                                                                        .px_3()
                                                                        .py_2()
                                                                        .cursor_pointer()
                                                                        .border_b_1()
                                                                        .border_color(rgb(border))
                                                                        .bg(background_color)
                                                                        .on_click(cx.listener(
                                                                            move |this, _, _, cx| {
                                                                                this.select_mailbox(
                                                                                    mailbox.id,
                                                                                    cx,
                                                                                );
                                                                            },
                                                                        ))
                                                                        .child(
                                                                            div()
                                                                                .flex()
                                                                                .justify_between()
                                                                                .items_center()
                                                                                .w_full()
                                                                                .child(
                                                                                    div()
                                                                                        .flex()
                                                                                        .flex_col()
                                                                                        .child(mailbox.name.clone())
                                                                                        .child(
                                                                                            div()
                                                                                                .text_sm()
                                                                                                .text_color(rgb(muted))
                                                                                                .child(format!(
                                                                                                    "{} threads",
                                                                                                    mailbox.total_threads
                                                                                                )),
                                                                                        ),
                                                                                )
                                                                                .child(
                                                                                    div()
                                                                                        .text_sm()
                                                                                        .text_color(rgb(accent))
                                                                                        .child(format!(
                                                                                            "{}",
                                                                                            mailbox.unread_threads
                                                                                        )),
                                                                                ),
                                                                        ),
                                                                );
                                                            }
                                                            items
                                                        },
                                                    ),
                                                )
                                                .track_scroll(self.mailbox_scroll.clone())
                                                .h_full(),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(430.0))
                                    .flex()
                                    .flex_col()
                                    .bg(rgb(surface))
                                    .border_r_1()
                                    .border_color(rgb(border))
                                    .child(
                                        div()
                                            .px_3()
                                            .py_3()
                                            .border_b_1()
                                            .border_color(rgb(border))
                                            .child(list_title)
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(muted))
                                                    .child(match self.list_mode {
                                                        ListMode::Mail => thread_header,
                                                        ListMode::Search => format!(
                                                            "{} result(s) for {:?}",
                                                            self.search_results.len(),
                                                            self.search_query.text
                                                        ),
                                                        ListMode::Drafts => format!(
                                                            "{} draft(s) visible",
                                                            self.drafts.len()
                                                        ),
                                                        ListMode::Outbox => format!(
                                                            "{} outbox job(s)",
                                                            self.outbox_jobs.len()
                                                        ),
                                                    }),
                                            ),
                                    )
                                    .child(match self.list_mode {
                                        ListMode::Mail => {
                                            if self.accounts.is_empty() {
                                                empty_state(
                                                    "No accounts yet",
                                                    "Use Add Account to configure IMAP or JMAP directly in the app.",
                                                    muted,
                                                )
                                            } else if thread_count == 0 {
                                                empty_state(
                                                    "No local mail yet",
                                                    "Sync the selected account to populate the local mirror.",
                                                    muted,
                                                )
                                            } else {
                                                div()
                                                    .flex_1()
                                                    .overflow_hidden()
                                                    .child(
                                                        uniform_list(
                                                            "threads",
                                                            thread_count,
                                                            cx.processor(
                                                                move |this,
                                                                      range: Range<usize>,
                                                                      _window,
                                                                      cx| {
                                                                    let Some(mailbox_id) =
                                                                        this.selected_mailbox_id
                                                                    else {
                                                                        return Vec::new();
                                                                    };
                                                                    let threads = match this
                                                                        .store
                                                                        .list_threads(mailbox_id, range)
                                                                    {
                                                                        Ok(threads) => {
                                                                            this.store_error = None;
                                                                            threads
                                                                        }
                                                                        Err(error) => {
                                                                            this.store_error =
                                                                                Some(error.to_string());
                                                                            Vec::new()
                                                                        }
                                                                    };

                                                                    threads
                                                                        .into_iter()
                                                                        .map(|thread| {
                                                                            let is_selected =
                                                                                Some(thread.id)
                                                                                    == this.selected_thread_id;
                                                                            let background_color =
                                                                                if is_selected {
                                                                                    rgb(selected_background)
                                                                                } else {
                                                                                    rgb(surface)
                                                                                };
                                                                            let thread_id = thread.id;
                                                                            div()
                                                                                .id(("thread", thread_id as u64))
                                                                                .w_full()
                                                                                .h(px(82.0))
                                                                                .px_3()
                                                                                .py_2()
                                                                                .cursor_pointer()
                                                                                .border_b_1()
                                                                                .border_color(rgb(border))
                                                                                .bg(background_color)
                                                                                .on_click(cx.listener(
                                                                                    move |this, _, _, cx| {
                                                                                        this.select_thread(
                                                                                            thread_id,
                                                                                            mailbox_id,
                                                                                            cx,
                                                                                        );
                                                                                    },
                                                                                ))
                                                                                .child(
                                                                                    list_row(
                                                                                        &thread.subject,
                                                                                        &thread.latest_message_at,
                                                                                        &thread.participants,
                                                                                        &thread.preview,
                                                                                        &format!(
                                                                                            "{} msgs • {} unread",
                                                                                            thread.message_count,
                                                                                            thread.unread_count
                                                                                        ),
                                                                                        muted,
                                                                                        accent,
                                                                                    ),
                                                                                )
                                                                        })
                                                                        .collect()
                                                                },
                                                            ),
                                                        )
                                                        .track_scroll(self.thread_scroll.clone())
                                                        .h_full(),
                                                    )
                                                    .into_any_element()
                                            }
                                        }
                                        ListMode::Search => {
                                            if self.search_results.is_empty() {
                                                empty_state(
                                                    "No search results",
                                                    "Type a query above and run search against the local index.",
                                                    muted,
                                                )
                                            } else {
                                                div()
                                                    .id("search-results-list")
                                                    .flex_1()
                                                    .overflow_y_scroll()
                                                    .children(self.search_results.iter().map(|hit| {
                                                        let mailbox_id = hit.mailbox_id;
                                                        let thread_id = hit.thread_id;
                                                        let is_selected =
                                                            Some(thread_id) == self.selected_thread_id;
                                                        let background_color = if is_selected {
                                                            rgb(selected_background)
                                                        } else {
                                                            rgb(surface)
                                                        };
                                                        div()
                                                            .id(("search", thread_id as u64))
                                                            .px_3()
                                                            .py_3()
                                                            .cursor_pointer()
                                                            .border_b_1()
                                                            .border_color(rgb(border))
                                                            .bg(background_color)
                                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                                this.select_thread(thread_id, mailbox_id, cx);
                                                            }))
                                                            .child(
                                                                list_row(
                                                                    &hit.subject,
                                                                    &hit.latest_message_at,
                                                                    &format!("{} • {}", hit.mailbox_name, hit.participants),
                                                                    &hit.preview,
                                                                    &format!(
                                                                        "{} unread • flagged={}",
                                                                        hit.unread_count, hit.is_flagged
                                                                    ),
                                                                    muted,
                                                                    accent,
                                                                ),
                                                            )
                                                    }))
                                                    .into_any_element()
                                            }
                                        }
                                        ListMode::Drafts => {
                                            if self.drafts.is_empty() {
                                                empty_state(
                                                    "No drafts",
                                                    "Press Compose to start a new message.",
                                                    muted,
                                                )
                                            } else {
                                                div()
                                                    .id("drafts-list")
                                                    .flex_1()
                                                    .overflow_y_scroll()
                                                    .children(self.drafts.iter().map(|draft| {
                                                        let draft_id = draft.id;
                                                        let is_selected =
                                                            Some(draft_id) == self.selected_draft_id;
                                                        let background_color = if is_selected {
                                                            rgb(selected_background)
                                                        } else {
                                                            rgb(surface)
                                                        };
                                                        div()
                                                            .id(("draft", draft_id as u64))
                                                            .px_3()
                                                            .py_3()
                                                            .cursor_pointer()
                                                            .border_b_1()
                                                            .border_color(rgb(border))
                                                            .bg(background_color)
                                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                                this.load_draft_into_compose(draft_id);
                                                                this.list_mode = ListMode::Drafts;
                                                                this.detail_mode = DetailMode::Compose;
                                                                cx.notify();
                                                            }))
                                                            .child(
                                                                list_row(
                                                                    &draft.subject,
                                                                    &draft.updated_at,
                                                                    &draft.identity_name,
                                                                    &draft.recipient_summary,
                                                                    &format!(
                                                                        "{} • {} attachment(s)",
                                                                        draft.state, draft.attachment_count
                                                                    ),
                                                                    muted,
                                                                    accent,
                                                                ),
                                                            )
                                                    }))
                                                    .into_any_element()
                                            }
                                        }
                                        ListMode::Outbox => {
                                            if self.outbox_jobs.is_empty() {
                                                empty_state(
                                                    "No outbox jobs",
                                                    "Queued and retrying deliveries will appear here.",
                                                    muted,
                                                )
                                            } else {
                                                div()
                                                    .id("outbox-list")
                                                    .flex_1()
                                                    .overflow_y_scroll()
                                                    .children(self.outbox_jobs.iter().map(|job| {
                                                        let job_id = job.id;
                                                        let is_selected = Some(job_id) == self.selected_outbox_job_id;
                                                        let background_color = if is_selected {
                                                            rgb(selected_background)
                                                        } else {
                                                            rgb(surface)
                                                        };
                                                        div()
                                                            .id(("outbox", job_id as u64))
                                                            .px_3()
                                                            .py_3()
                                                            .cursor_pointer()
                                                            .border_b_1()
                                                            .border_color(rgb(border))
                                                            .bg(background_color)
                                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                                this.selected_outbox_job_id = Some(job_id);
                                                                cx.notify();
                                                            }))
                                                            .child(
                                                                list_row(
                                                                    &format!("Draft {}", job.draft_id),
                                                                    job.next_attempt_at.as_deref().unwrap_or("no retry scheduled"),
                                                                    &format!("{} • attempts={}", job.state, job.attempt_count),
                                                                    job.last_error_message.as_deref().unwrap_or("Ready to send or retry."),
                                                                    &format!("Job {}", job.id),
                                                                    muted,
                                                                    accent,
                                                                ),
                                                            )
                                                    }))
                                                    .into_any_element()
                                            }
                                        }
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .bg(rgb(background))
                                    .child(
                                        div()
                                            .px_4()
                                            .py_3()
                                            .border_b_1()
                                            .border_color(rgb(border))
                                            .child(match self.detail_mode {
                                                DetailMode::Reader => "Reader",
                                                DetailMode::Compose => "Compose",
                                                DetailMode::AccountSetup => "Account Setup",
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id("detail-scroll")
                                            .flex_1()
                                            .overflow_y_scroll()
                                            .px_4()
                                            .py_4()
                                            .child(match self.detail_mode {
                                                DetailMode::Reader => render_reader_detail(
                                                    selected_thread.as_ref(),
                                                    selected_account.as_ref(),
                                                    &self.mailboxes,
                                                    &self.identities,
                                                    border,
                                                    surface,
                                                    button_background,
                                                    muted,
                                                    accent,
                                                    cx,
                                                ),
                                                DetailMode::Compose => div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_3()
                                                    .child(render_compose_detail(
                                                        self.compose.as_ref(),
                                                        &self.identities,
                                                        self.active_field,
                                                        surface,
                                                        border,
                                                        accent,
                                                        muted,
                                                        cx,
                                                    ))
                                                    .children(self.compose.as_ref().map(|compose| {
                                                        compose
                                                            .attachments
                                                            .iter()
                                                            .map(|attachment| {
                                                                let attachment_id = attachment.id;
                                                                div()
                                                                    .px_3()
                                                                    .py_2()
                                                                    .bg(rgb(surface))
                                                                    .border_1()
                                                                    .border_color(rgb(border))
                                                                    .child(
                                                                        div()
                                                                            .flex()
                                                                            .justify_between()
                                                                            .items_center()
                                                                            .child(
                                                                                div()
                                                                                    .flex()
                                                                                    .flex_col()
                                                                                    .child(attachment.filename.clone())
                                                                                    .child(
                                                                                        div()
                                                                                            .text_sm()
                                                                                            .text_color(rgb(muted))
                                                                                            .child(format!(
                                                                                                "{} • {} bytes",
                                                                                                attachment.content_type,
                                                                                                attachment.size_bytes
                                                                                            )),
                                                                                    ),
                                                                            )
                                                                            .child(
                                                                                toolbar_button(
                                                                                    "Remove",
                                                                                    button_background,
                                                                                    border,
                                                                                    cx.listener(move |this: &mut Self, _, _, cx| {
                                                                                        this.remove_current_attachment(
                                                                                            attachment_id,
                                                                                            cx,
                                                                                        );
                                                                                    }),
                                                                                ),
                                                                            ),
                                                                    )
                                                            })
                                                            .collect::<Vec<_>>()
                                                    }).unwrap_or_default())
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .gap_2()
                                                            .pt_3()
                                                            .child(toolbar_button(
                                                                "Add Attachment",
                                                                button_background,
                                                                border,
                                                                cx.listener(|this: &mut Self, _, _, cx| {
                                                                    this.add_current_attachment(cx);
                                                                }),
                                                            ))
                                                            .child(toolbar_button(
                                                                "Send Draft",
                                                                button_background,
                                                                border,
                                                                cx.listener(|this: &mut Self, _, window, cx| {
                                                                    this.send_current_draft(window, cx);
                                                                }),
                                                            ))
                                                    )
                                                    .into_any_element(),
                                                DetailMode::AccountSetup => render_account_setup_detail(
                                                    &self.account_setup,
                                                    self.active_field,
                                                    surface,
                                                    border,
                                                    accent,
                                                    muted,
                                                    cx,
                                                )
                                                .child(
                                                    div()
                                                        .flex()
                                                        .gap_2()
                                                        .pt_3()
                                                        .child(toolbar_button(
                                                            "Save Account",
                                                            button_background,
                                                            border,
                                                            cx.listener(|this: &mut Self, _, window, cx| {
                                                                this.submit_account_setup(window, cx);
                                                            }),
                                                        )),
                                                )
                                                .into_any_element(),
                                            }),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap_2()
                                            .px_4()
                                            .py_3()
                                            .border_t_1()
                                            .border_color(rgb(border))
                                            .child(match self.detail_mode {
                                                DetailMode::Reader => div()
                                                    .flex()
                                                    .gap_2()
                                                    .children(reader_action_buttons(
                                                        &selected_thread,
                                                        &self.mailboxes,
                                                        border,
                                                        button_background,
                                                        cx,
                                                    )),
                                                DetailMode::Compose => div()
                                                    .flex()
                                                    .gap_2()
                                                    .children(compose_focus_buttons(border, button_background, cx)),
                                                DetailMode::AccountSetup => div()
                                                    .flex()
                                                    .gap_2()
                                                    .children(account_protocol_buttons(
                                                        self.account_setup.protocol,
                                                        border,
                                                        button_background,
                                                        selected_background,
                                                        cx,
                                                    )),
                                            }),
                                    ),
                            ),
                    )
                    .when_some(self.status_message.clone(), |this, message| {
                        this.child(
                            div()
                                .px_4()
                                .py_3()
                                .bg(rgb(success_background))
                                .border_t_1()
                                .border_color(rgb(border))
                                .child(div().text_sm().child(message)),
                        )
                    })
                    .when_some(self.store_error.clone(), |this, error| {
                        this.child(
                            div()
                                .px_4()
                                .py_3()
                                .bg(rgb(warning_background))
                                .border_t_1()
                                .border_color(rgb(border))
                                .child(div().text_sm().child(format!("Issue: {error}"))),
                        )
                    }),
            )
    }
}

fn toolbar_button(
    label: impl Into<String>,
    background: u32,
    border: u32,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id("toolbar-button")
        .px_2()
        .py_1()
        .bg(rgb(background))
        .border_1()
        .border_color(rgb(border))
        .cursor_pointer()
        .on_click(on_click)
        .child(label)
}

fn empty_state(title: &str, detail: &str, muted: u32) -> AnyElement {
    div()
        .flex_1()
        .items_center()
        .justify_center()
        .px_6()
        .text_center()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(title.to_owned())
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(muted))
                        .child(detail.to_owned()),
                ),
        )
        .into_any_element()
}

fn list_row(
    title: &str,
    trailing: &str,
    subtitle: &str,
    preview: &str,
    meta: &str,
    muted: u32,
    accent: u32,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .w_full()
                .child(div().truncate().child(title.to_owned()))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(muted))
                        .child(trailing.to_owned()),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(muted))
                .truncate()
                .child(subtitle.to_owned()),
        )
        .child(div().text_sm().truncate().child(preview.to_owned()))
        .child(
            div()
                .text_sm()
                .text_color(rgb(accent))
                .child(meta.to_owned()),
        )
}

#[allow(clippy::too_many_arguments)]
fn input_card(
    id: &'static str,
    label: &str,
    buffer: &UiBuffer,
    active: bool,
    multiline: bool,
    surface: u32,
    border: u32,
    accent: u32,
) -> Stateful<gpui::Div> {
    let card_border = if active { accent } else { border };
    let min_height = if multiline { px(132.0) } else { px(52.0) };
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().child(label.to_owned()))
        .child(
            div()
                .min_h(min_height)
                .w_full()
                .px_2()
                .py_2()
                .bg(rgb(surface))
                .border_1()
                .border_color(rgb(card_border))
                .child(buffer.display_text(active)),
        )
}

#[allow(clippy::too_many_arguments)]
fn render_reader_detail(
    selected_thread: Option<&ThreadView>,
    selected_account: Option<&AccountSummary>,
    mailboxes: &[MailboxSummary],
    identities: &[IdentitySummary],
    border: u32,
    surface: u32,
    _button_background: u32,
    muted: u32,
    accent: u32,
    _cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    match selected_thread {
        Some(thread) => {
            let sync_detail = selected_account
                .map(|account| {
                    format!(
                        "{} • {}",
                        account.sync_status.phase, account.sync_status.detail
                    )
                })
                .unwrap_or_else(|| "No sync state".to_owned());
            let move_targets = mailboxes
                .iter()
                .filter(|mailbox| mailbox.id != thread.mailbox_id)
                .map(|mailbox| mailbox.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(thread.subject.clone())
                .child(div().text_sm().text_color(rgb(muted)).child(format!(
                            "{} • {} • {}",
                            thread.participants,
                            thread
                                .messages
                                .last()
                                .map(|message| message.received_at.clone())
                                .unwrap_or_default(),
                            thread.mailbox_name
                        )))
                .child(div().text_sm().text_color(rgb(muted)).child(format!(
                    "{} msgs • {} unread • {}",
                    thread.message_count,
                    thread.unread_count,
                    if thread.has_remote_content {
                        "remote content blocked"
                    } else {
                        "all content local"
                    }
                )))
                .child(div().text_sm().text_color(rgb(muted)).child(format!(
                    "Move targets: {}",
                    if move_targets.is_empty() {
                        "none".to_owned()
                    } else {
                        move_targets
                    }
                )))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child("Conversation")
                        .children(thread.messages.iter().map(|message| {
                            let mode_label = match message.render_mode {
                                MessageRenderMode::PlainText => "Plain text",
                                MessageRenderMode::SanitizedHtml => "Sanitized HTML",
                                MessageRenderMode::PreviewOnly => "Preview only",
                            };
                            let status = if message.has_remote_content {
                                format!("{mode_label} • remote content blocked")
                            } else if message.is_flagged {
                                format!("{mode_label} • flagged")
                            } else if !message.is_read {
                                format!("{mode_label} • unread")
                            } else {
                                mode_label.to_owned()
                            };
                            div()
                                .px_3()
                                .py_3()
                                .bg(rgb(surface))
                                .border_1()
                                .border_color(rgb(border))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex()
                                                .justify_between()
                                                .items_start()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_col()
                                                        .gap_1()
                                                        .child(message.sender.clone())
                                                        .child(
                                                            div()
                                                                .text_sm()
                                                                .text_color(rgb(muted))
                                                                .child(message.recipients.clone()),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(rgb(muted))
                                                        .child(message.received_at.clone()),
                                                ),
                                        )
                                        .child(
                                            div().text_sm().text_color(rgb(accent)).child(status),
                                        )
                                        .child(message.rendered_body.clone()),
                                )
                        })),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child("Sync Status")
                        .child(div().text_sm().text_color(rgb(muted)).child(sync_detail)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child("Identities")
                        .children(identities.iter().map(|identity| {
                            let primary = if identity.is_primary {
                                "primary"
                            } else {
                                "secondary"
                            };
                            div()
                                .px_3()
                                .py_2()
                                .bg(rgb(surface))
                                .border_1()
                                .border_color(rgb(border))
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .items_center()
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(identity.display_name.clone())
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(rgb(muted))
                                                        .child(identity.email_address.clone()),
                                                ),
                                        )
                                        .child(
                                            div().text_sm().text_color(rgb(accent)).child(primary),
                                        ),
                                )
                        })),
                )
                .into_any_element()
        }
        None => empty_state(
            "No message selected",
            "Select a thread, search result, or draft-related item to inspect it here.",
            muted,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_compose_detail(
    compose: Option<&ComposeState>,
    identities: &[IdentitySummary],
    active_field: Option<EditorField>,
    surface: u32,
    border: u32,
    accent: u32,
    muted: u32,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let Some(compose) = compose else {
        return empty_state(
            "No draft open",
            "Press Compose or select a draft to edit it here.",
            muted,
        );
    };

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(format!(
            "Draft {} • {}",
            compose.draft_id, compose.state_label
        ))
        .children(compose.last_error_message.as_ref().map(|error| {
            div()
                .text_sm()
                .text_color(rgb(0xF59E0B))
                .child(format!("Last send error: {error}"))
        }))
        .child("Identity")
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(identities.iter().map(|identity| {
                    let identity_id = identity.id;
                    let selected = identity.id == compose.identity_id;
                    let background = if selected { accent } else { surface };
                    div()
                        .id(("compose-identity", identity.id as u64))
                        .px_2()
                        .py_1()
                        .bg(rgb(background))
                        .border_1()
                        .border_color(rgb(border))
                        .child(format!(
                            "{} <{}>",
                            identity.display_name, identity.email_address
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_compose_identity(identity_id, cx);
                        }))
                })),
        )
        .child(
            input_card(
                "compose-to-field",
                "To",
                &compose.to,
                active_field == Some(EditorField::ComposeTo),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeTo, window, cx);
            })),
        )
        .child(
            input_card(
                "compose-cc-field",
                "Cc",
                &compose.cc,
                active_field == Some(EditorField::ComposeCc),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeCc, window, cx);
            })),
        )
        .child(
            input_card(
                "compose-bcc-field",
                "Bcc",
                &compose.bcc,
                active_field == Some(EditorField::ComposeBcc),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeBcc, window, cx);
            })),
        )
        .child(
            input_card(
                "compose-subject-field",
                "Subject",
                &compose.subject,
                active_field == Some(EditorField::ComposeSubject),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeSubject, window, cx);
            })),
        )
        .child(
            input_card(
                "compose-body-field",
                "Body",
                &compose.body,
                active_field == Some(EditorField::ComposeBody),
                true,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeBody, window, cx);
            })),
        )
        .child(
            input_card(
                "compose-attachment-field",
                "Attachment Path",
                &compose.attachment_path,
                active_field == Some(EditorField::ComposeAttachmentPath),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeAttachmentPath, window, cx);
            })),
        )
        .child("Attachments")
        .into_any_element()
}

fn render_account_setup_detail(
    form: &AccountSetupState,
    active_field: Option<EditorField>,
    surface: u32,
    border: u32,
    accent: u32,
    muted: u32,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child("Create a real IMAP or JMAP account directly in the desktop shell.")
        .child(
            div()
                .text_sm()
                .text_color(rgb(muted))
                .child("For IMAP accounts, configure SMTP submission here as well."),
        )
        .child(
            input_card(
                "account-display-name-field",
                "Display Name",
                &form.display_name,
                active_field == Some(EditorField::AccountDisplayName),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::AccountDisplayName, window, cx);
            })),
        )
        .child(
            input_card(
                "account-endpoint-field",
                "Endpoint",
                &form.endpoint,
                active_field == Some(EditorField::AccountEndpoint),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::AccountEndpoint, window, cx);
            })),
        )
        .child(
            input_card(
                "account-username-field",
                "Username",
                &form.username,
                active_field == Some(EditorField::AccountUsername),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::AccountUsername, window, cx);
            })),
        )
        .child(
            input_card(
                "account-password-field",
                "Password",
                &form.password,
                active_field == Some(EditorField::AccountPassword),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::AccountPassword, window, cx);
            })),
        )
        .child(
            input_card(
                "account-identity-name-field",
                "Primary Identity Name",
                &form.identity_name,
                active_field == Some(EditorField::AccountIdentityName),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::AccountIdentityName, window, cx);
            })),
        )
        .child(
            input_card(
                "account-identity-email-field",
                "Primary Identity Email",
                &form.identity_email,
                active_field == Some(EditorField::AccountIdentityEmail),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::AccountIdentityEmail, window, cx);
            })),
        )
        .child(
            input_card(
                "account-signature-field",
                "Signature",
                &form.signature,
                active_field == Some(EditorField::AccountSignature),
                true,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::AccountSignature, window, cx);
            })),
        )
        .child(
            input_card(
                "account-alias-emails-field",
                "Additional Identity Emails",
                &form.alias_emails,
                active_field == Some(EditorField::AccountAliasEmails),
                false,
                surface,
                border,
                accent,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::AccountAliasEmails, window, cx);
            })),
        )
        .children((form.protocol == ProtocolKind::Imap).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child("SMTP Submission")
                .child(
                    input_card(
                        "account-smtp-host-field",
                        "SMTP Host",
                        &form.smtp_host,
                        active_field == Some(EditorField::AccountSmtpHost),
                        false,
                        surface,
                        border,
                        accent,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_active_field(EditorField::AccountSmtpHost, window, cx);
                    })),
                )
                .child(
                    input_card(
                        "account-smtp-port-field",
                        "SMTP Port",
                        &form.smtp_port,
                        active_field == Some(EditorField::AccountSmtpPort),
                        false,
                        surface,
                        border,
                        accent,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_active_field(EditorField::AccountSmtpPort, window, cx);
                    })),
                )
                .child(
                    input_card(
                        "account-smtp-security-field",
                        "SMTP Security [plain|starttls|implicit_tls]",
                        &form.smtp_security,
                        active_field == Some(EditorField::AccountSmtpSecurity),
                        false,
                        surface,
                        border,
                        accent,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_active_field(EditorField::AccountSmtpSecurity, window, cx);
                    })),
                )
                .child(
                    input_card(
                        "account-smtp-username-field",
                        "SMTP Username",
                        &form.smtp_username,
                        active_field == Some(EditorField::AccountSmtpUsername),
                        false,
                        surface,
                        border,
                        accent,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_active_field(EditorField::AccountSmtpUsername, window, cx);
                    })),
                )
        }))
}

fn reader_action_buttons(
    selected_thread: &Option<ThreadView>,
    mailboxes: &[MailboxSummary],
    border: u32,
    button_background: u32,
    cx: &mut Context<WorkspaceView>,
) -> Vec<AnyElement> {
    let Some(thread) = selected_thread.as_ref() else {
        return Vec::new();
    };

    let thread_has_unread = thread.unread_count > 0;
    let thread_is_flagged = thread.is_flagged;
    let mut buttons = vec![
        toolbar_button(
            if thread_has_unread {
                "Mark Read"
            } else {
                "Mark Unread"
            },
            button_background,
            border,
            cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                let result = this
                    .selected_thread_id
                    .map(|thread_id| this.store.set_thread_read(thread_id, thread_has_unread))
                    .unwrap_or_else(|| Ok(()));
                if let Err(error) = result {
                    this.store_error = Some(error.to_string());
                } else {
                    this.refresh_from_store();
                }
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            if thread_is_flagged { "Unflag" } else { "Flag" },
            button_background,
            border,
            cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                let result = this
                    .selected_thread_id
                    .map(|thread_id| this.store.set_thread_flagged(thread_id, !thread_is_flagged))
                    .unwrap_or_else(|| Ok(()));
                if let Err(error) = result {
                    this.store_error = Some(error.to_string());
                } else {
                    this.refresh_from_store();
                }
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Archive",
            button_background,
            border,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                let result = this
                    .selected_thread_id
                    .map(|thread_id| this.store.archive_thread(thread_id))
                    .unwrap_or_else(|| Ok(()));
                if let Err(error) = result {
                    this.store_error = Some(error.to_string());
                } else {
                    this.refresh_from_store();
                }
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Delete",
            button_background,
            border,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                let result = this
                    .selected_thread_id
                    .map(|thread_id| this.store.delete_thread(thread_id))
                    .unwrap_or_else(|| Ok(()));
                if let Err(error) = result {
                    this.store_error = Some(error.to_string());
                } else {
                    this.refresh_from_store();
                }
                cx.notify();
            }),
        )
        .into_any_element(),
    ];

    for mailbox in mailboxes
        .iter()
        .filter(|mailbox| mailbox.id != thread.mailbox_id)
    {
        let mailbox_id = mailbox.id;
        let mailbox_name = mailbox.name.clone();
        buttons.push(
            toolbar_button(
                format!("Move: {mailbox_name}"),
                button_background,
                border,
                cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                    let result = this
                        .selected_thread_id
                        .map(|thread_id| this.store.move_thread(thread_id, mailbox_id))
                        .unwrap_or_else(|| Ok(()));
                    if let Err(error) = result {
                        this.store_error = Some(error.to_string());
                    } else {
                        this.selected_mailbox_id = Some(mailbox_id);
                        this.refresh_from_store();
                    }
                    cx.notify();
                }),
            )
            .into_any_element(),
        );
    }
    buttons
}

fn compose_focus_buttons(
    border: u32,
    button_background: u32,
    cx: &mut Context<WorkspaceView>,
) -> Vec<AnyElement> {
    vec![
        toolbar_button(
            "Focus To",
            button_background,
            border,
            cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                this.set_active_field(EditorField::ComposeTo, window, cx);
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Focus Subject",
            button_background,
            border,
            cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                this.set_active_field(EditorField::ComposeSubject, window, cx);
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Focus Body",
            button_background,
            border,
            cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                this.set_active_field(EditorField::ComposeBody, window, cx);
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Show Outbox",
            button_background,
            border,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.list_mode = ListMode::Outbox;
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Retry Selected Job",
            button_background,
            border,
            cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                this.retry_selected_outbox(window, cx);
            }),
        )
        .into_any_element(),
    ]
}

fn account_protocol_buttons(
    protocol: ProtocolKind,
    border: u32,
    button_background: u32,
    selected_background: u32,
    cx: &mut Context<WorkspaceView>,
) -> Vec<AnyElement> {
    vec![
        toolbar_button(
            "IMAP + SMTP",
            if protocol == ProtocolKind::Imap {
                selected_background
            } else {
                button_background
            },
            border,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.account_setup.protocol = ProtocolKind::Imap;
                cx.notify();
            }),
        )
        .into_any_element(),
        toolbar_button(
            "JMAP",
            if protocol == ProtocolKind::Jmap {
                selected_background
            } else {
                button_background
            },
            border,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.account_setup.protocol = ProtocolKind::Jmap;
                cx.notify();
            }),
        )
        .into_any_element(),
    ]
}

fn split_addresses(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn required_text(label: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(trimmed.to_owned())
}

fn validate_endpoint(protocol: ProtocolKind, endpoint: &str) -> Result<()> {
    let parsed =
        Url::parse(endpoint).with_context(|| format!("failed to parse endpoint {endpoint}"))?;
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

fn parse_submission_security(value: &str) -> Result<SubmissionSecurity> {
    SubmissionSecurity::parse(&value.trim().to_ascii_lowercase())
        .with_context(|| format!("unknown submission security mode {value}"))
}

fn derive_host(endpoint: &str) -> String {
    Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "localhost".to_owned())
}

fn sync_badge(phase: SyncPhase, accent: u32) -> (u32, &'static str) {
    match phase {
        SyncPhase::Idle => (accent, "Idle"),
        SyncPhase::Connecting => (0xF7C948, "Connecting"),
        SyncPhase::Discovering => (0x60A5FA, "Discovering"),
        SyncPhase::Syncing => (0x34D399, "Syncing"),
        SyncPhase::Backoff => (0xF59E0B, "Backoff"),
        SyncPhase::Failed => (0xF87171, "Failed"),
    }
}

fn short_sha(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn hex_to_rgb(hex: Option<&str>, fallback: u32) -> u32 {
    let Some(hex) = hex else {
        return fallback;
    };
    let trimmed = hex.trim_start_matches('#');
    if trimmed.len() != 6 {
        return fallback;
    }
    u32::from_str_radix(trimmed, 16).unwrap_or(fallback)
}
