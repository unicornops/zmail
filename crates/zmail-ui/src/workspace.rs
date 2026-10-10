//! Business-logic methods for [`WorkspaceView`].
//!
//! This module contains the single `impl WorkspaceView` block with all
//! non-render methods: store access, account/mailbox/thread selection, sync
//! orchestration, compose workflow, account setup, and key-event routing.
//!
//! Private utility functions used only by these methods live at the bottom of
//! this file.

use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail};
use gpui::{Context, KeyDownEvent, SharedString, Timer, Window};
use url::Url;
use zmail_core::{
    AccountSummary, AppBootstrap, AuthKind, CachePolicySummary, DraftUpdate, IdentitySummary,
    MailboxSummary, NewAccount, NewDraft, NewIdentity, OutgoingConfig, ProtocolKind, StoreStats,
    SubmissionSecurity, SyncPhase, ThreadMessage, ThreadView,
};
use zmail_notify::{NewMailNotifier, OsNotificationSink};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use super::{DetailMode, ListMode, WorkspaceView};
use crate::account_setup::AccountSetupState;
use crate::compose::ComposeState;
use crate::input::{EditorField, InputOutcome, UiBuffer};
use crate::styles::UiColors;

impl WorkspaceView {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Construct a new [`WorkspaceView`], loading initial state from the store.
    // The constructor mirrors the shell's wiring inputs; grouping them into a
    // config struct would hide the one-shot nature of the bootstrap.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        bootstrap: AppBootstrap,
        store: Arc<LocalStore>,
        sync_service: SyncService,
        secrets: Arc<dyn SecretStore>,
        notifier: Arc<NewMailNotifier<OsNotificationSink>>,
        initial_draft_id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        use gpui::UniformListScrollHandle;
        let mut view = Self {
            bootstrap,
            store,
            sync_service,
            secrets,
            notifier,
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
            pending_op_count: 0,
            dead_op_count: 0,
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
            hovered_element: None,
            refresh_scheduled: false,
        };

        // New mail only notifies while the window is unfocused: the quiet
        // flag mirrors the window activation state.
        view.notifier.set_quiet(window.is_window_active());
        cx.observe_window_activation(
            window,
            |this: &mut WorkspaceView, window: &mut Window, _cx: &mut Context<Self>| {
                this.notifier.set_quiet(window.is_window_active());
            },
        )
        .detach();

        view.refresh_from_store();
        if view.accounts.is_empty() {
            view.detail_mode = DetailMode::AccountSetup;
            view.active_field = Some(EditorField::AccountDisplayName);
        }

        // A mailto launch pre-selects the draft's account and opens the
        // compose pane on the pre-filled draft.
        if let Some(draft_id) = initial_draft_id
            && let Ok(Some(draft)) = view.store.draft_detail(draft_id)
        {
            view.select_account(draft.account_id, cx);
            view.load_draft_into_compose(draft_id);
        }

        // New mail synced in the background by the mail watcher appears
        // without user interaction; the refresh reads local state only.
        let entity = cx.entity();
        window
            .spawn(cx, async move |cx| {
                loop {
                    Timer::after(Duration::from_secs(5)).await;
                    let refreshed = entity.update(cx, |this, cx| {
                        this.refresh_from_store();
                        cx.notify();
                    });
                    if refreshed.is_err() {
                        break;
                    }
                }
            })
            .detach();

        view
    }

    // -----------------------------------------------------------------------
    // Store refresh
    // -----------------------------------------------------------------------

    /// Re-read all relevant state from the local store and update the view.
    ///
    /// Clears `refresh_scheduled` so the debounce guard is reset.
    pub(crate) fn refresh_from_store(&mut self) {
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
        self.pending_op_count = self
            .selected_account_id
            .and_then(|account_id| self.store.pending_op_count(account_id).ok())
            .unwrap_or(0);
        self.dead_op_count = self
            .selected_account_id
            .and_then(|account_id| self.store.dead_op_count(account_id).ok())
            .unwrap_or(0);

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

    /// Schedule a debounced store refresh 900 ms from now.
    ///
    /// A second call while a refresh is already pending is a no-op.
    pub(crate) fn schedule_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    // -----------------------------------------------------------------------
    // Selection helpers
    // -----------------------------------------------------------------------

    /// Return a reference to the currently-selected account summary, if any.
    pub(crate) fn selected_account(&self) -> Option<&AccountSummary> {
        self.selected_account_id.and_then(|account_id| {
            self.accounts
                .iter()
                .find(|account| account.id == account_id)
        })
    }

    /// Return a reference to the currently-selected mailbox summary, if any.
    pub(crate) fn selected_mailbox(&self) -> Option<&MailboxSummary> {
        self.selected_mailbox_id.and_then(|mailbox_id| {
            self.mailboxes
                .iter()
                .find(|mailbox| mailbox.id == mailbox_id)
        })
    }

    /// Fetch the full thread view for the selected thread from the store.
    ///
    /// Sets `store_error` on failure.  This takes `&mut self` because it
    /// mutates the error field as a side effect.
    pub(crate) fn selected_thread_view(&mut self) -> Option<ThreadView> {
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

    /// Track whether a specific interactive element is hovered.
    pub(crate) fn set_hovered_element(
        &mut self,
        element_id: SharedString,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if hovered {
            if self.hovered_element.as_ref() == Some(&element_id) {
                false
            } else {
                self.hovered_element = Some(element_id);
                true
            }
        } else if self.hovered_element.as_ref() == Some(&element_id) {
            self.hovered_element = None;
            true
        } else {
            false
        };

        if changed {
            cx.notify();
        }
    }

    /// Whether the given element is the currently-hovered item.
    pub(crate) fn is_hovered(&self, element_id: &SharedString) -> bool {
        self.hovered_element.as_ref() == Some(element_id)
    }

    /// Resolve the background for a selectable / hoverable surface.
    pub(crate) fn interactive_background(
        &self,
        element_id: &SharedString,
        selected: bool,
        base_background: u32,
        colors: &UiColors,
    ) -> u32 {
        if selected {
            colors.selected_background
        } else if self.is_hovered(element_id) {
            colors.hover_background
        } else {
            base_background
        }
    }

    /// Swap the active theme with the bundled alternate variant and trigger
    /// a rerender so all colours and icons update immediately.
    pub(crate) fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        std::mem::swap(
            &mut self.bootstrap.theme,
            &mut self.bootstrap.alternate_theme,
        );
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Navigation actions
    // -----------------------------------------------------------------------

    /// Switch the active account and refresh the view.
    pub(crate) fn select_account(&mut self, account_id: i64, cx: &mut Context<Self>) {
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

    /// Switch the active mailbox and select its first thread.
    pub(crate) fn select_mailbox(&mut self, mailbox_id: i64, cx: &mut Context<Self>) {
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

    /// Select a specific thread (e.g. from the search results list).
    pub(crate) fn select_thread(
        &mut self,
        thread_id: i64,
        mailbox_id: i64,
        cx: &mut Context<Self>,
    ) {
        self.selected_mailbox_id = Some(mailbox_id);
        self.selected_thread_id = Some(thread_id);
        self.detail_mode = DetailMode::Reader;
        self.hydrate_selected_thread_bodies(cx);
        cx.notify();
    }

    /// Fetches the bodies of the selected thread's un-hydrated messages on
    /// demand; the reader shows the preview until the body lands.
    ///
    /// Hydration failures are not surfaced: an offline open keeps showing
    /// the stored preview instead of an error.
    pub(crate) fn hydrate_selected_thread_bodies(&mut self, cx: &mut Context<Self>) {
        let (Some(account_id), Some(thread_id)) =
            (self.selected_account_id, self.selected_thread_id)
        else {
            return;
        };
        let needs_hydration = match self.store.messages_missing_bodies(thread_id) {
            Ok(Some(missing)) => !missing.is_empty(),
            _ => false,
        };
        if !needs_hydration {
            return;
        }

        let service = self.sync_service.clone();
        cx.spawn(async move |this, cx| {
            let _ = service
                .hydrate_thread_body_and_wait(account_id, thread_id)
                .await;
            let _ = this.update(cx, |view, cx| {
                view.refresh_from_store();
                cx.notify();
            });
        })
        .detach();
    }

    // -----------------------------------------------------------------------
    // Sync actions
    // -----------------------------------------------------------------------

    /// Trigger a sync for the currently-selected account.
    pub(crate) fn sync_selected_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Trigger a sync for all configured accounts.
    pub(crate) fn sync_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// Execute the current search query against the local full-text index.
    pub(crate) fn run_search(&mut self, cx: &mut Context<Self>) {
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

    // -----------------------------------------------------------------------
    // Compose workflow
    // -----------------------------------------------------------------------

    /// Open the compose pane for a new draft, creating the draft in the store.
    pub(crate) fn open_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Open a reply or reply-all draft for the selected thread.
    pub(crate) fn reply_to_selected_thread(
        &mut self,
        reply_all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.selected_thread_view() else {
            self.store_error
                .get_or_insert_with(|| "Select a thread first.".to_owned());
            cx.notify();
            return;
        };

        match self.create_reply_draft(&thread, reply_all) {
            Ok(draft_id) => {
                self.load_draft_into_compose(draft_id);
                self.list_mode = ListMode::Drafts;
                self.detail_mode = DetailMode::Compose;
                self.active_field = Some(EditorField::ComposeBody);
                self.status_message = Some(if reply_all {
                    format!("Reply-all draft {draft_id} ready.")
                } else {
                    format!("Reply draft {draft_id} ready.")
                });
                self.refresh_from_store();
                window.focus(&self.focus_handle);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Open a forward draft for the selected thread.
    /// Saves every attachment of the selected thread into the store's
    /// `saved-attachments` directory and reports the destination.
    pub(crate) fn save_selected_thread_attachments(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.selected_thread_view() else {
            self.store_error
                .get_or_insert_with(|| "Select a thread first.".to_owned());
            cx.notify();
            return;
        };

        let destination = self
            .store
            .path()
            .parent()
            .map(|parent| parent.join("saved-attachments"))
            .unwrap_or_else(|| std::path::PathBuf::from("saved-attachments"));

        let mut saved = 0usize;
        let mut last_error = None;
        for message in &thread.messages {
            for attachment in &message.attachments {
                match self.store.save_attachment(attachment.id, &destination) {
                    Ok(_) => saved += 1,
                    Err(error) => last_error = Some(error.to_string()),
                }
            }
        }

        if let Some(error) = last_error {
            self.store_error = Some(error);
        } else if saved == 0 {
            self.store_error = Some("The selected thread has no attachments.".to_owned());
        } else {
            self.store_error = None;
            self.status_message = Some(format!(
                "Saved {saved} attachment(s) to {}.",
                destination.display()
            ));
        }
        cx.notify();
    }

    pub(crate) fn forward_selected_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.selected_thread_view() else {
            self.store_error
                .get_or_insert_with(|| "Select a thread first.".to_owned());
            cx.notify();
            return;
        };

        match self.create_forward_draft(&thread) {
            Ok(draft_id) => {
                self.load_draft_into_compose(draft_id);
                self.list_mode = ListMode::Drafts;
                self.detail_mode = DetailMode::Compose;
                self.active_field = Some(EditorField::ComposeTo);
                self.status_message = Some(format!("Forward draft {draft_id} ready."));
                self.refresh_from_store();
                window.focus(&self.focus_handle);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn create_reply_draft(&self, thread: &ThreadView, reply_all: bool) -> Result<i64> {
        let latest_message = thread
            .messages
            .last()
            .context("selected thread has no messages to reply to")?;
        let own_addresses: HashSet<String> = self
            .identities
            .iter()
            .map(|identity| normalize_contact(&identity.email_address))
            .filter(|value| !value.is_empty())
            .collect();
        let sender = latest_message.sender.trim().to_owned();
        let sender_key = normalize_contact(&sender);

        let mut to =
            if sender.is_empty() || sender_key.is_empty() || own_addresses.contains(&sender_key) {
                Vec::new()
            } else {
                vec![sender]
            };

        let mut cc = if reply_all {
            dedupe_contacts(
                split_contact_summary(&latest_message.recipients)
                    .into_iter()
                    .filter(|recipient| {
                        let normalized = normalize_contact(recipient);
                        !normalized.is_empty()
                            && normalized != sender_key
                            && !own_addresses.contains(&normalized)
                    })
                    .collect(),
            )
        } else {
            Vec::new()
        };

        if to.is_empty() && !cc.is_empty() {
            to.push(cc.remove(0));
        }

        if to.is_empty() {
            bail!("the selected message does not expose reply recipients");
        }

        self.create_thread_draft(
            to,
            cc,
            prefixed_subject("Re", &thread.subject),
            quoted_reply_body(latest_message),
        )
    }

    fn create_forward_draft(&self, thread: &ThreadView) -> Result<i64> {
        let latest_message = thread
            .messages
            .last()
            .context("selected thread has no messages to forward")?;

        self.create_thread_draft(
            Vec::new(),
            Vec::new(),
            prefixed_subject("Fwd", &thread.subject),
            forwarded_body(latest_message),
        )
    }

    fn create_thread_draft(
        &self,
        to: Vec<String>,
        cc: Vec<String>,
        subject: String,
        body: String,
    ) -> Result<i64> {
        let account_id = self
            .selected_account_id
            .context("select an account before composing")?;
        let identity = self.default_compose_identity()?;

        self.store.create_draft(&NewDraft {
            account_id,
            identity_id: identity.id,
            to,
            cc,
            bcc: Vec::new(),
            subject,
            body,
        })
    }

    fn default_compose_identity(&self) -> Result<IdentitySummary> {
        self.identities
            .iter()
            .find(|identity| identity.is_primary)
            .or_else(|| self.identities.first())
            .cloned()
            .context("select or configure an account identity before composing")
    }

    /// Load a draft from the store into the compose state.
    pub(crate) fn load_draft_into_compose(&mut self, draft_id: i64) {
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

    /// Flush the current compose buffers back to the store.
    pub(crate) fn save_compose(&mut self) -> Result<()> {
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

    /// Persist the currently-open draft and surface a status banner.
    pub(crate) fn save_current_draft(&mut self, cx: &mut Context<Self>) {
        let draft_id = self.compose.as_ref().map(|compose| compose.draft_id);

        match self.save_compose() {
            Ok(()) => {
                self.store_error = None;
                if let Some(draft_id) = draft_id {
                    self.status_message = Some(format!("Draft {draft_id} saved."));
                    self.load_draft_into_compose(draft_id);
                    self.refresh_from_store();
                }
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }

        cx.notify();
    }

    /// Save and submit the current draft to the outbox.
    pub(crate) fn send_current_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Add the file at the path currently in the attachment-path buffer.
    pub(crate) fn add_current_attachment(&mut self, cx: &mut Context<Self>) {
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

    /// Remove an attachment by its store ID.
    pub(crate) fn remove_current_attachment(&mut self, attachment_id: i64, cx: &mut Context<Self>) {
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

    /// Switch the sending identity for the open compose draft.
    pub(crate) fn set_compose_identity(&mut self, identity_id: i64, cx: &mut Context<Self>) {
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

    /// Retry the currently-selected outbox job.
    pub(crate) fn retry_selected_outbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    // -----------------------------------------------------------------------
    // Account setup
    // -----------------------------------------------------------------------

    /// Switch to the account setup detail pane.
    pub(crate) fn open_account_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detail_mode = DetailMode::AccountSetup;
        self.active_field = Some(EditorField::AccountDisplayName);
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Validate and submit the account setup form, inserting the account into
    /// the store and triggering an initial sync.
    pub(crate) fn submit_account_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.build_account_from_form() {
            Ok((account, secret)) => match self.store.insert_account(&account) {
                Ok(account_id) => {
                    // The optional disk budget applies after the account
                    // exists; parse failures surface like any other form
                    // error.
                    match parse_disk_budget_mb(&self.account_setup.disk_budget_mb.text) {
                        Ok(Some(budget_mb)) => {
                            if let Err(error) = self
                                .store
                                .set_account_disk_budget(account_id, Some(budget_mb))
                            {
                                self.store_error = Some(error.to_string());
                                cx.notify();
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            self.store_error = Some(error.to_string());
                            cx.notify();
                            return;
                        }
                    }
                    let oauth2_flow = match secret {
                        AccountSetupSecret::Password(secret) => {
                            if let Err(error) = self.secrets.set_account_secret(account_id, &secret)
                            {
                                self.store_error = Some(error.to_string());
                                cx.notify();
                                return;
                            }
                            None
                        }
                        AccountSetupSecret::OAuth2(client) => {
                            // The browser flow blocks for up to its redirect
                            // timeout, so it runs off the GUI thread; the
                            // account's sync status marks the wait, and the
                            // grant (tokens, client config, expiry) is
                            // persisted before the first sync starts.
                            let _ = self.store.begin_sync_attempt(
                                account_id,
                                SyncPhase::Connecting,
                                "Waiting for browser authorization",
                            );
                            self.spawn_oauth2_authorization(account_id, client);
                            Some(())
                        }
                    };
                    self.selected_account_id = Some(account_id);
                    self.detail_mode = DetailMode::Reader;
                    self.list_mode = ListMode::Mail;
                    self.account_setup = AccountSetupState::default();
                    self.active_field = None;
                    self.status_message = Some(if oauth2_flow.is_some() {
                        format!(
                            "Account {account_id} added. Finish the authorization in the opened browser page; the account syncs once it completes."
                        )
                    } else {
                        format!("Account {account_id} added.")
                    });
                    // OAuth2 accounts sync from the authorization thread,
                    // once the grant is stored; syncing here would only
                    // record a missing-credentials failure.
                    if oauth2_flow.is_none() {
                        let _ = self.sync_service.sync_account(account_id);
                    }
                    // Start the new account's background watcher immediately
                    // instead of waiting for the next app launch.
                    let _ = self.sync_service.sync_watchers();
                    self.refresh_from_store();
                    self.schedule_refresh(window, cx);
                }
                Err(error) => self.store_error = Some(error.to_string()),
            },
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Runs the OAuth2 browser flow on a background thread and stores the
    /// resulting grant, so the GUI never blocks on the redirect wait.
    fn spawn_oauth2_authorization(&self, account_id: i64, client: zmail_auth::OAuth2Client) {
        let store = self.store.clone();
        let secrets = self.secrets.clone();
        let sync_service = self.sync_service.clone();
        let spawn = std::thread::Builder::new()
            .name("zmail-oauth2-setup".to_owned())
            .spawn(move || {
                let tokens = match client.authorize_interactively() {
                    Ok(tokens) => tokens,
                    Err(error) => {
                        let _ = store.record_sync_failure(
                            account_id,
                            "Browser authorization failed",
                            error.to_string(),
                        );
                        return;
                    }
                };
                if let Err(error) = zmail_sync::persist_oauth2_grant(
                    &store,
                    secrets.as_ref(),
                    account_id,
                    &client,
                    &tokens,
                ) {
                    let _ = store.record_sync_failure(
                        account_id,
                        "Storing the authorization failed",
                        error.to_string(),
                    );
                    return;
                }
                let _ = sync_service.sync_account(account_id);
                let _ = sync_service.sync_watchers();
            });
        if let Err(error) = spawn {
            let _ = self.store.record_sync_failure(
                account_id,
                "Browser authorization failed",
                error.to_string(),
            );
        }
    }

    /// Build a [`NewAccount`] plus the form's credential plan.
    pub(crate) fn build_account_from_form(&self) -> Result<(NewAccount, AccountSetupSecret)> {
        let form = &self.account_setup;
        let display_name = required_text("Display name", &form.display_name.text)?;
        let endpoint = required_text("Endpoint", &form.endpoint.text)?;
        validate_endpoint(form.protocol, &endpoint)?;
        let username = required_text("Username", &form.username.text)?;

        // JMAP accounts authenticate with a password; bearer-token auth for
        // JMAP is not wired yet.
        // JMAP accounts can authenticate with a password or an OAuth2
        // bearer token; no consumer presets offer JMAP, so OAuth2 uses the
        // provider's own endpoints entered in the form.
        let (auth, secret) = if form.protocol == ProtocolKind::Jmap {
            match form.auth {
                AuthKind::Password => (
                    AuthKind::Password,
                    AccountSetupSecret::Password(required_text("Password", &form.password.text)?),
                ),
                AuthKind::OAuth2 => {
                    let client = custom_oauth2_client_from_form(form)?;
                    (AuthKind::OAuth2, AccountSetupSecret::OAuth2(client))
                }
            }
        } else {
            match form.auth {
                AuthKind::Password => (
                    AuthKind::Password,
                    AccountSetupSecret::Password(required_text("Password", &form.password.text)?),
                ),
                AuthKind::OAuth2 => {
                    let preset = form
                        .oauth_preset
                        .context("Choose an OAuth2 provider (Gmail or Microsoft 365)")?;
                    let client_id = required_text("OAuth2 client id", &form.oauth_client_id.text)?;
                    let client_secret = trimmed_or_none(&form.oauth_client_secret.text);
                    let client = preset.client(client_id, client_secret, OAUTH2_REDIRECT_PORT);
                    (AuthKind::OAuth2, AccountSetupSecret::OAuth2(client))
                }
            }
        };
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

    // -----------------------------------------------------------------------
    // Input routing
    // -----------------------------------------------------------------------

    /// Set the active editor field and give keyboard focus to the workspace.
    pub(crate) fn set_active_field(
        &mut self,
        field: EditorField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_field = Some(field);
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Return a mutable reference to the buffer for the currently-active field.
    pub(crate) fn active_buffer_mut(&mut self) -> Option<&mut UiBuffer> {
        match self.active_field? {
            EditorField::SearchQuery => Some(&mut self.search_query),
            EditorField::ComposeTo => self.compose.as_mut().map(|c| &mut c.to),
            EditorField::ComposeCc => self.compose.as_mut().map(|c| &mut c.cc),
            EditorField::ComposeBcc => self.compose.as_mut().map(|c| &mut c.bcc),
            EditorField::ComposeSubject => self.compose.as_mut().map(|c| &mut c.subject),
            EditorField::ComposeBody => self.compose.as_mut().map(|c| &mut c.body),
            EditorField::ComposeAttachmentPath => {
                self.compose.as_mut().map(|c| &mut c.attachment_path)
            }
            EditorField::AccountDisplayName => Some(&mut self.account_setup.display_name),
            EditorField::AccountEndpoint => Some(&mut self.account_setup.endpoint),
            EditorField::AccountUsername => Some(&mut self.account_setup.username),
            EditorField::AccountPassword => Some(&mut self.account_setup.password),
            EditorField::AccountOauthClientId => Some(&mut self.account_setup.oauth_client_id),
            EditorField::AccountOauthClientSecret => {
                Some(&mut self.account_setup.oauth_client_secret)
            }
            EditorField::AccountOauthAuthorizationEndpoint => {
                Some(&mut self.account_setup.oauth_authorization_endpoint)
            }
            EditorField::AccountOauthTokenEndpoint => {
                Some(&mut self.account_setup.oauth_token_endpoint)
            }
            EditorField::AccountOauthScopes => Some(&mut self.account_setup.oauth_scopes),
            EditorField::AccountDiskBudget => Some(&mut self.account_setup.disk_budget_mb),
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

    /// GPUI key-down handler: route events to the active buffer and react to
    /// the outcome.
    pub(crate) fn handle_key_down(
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
                | EditorField::AccountOauthClientSecret
                | EditorField::AccountSignature
                | EditorField::AccountAliasEmails
                | EditorField::AccountSmtpUsername => self.submit_account_setup(window, cx),
                _ => cx.notify(),
            },
            InputOutcome::Ignored => {}
        }
    }

    /// Switch the list panel mode.  Switching to [`ListMode::Mail`] also
    /// resets the detail pane to the reader.
    pub(crate) fn set_list_mode(&mut self, list_mode: ListMode, cx: &mut Context<Self>) {
        self.list_mode = list_mode;
        if matches!(list_mode, ListMode::Mail) {
            self.detail_mode = DetailMode::Reader;
        }
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// Private utility functions
// ---------------------------------------------------------------------------

/// Split a comma-separated address string into individual trimmed addresses,
/// discarding empty entries.
pub(crate) fn split_addresses(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Loopback port the UI OAuth2 flow listens on for the provider redirect,
/// matching the CLI default.
const OAUTH2_REDIRECT_PORT: u16 = 8357;

/// Credentials the account setup form collected for a new account.
pub(crate) enum AccountSetupSecret {
    /// Password or app password, stored directly.
    Password(String),
    /// The OAuth2 client whose browser flow runs after the account is stored.
    OAuth2(zmail_auth::OAuth2Client),
}

/// Parses the account setup form's disk budget: empty means unlimited.
fn parse_disk_budget_mb(value: &str) -> Result<Option<u64>> {
    match value.trim() {
        "" => Ok(None),
        trimmed => trimmed
            .parse::<u64>()
            .map(Some)
            .context("the disk budget must be a whole number of MiB"),
    }
}

/// Returns the trimmed text, or `None` when it is empty or whitespace-only.
fn trimmed_or_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Builds the provider-agnostic OAuth2 client for JMAP accounts from the
/// form's custom endpoint fields.
fn custom_oauth2_client_from_form(form: &AccountSetupState) -> Result<zmail_auth::OAuth2Client> {
    let client_id = required_text("OAuth2 client id", &form.oauth_client_id.text)?;
    let client_secret = trimmed_or_none(&form.oauth_client_secret.text);
    let authorization_endpoint = required_text(
        "OAuth2 authorization endpoint",
        &form.oauth_authorization_endpoint.text,
    )?;
    let token_endpoint = required_text("OAuth2 token endpoint", &form.oauth_token_endpoint.text)?;
    let scopes = form
        .oauth_scopes
        .text
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(zmail_auth::OAuth2Client {
        client_id,
        client_secret,
        authorization_endpoint,
        token_endpoint,
        scopes,
        redirect_port: OAUTH2_REDIRECT_PORT,
        extra_authorize_params: Vec::new(),
    })
}

/// Return `Ok(trimmed)` or an error if `value` is empty or whitespace-only.
fn required_text(label: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(trimmed.to_owned())
}

/// Validate that `endpoint` is a well-formed URL with the correct scheme for
/// `protocol`.
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

/// Parse a submission security mode string (case-insensitive).
fn parse_submission_security(value: &str) -> Result<SubmissionSecurity> {
    SubmissionSecurity::parse(&value.trim().to_ascii_lowercase())
        .with_context(|| format!("unknown submission security mode {value}"))
}

/// Extract the host component from an endpoint URL, defaulting to
/// `"localhost"` if parsing fails.
fn derive_host(endpoint: &str) -> String {
    Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "localhost".to_owned())
}

fn prefixed_subject(prefix: &str, subject: &str) -> String {
    let trimmed = subject.trim();
    let normalized_prefix = format!("{}:", prefix.to_ascii_lowercase());

    if trimmed.to_ascii_lowercase().starts_with(&normalized_prefix) {
        trimmed.to_owned()
    } else if trimmed.is_empty() {
        format!("{prefix}:")
    } else {
        format!("{prefix}: {trimmed}")
    }
}

fn quoted_reply_body(message: &ThreadMessage) -> String {
    format!(
        "\n\nOn {}, {}, wrote:\n{}",
        message.received_at,
        message.sender,
        quote_text(&message.rendered_body)
    )
}

fn forwarded_body(message: &ThreadMessage) -> String {
    format!(
        "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {}\nTo: {}\nSubject: {}\n\n{}",
        message.sender,
        message.received_at,
        if message.recipients.trim().is_empty() {
            "(unknown)"
        } else {
            message.recipients.trim()
        },
        message.subject,
        message.rendered_body
    )
}

fn quote_text(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn split_contact_summary(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn dedupe_contacts(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for value in values {
        let normalized = normalize_contact(&value);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        deduped.push(value);
    }

    deduped
}

fn normalize_contact(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some(start) = trimmed.rfind('<')
        && let Some(end) = trimmed.rfind('>')
        && start < end
    {
        return trimmed[start + 1..end].trim().to_ascii_lowercase();
    }

    trimmed.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{
        AccountSetupState, custom_oauth2_client_from_form, normalize_contact, parse_disk_budget_mb,
        prefixed_subject, split_contact_summary,
    };

    #[test]
    fn disk_budget_parses_empty_as_unlimited() {
        assert_eq!(parse_disk_budget_mb("").expect("empty parses"), None);
        assert_eq!(parse_disk_budget_mb("  ").expect("blank parses"), None);
        assert_eq!(
            parse_disk_budget_mb(" 512 ").expect("a number parses"),
            Some(512)
        );
        assert!(parse_disk_budget_mb("512x").is_err(), "garbage is rejected");
        assert!(
            parse_disk_budget_mb("-1").is_err(),
            "negatives are rejected"
        );
    }

    #[test]
    fn custom_oauth2_client_from_form_validates_and_builds() {
        let mut form = AccountSetupState::default();
        form.oauth_client_id.text = "client-1".to_owned();

        // Missing provider endpoints are rejected with a clear label.
        let error = custom_oauth2_client_from_form(&form)
            .expect_err("the authorization endpoint should be required");
        assert!(
            format!("{error}").contains("OAuth2 authorization endpoint"),
            "the error should name the missing field: {error}"
        );

        form.oauth_authorization_endpoint.text = "https://provider.test/authorize".to_owned();
        let error = custom_oauth2_client_from_form(&form)
            .expect_err("the token endpoint should be required");
        assert!(
            format!("{error}").contains("OAuth2 token endpoint"),
            "the error should name the missing field: {error}"
        );

        form.oauth_token_endpoint.text = "https://provider.test/token".to_owned();
        form.oauth_scopes.text = "email profile".to_owned();
        let client = custom_oauth2_client_from_form(&form).expect("the custom client should build");
        assert_eq!(client.client_id, "client-1");
        assert_eq!(
            client.authorization_endpoint,
            "https://provider.test/authorize"
        );
        assert_eq!(client.token_endpoint, "https://provider.test/token");
        assert_eq!(
            client.scopes,
            vec!["email".to_owned(), "profile".to_owned()]
        );
        assert!(client.client_secret.is_none());
    }

    #[test]
    fn prefixed_subject_avoids_duplicate_reply_prefix() {
        assert_eq!(prefixed_subject("Re", "Re: Existing"), "Re: Existing");
        assert_eq!(prefixed_subject("Fwd", "Subject"), "Fwd: Subject");
    }

    #[test]
    fn normalize_contact_extracts_bracketed_email() {
        assert_eq!(
            normalize_contact("Alice Example <alice@example.com>"),
            "alice@example.com"
        );
        assert_eq!(normalize_contact("bob@example.com"), "bob@example.com");
    }

    #[test]
    fn split_contact_summary_supports_commas_and_semicolons() {
        assert_eq!(
            split_contact_summary("alice@example.com, bob@example.com; carol@example.com"),
            vec!["alice@example.com", "bob@example.com", "carol@example.com",]
        );
    }
}
