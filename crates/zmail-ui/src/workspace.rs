//! The workspace shell: [`WorkspaceView`]'s logic and rendering.
//!
//! The shell reads the local store, keeps the selection, and pushes what
//! each pane shows into it ([`WorkspaceView::sync_panes`] runs after every
//! change). It handles the commands that need more than one pane, such as
//! replying (selected thread plus the account's identities) or moving a
//! thread, and it moves keyboard focus between panes.
//!
//! Panes report back through events; mail changes go through
//! [`MailActions`], whose [`MailEvent`]s make the shell refresh from the
//! store. Private utility functions live at the bottom of this file.

use std::{collections::HashSet, time::Duration};

use anyhow::{Context as _, Result, bail};
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight,
    IntoElement, MouseButton, Render, SharedString, StyleRefinement, Styled, Window,
    WindowAppearance, div, hsla, prelude::*, px, rgb,
};
use std::sync::Arc;
use zmail_core::{
    AccountSummary, AppBootstrap, IdentitySummary, MailboxSelection, MailboxSummary, NewDraft,
    OutboxJobState, StoreStats, SyncPhase, ThemeAppearance, ThreadMessage, ThreadView,
    VirtualMailbox,
    text::count_noun,
    time::{HourCycle, format_full_date, next_local_midnight},
};
use zmail_notify::{NewMailNotifier, OsNotificationSink};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use super::{DetailMode, InitialView, ListMode, WorkspaceView};
use crate::account_setup::{AccountSetupEvent, AccountSetupView};
use crate::actions::{
    AddAccount, Archive, Cancel, Delete, DismissSenderWarning, FocusNextPane, FocusPreviousPane,
    FocusSearch, Forward, GoBack, MoveToMailbox, NewMessage, Reply, ReplyAll, ReplyToMessage,
    RetryOutbox, SaveAttachments, ShowDrafts, ShowMail, ShowOutbox, SyncAccount, SyncAll,
    ToggleFlag, ToggleRead, ToggleSidebar, ToggleTheme, Undo,
};
use crate::app_menu::AppMenuButton;
use crate::components::{
    self, IconButton, SplitHandle, TextInput, TextInputEvent, TitleBar, Toast, ToastHost, ToastKind,
};
use crate::compose::{ComposeEvent, ComposeView, ComposeWindow, Presentation};
use crate::layout::{LayoutMode, PaneLayout, SinglePane};
use crate::mail_actions::{MailActions, MailEvent, UndoEntry, UndoId};
use crate::message_list::{
    AccountLabel, ListMailbox, ListSelection, MessageListEvent, MessageListState, MessageListView,
};
use crate::pane::{Pane, dispatch_from};
use crate::reader::{ReaderState, ReaderView};
use crate::sidebar::{
    OutboxStatus, SidebarEvent, SidebarHighlight, SidebarState, SidebarView, TreeNode,
};
use crate::styles::{
    DETAIL_MIN_WIDTH, ICON_SIZE_SM, MESSAGE_LIST_MIN_WIDTH, MESSAGE_LIST_WIDTH, MenuPlacement,
    PlatformStyle, READER_COMPACT_ACTIONS_BELOW, SIDEBAR_MIN_WIDTH, SIDEBAR_WIDTH, UiColors,
};
use crate::widgets::{EllipsisExt, HoverExt, icon, status_banner};

/// The inline reply open below a conversation.
pub(crate) struct InlineReply {
    pub(crate) thread_id: i64,
    pub(crate) compose: Entity<ComposeView>,
    _subscription: gpui::Subscription,
}

/// Where Undo Send puts the draft back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReopenIn {
    /// Its compose window.
    Window,
    /// The compose pane.
    Pane,
    /// The inline reply below this conversation.
    Reply(i64),
}

/// The key context the shell sets on the window root.
pub(crate) const KEY_CONTEXT: &str = "Workspace";

impl WorkspaceView {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Construct the shell and its panes, loading initial state from the
    /// store.
    // The constructor mirrors the shell's wiring inputs; grouping them into a
    // config struct would hide the one-shot nature of the bootstrap.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        bootstrap: AppBootstrap,
        store: Arc<LocalStore>,
        sync_service: SyncService,
        secrets: Arc<dyn SecretStore>,
        notifier: Arc<NewMailNotifier<OsNotificationSink>>,
        initial_view: InitialView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hour_cycle = HourCycle::from_env();
        let mail_actions = cx.new(|_| MailActions::new(store.clone()));
        let sidebar = cx.new(SidebarView::new);
        let message_list =
            cx.new(|cx| MessageListView::new(store.clone(), mail_actions.clone(), hour_cycle, cx));
        let reader = cx.new(|cx| ReaderView::new(hour_cycle, cx));
        let compose = cx.new(|cx| {
            ComposeView::new(
                store.clone(),
                sync_service.clone(),
                mail_actions.clone(),
                Presentation::Pane,
                cx,
            )
        });
        let account_setup = cx.new(|cx| {
            AccountSetupView::new(store.clone(), secrets, sync_service.clone(), window, cx)
        });
        let search_query = cx.new(|cx| {
            TextInput::new(cx)
                .compact()
                .leading_icon("search")
                .clearable("Clear search")
                .label("Search mail")
                .placeholder("Search mail")
        });

        let focus_handle = cx.focus_handle();
        let app_menu = cx.new(|_| AppMenuButton::new(focus_handle.clone()));
        let toasts = cx.new(|_| ToastHost::new());

        let subscriptions = vec![
            cx.subscribe(
                &search_query,
                |this, _, event: &TextInputEvent, cx| match event {
                    TextInputEvent::Changed => this.search_edited(cx),
                    TextInputEvent::Submitted => this.submit_search(cx),
                },
            ),
            cx.subscribe_in(&mail_actions, window, Self::on_mail_event),
            cx.subscribe_in(&sidebar, window, Self::on_sidebar_event),
            cx.subscribe_in(&message_list, window, Self::on_message_list_event),
            cx.subscribe_in(&compose, window, Self::on_compose_event),
            cx.subscribe_in(&account_setup, window, Self::on_account_setup_event),
            // Whatever changed in the shell, the panes get the new state.
            cx.observe_self(|this, cx| this.sync_panes(cx)),
        ];

        let mut view = Self {
            bootstrap,
            store,
            sync_service,
            notifier,
            focus_handle,
            mail_actions,
            sidebar,
            message_list,
            reader,
            compose,
            account_setup,
            search_query,
            app_menu,
            toasts,
            attachments: crate::attachments::AttachmentState::new(cx),
            undo_toast: None,
            last_session_jobs: Vec::new(),
            compose_windows: Default::default(),
            inline_reply: None,
            mail_refresh_queued: false,
            account: None,
            accounts: Vec::new(),
            identities: Vec::new(),
            stats: StoreStats {
                account_count: 0,
                mailbox_count: 0,
                thread_count: 0,
                message_count: 0,
            },
            all_mailboxes: Vec::new(),
            mailboxes: Vec::new(),
            drafts: Vec::new(),
            outbox_jobs: Vec::new(),
            pending_op_count: 0,
            dead_op_count: 0,
            search_results: Vec::new(),
            search_results_query: String::new(),
            search_pending_query: String::new(),
            search_context: Default::default(),
            search_typing: gpui::Task::ready(()),
            search_read: gpui::Task::ready(()),
            selected_account_id: None,
            selected_mailbox_id: None,
            virtual_mailbox: None,
            virtual_counts: Default::default(),
            flagged_unread: 0,
            sidebar_tree: Default::default(),
            selected_thread_id: None,
            list_selection: ListSelection::default(),
            selected_draft_id: None,
            selected_outbox_job_id: None,
            list_mode: ListMode::Mail,
            detail_mode: DetailMode::Reader,
            store_error: None,
            status_message: None,
            status_generation: 0,
            thread_view_cache: None,
            refresh_scheduled: false,
            theme_overridden: false,
            hour_cycle,
            sidebar_collapsed: false,
            density: Default::default(),
            sidebar_width: SIDEBAR_WIDTH,
            list_width: MESSAGE_LIST_WIDTH,
            pane_widths_generation: 0,
            sidebar_overlay_open: false,
            single_pane: SinglePane::List,
            viewport_width: f32::from(window.viewport_size().width),
            _subscriptions: subscriptions,
        };

        view.prune_open_cache_later(cx);

        // A job still queued or sending now was queued before this window
        // opened: the sync service sends it, and a toast says so.
        view.last_session_jobs = view
            .store
            .list_outbox_jobs(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|job| matches!(job.state, OutboxJobState::Queued | OutboxJobState::Sending))
            .map(|job| job.id)
            .collect();

        // The pane layout follows the window width.
        cx.observe_window_bounds(
            window,
            |this: &mut WorkspaceView, window: &mut Window, cx: &mut Context<Self>| {
                this.on_window_resized(f32::from(window.viewport_size().width), window, cx);
            },
        )
        .detach();

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

        // Start in the saved theme mode (system, light or dark) and follow
        // system changes unless the user has picked a theme.
        view.apply_theme_preference(window.appearance());
        view.apply_layout_preferences();
        cx.set_global(UiColors::from_theme(&view.bootstrap.theme));
        cx.observe_window_appearance(
            window,
            |this: &mut WorkspaceView, window: &mut Window, cx: &mut Context<Self>| {
                if !this.theme_overridden && this.follow_system_appearance(window.appearance()) {
                    this.apply_theme(window, cx);
                }
            },
        )
        .detach();

        view.refresh_from_store(cx);
        if view.accounts.is_empty() {
            view.detail_mode = DetailMode::AccountSetup;
            view.account_setup.update(cx, |form, cx| {
                form.open(
                    Some("Add your first mail account to get started.".into()),
                    window,
                    cx,
                );
            });
        } else {
            window.focus(&view.message_list.focus_handle(cx), cx);
        }

        if matches!(
            initial_view,
            InitialView::Inbox | InitialView::Sidebar | InitialView::Search(_)
        ) {
            view.restore_last_mailbox(cx);
        }
        match initial_view {
            InitialView::Inbox => {}
            // A mailto launch pre-selects the draft's account and opens the
            // pre-filled draft in a compose window.
            InitialView::Draft(draft_id) => {
                if let Ok(Some(draft)) = view.store.draft_detail(draft_id) {
                    view.select_account(draft.account_id, cx);
                    // Once the main window is up.
                    cx.defer_in(window, move |view, window, cx| {
                        view.open_draft_in_window(draft_id, false, window, cx);
                    });
                }
            }
            InitialView::Reply(thread_id) => {
                if let Ok(Some(thread)) = view.store.thread_view(thread_id) {
                    view.show_thread(thread_id, thread.mailbox_id, cx);
                    view.refresh_from_store(cx);
                    view.reply_to_selected_thread(false, None, window, cx);
                }
            }
            InitialView::Sending(draft_id) => {
                if let Ok(Some(draft)) = view.store.draft_detail(draft_id) {
                    view.select_account(draft.account_id, cx);
                    view.open_draft(draft_id, window, cx);
                    view.compose
                        .update(cx, |compose, cx| compose.send_draft(cx));
                }
            }
            // `show_thread`, not `select_thread`, which would mark the thread
            // read after a delay.
            InitialView::Thread(thread_id)
            | InitialView::ThreadExpanded(thread_id)
            | InitialView::ThreadSelection(thread_id)
            | InitialView::ThreadLinkPrompt(thread_id) => {
                if let Ok(Some(thread)) = view.store.thread_view(thread_id) {
                    view.show_thread(thread_id, thread.mailbox_id, cx);
                    view.refresh_from_store(cx);
                    view.focus_pane(Pane::Detail, window, cx);
                }
                view.sync_panes(cx);
                view.reader.update(cx, |reader, cx| match initial_view {
                    InitialView::ThreadExpanded(_) => reader.expand_all(cx),
                    InitialView::ThreadSelection(_) => reader.select_paragraph(1, cx),
                    InitialView::ThreadLinkPrompt(_) => reader.open_first_link(window, cx),
                    _ => {}
                });
            }
            InitialView::AccountSetup => view.open_account_setup(window, cx),
            InitialView::Sidebar => view.focus_pane(Pane::Sidebar, window, cx),
            InitialView::MultiSelect => {
                if let Some(selection) = view.mailbox_selection()
                    && let Ok(threads) = view.store.list_selection_threads(&selection, 0..3)
                {
                    let ids: Vec<i64> = threads.iter().map(|thread| thread.id).collect();
                    view.selected_thread_id = ids.first().copied();
                    view.set_list_selection(ids, cx);
                }
            }
            InitialView::Outbox => view.show_outbox(&ShowOutbox, window, cx),
            InitialView::Search(query) => {
                view.search_query
                    .update(cx, |input, cx| input.set_text(query, cx));
                view.search_on_open(cx);
                view.focus_pane(Pane::MessageList, window, cx);
            }
        }
        view.sync_panes(cx);

        // New mail synced in the background by the mail watcher appears
        // without user interaction; the refresh reads local state only.
        // A weak handle: strong `Entity::update` no longer reports a released
        // entity, so the weak update's error is what ends the loop.
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(5)).await;
                    let refreshed = entity.update(cx, |this, cx| {
                        this.refresh_from_store(cx);
                        cx.notify();
                    });
                    if refreshed.is_err() {
                        break;
                    }
                }
            })
            .detach();

        // Relative dates ("10:42", "Yesterday") change at midnight even when
        // nothing else does, so redraw every pane at each local midnight.
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                loop {
                    let now = chrono::Local::now();
                    let until_midnight = (next_local_midnight(&now) - now)
                        .to_std()
                        .unwrap_or(Duration::from_secs(60));
                    cx.background_executor()
                        .timer(until_midnight + Duration::from_secs(1))
                        .await;
                    if entity.upgrade().is_none()
                        || cx.update(|window, _| window.refresh()).is_err()
                    {
                        break;
                    }
                }
            })
            .detach();

        view
    }

    /// The lowercased addresses of the selected account's identities, used
    /// to show the user as "me" in participant lists.
    pub(crate) fn own_addresses(&self) -> Vec<String> {
        self.identities
            .iter()
            .map(|identity| identity.email_address.trim().to_ascii_lowercase())
            .filter(|address| !address.is_empty())
            .collect()
    }

    // -----------------------------------------------------------------------
    // Pushing state into the panes
    // -----------------------------------------------------------------------

    /// Pushes the current state into every pane. Each pane re-renders only
    /// if what it shows changed. Runs after every shell notification.
    pub(crate) fn sync_panes(&mut self, cx: &mut Context<Self>) {
        let sidebar_state = SidebarState {
            accounts: self.accounts.clone(),
            mailboxes: self.all_mailboxes.clone(),
            highlight: match self.list_mode {
                ListMode::Mail => match self.mailbox_selection() {
                    Some(MailboxSelection::Virtual(virtual_mailbox)) => {
                        SidebarHighlight::Virtual(virtual_mailbox)
                    }
                    Some(MailboxSelection::Mailbox { mailbox_id, .. }) => {
                        SidebarHighlight::Mailbox(mailbox_id)
                    }
                    None => SidebarHighlight::None,
                },
                ListMode::Drafts => SidebarHighlight::Drafts,
                ListMode::Outbox => SidebarHighlight::Outbox,
                ListMode::Search => SidebarHighlight::None,
            },
            flagged_unread: self.flagged_unread,
            draft_count: self.drafts.len(),
            outbox: OutboxStatus {
                waiting: self
                    .outbox_jobs
                    .iter()
                    .filter(|job| job.state != OutboxJobState::Sent)
                    .count(),
                failed: self
                    .outbox_jobs
                    .iter()
                    .filter(|job| is_retryable(job.state))
                    .count(),
            },
            tree: self.sidebar_tree.clone(),
            density: self.density,
        };
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_state(sidebar_state, cx));

        let own_addresses = self.own_addresses();
        let list_state = MessageListState {
            mode: self.list_mode,
            has_accounts: !self.accounts.is_empty(),
            mailbox: self.list_mailbox(),
            accounts: self
                .accounts
                .iter()
                .map(|account| AccountLabel {
                    id: account.id,
                    name: account.display_name.clone(),
                    color: account.color(),
                })
                .collect(),
            own_addresses: own_addresses.clone(),
            selected_thread_id: self.selected_thread_id,
            search_results: self.search_results.clone(),
            search_query: self.search_results_query.clone(),
            searching_server: self.search_context.servers_pending > 0,
            search_scope: self.search_scope_state(),
            drafts: self.drafts.clone(),
            selected_draft_id: self.selected_draft_id,
            outbox_jobs: self.outbox_jobs.clone(),
            selected_outbox_job_id: self.selected_outbox_job_id,
            density: self.density,
            selection: ListSelection {
                account_id: if self.list_selection.is_multiple() {
                    self.list_selection.account_id
                } else {
                    self.selected_account_id
                },
                ..self.list_selection.clone()
            },
            mailboxes: self.all_mailboxes.clone(),
            first_sync: self.list_first_sync(),
        };
        self.message_list
            .update(cx, |list, cx| list.set_state(list_state, cx));

        let selected_count = self.list_selection.thread_ids.len();
        let reader_state = ReaderState {
            // Several selected conversations show as a count instead.
            thread: if selected_count > 1 {
                None
            } else {
                self.selected_thread_view()
            },
            selected_count,
            mailboxes: self.mailboxes.clone(),
            own_addresses,
            compact_actions: self.detail_width() < READER_COMPACT_ACTIONS_BELOW,
            reply: self
                .inline_reply
                .as_ref()
                .filter(|reply| Some(reply.thread_id) == self.selected_thread_id)
                .map(|reply| reply.compose.clone()),
        };
        self.reader
            .update(cx, |reader, cx| reader.set_state(reader_state, cx));
    }

    // -----------------------------------------------------------------------
    // Store refresh
    // -----------------------------------------------------------------------

    /// Re-read all relevant state from the local store.
    ///
    /// Clears `refresh_scheduled` so the debounce guard is reset.
    pub(crate) fn refresh_from_store(&mut self, cx: &mut Context<Self>) {
        self.refresh_scheduled = false;
        // The selected thread may have changed (hydrated bodies, flags,
        // read state); it is reloaded once, on the next pane sync.
        self.thread_view_cache = None;
        match self.store.list_accounts() {
            Ok(accounts) => {
                self.accounts = accounts;
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

        self.all_mailboxes = match self.store.list_all_mailboxes() {
            Ok(mailboxes) => mailboxes,
            Err(error) => {
                self.store_error = Some(error.to_string());
                Vec::new()
            }
        };

        self.selected_account_id = self
            .selected_account_id
            .filter(|selected_id| {
                self.accounts
                    .iter()
                    .any(|account| account.id == *selected_id)
            })
            .or_else(|| self.accounts.first().map(|account| account.id));

        // A virtual mailbox acts on the open thread's account and mailbox.
        if let Some(virtual_mailbox) = self.virtual_mailbox {
            let selection = MailboxSelection::Virtual(virtual_mailbox);
            self.virtual_counts = self.store.selection_counts(&selection).unwrap_or_default();
            let rows = self
                .store
                .list_selection_threads(&selection, 0..200)
                .unwrap_or_default();
            let open = self
                .selected_thread_id
                .and_then(|selected| rows.iter().find(|row| row.id == selected))
                .or_else(|| rows.first());
            self.selected_thread_id = open.map(|row| row.id);
            if let Some(row) = open {
                self.selected_account_id = Some(row.account_id);
                self.selected_mailbox_id = Some(row.mailbox_id);
            }
        }

        self.account = self
            .selected_account_id
            .and_then(|account_id| self.store.account_overview(account_id).ok().flatten());
        self.identities = self
            .selected_account_id
            .and_then(|account_id| self.store.list_identities(account_id).ok())
            .unwrap_or_default();
        self.mailboxes = self
            .all_mailboxes
            .iter()
            .filter(|mailbox| Some(mailbox.account_id) == self.selected_account_id)
            .cloned()
            .collect();
        // Drafts and the outbox are Favourites: every account's.
        self.drafts = self.store.list_drafts(None).unwrap_or_default();
        self.outbox_jobs = self.store.list_outbox_jobs(None).unwrap_or_default();
        self.notice_last_session_sends(cx);
        self.flagged_unread = self
            .store
            .selection_counts(&MailboxSelection::Virtual(VirtualMailbox::Flagged))
            .map(|counts| counts.unread_threads)
            .unwrap_or_default();
        // Re-run the shown search so read state, flags and moves made since
        // it ran show up in the results.
        self.refresh_search_results();
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

        if self.virtual_mailbox.is_none() {
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
        }

        self.refresh_list_selection();

        self.selected_draft_id = self
            .selected_draft_id
            .filter(|selected_id| self.drafts.iter().any(|draft| draft.id == *selected_id))
            .or_else(|| self.drafts.first().map(|draft| draft.id));
        self.selected_outbox_job_id = self
            .selected_outbox_job_id
            .filter(|selected_id| self.outbox_jobs.iter().any(|job| job.id == *selected_id))
            .or_else(|| self.outbox_jobs.first().map(|job| job.id));

        // Only the store-owned parts of the open drafts refresh: the text
        // inputs keep the user's text, cursor and undo history.
        self.compose
            .update(cx, |compose, cx| compose.refresh_from_store(cx));
        for (handle, _) in self.compose_windows.values() {
            let _ = handle.update(cx, |root, _, cx| {
                root.compose()
                    .update(cx, |compose, cx| compose.refresh_from_store(cx))
            });
        }
        // The list's cached page of rows re-reads, and re-renders only if a
        // row changed.
        self.message_list
            .update(cx, |list, cx| list.reload_rows(cx));
    }

    /// Schedule a debounced store refresh 900 ms from now.
    ///
    /// A second call while a refresh is already pending is a no-op.
    pub(crate) fn schedule_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refresh_scheduled {
            return;
        }
        self.refresh_scheduled = true;
        let view = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(900))
                    .await;
                let _ = view.update(cx, |this, cx| {
                    this.refresh_from_store(cx);
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

    /// What the list shows in Mail mode: the virtual mailbox, or the
    /// selected mailbox.
    pub(crate) fn mailbox_selection(&self) -> Option<MailboxSelection> {
        match self.virtual_mailbox {
            Some(virtual_mailbox) => Some(MailboxSelection::Virtual(virtual_mailbox)),
            None => self
                .selected_mailbox()
                .map(|mailbox| MailboxSelection::Mailbox {
                    account_id: mailbox.account_id,
                    mailbox_id: mailbox.id,
                }),
        }
    }

    /// The list's mailbox with its name and counts.
    pub(crate) fn list_mailbox(&self) -> Option<ListMailbox> {
        match self.virtual_mailbox {
            Some(virtual_mailbox) => Some(ListMailbox {
                selection: MailboxSelection::Virtual(virtual_mailbox),
                name: virtual_mailbox.label().to_owned(),
                role: None,
                total_threads: self.virtual_counts.total_threads,
                unread_threads: self.virtual_counts.unread_threads,
            }),
            None => self.selected_mailbox().map(|mailbox| ListMailbox {
                selection: MailboxSelection::Mailbox {
                    account_id: mailbox.account_id,
                    mailbox_id: mailbox.id,
                },
                name: mailbox.leaf_name().to_owned(),
                role: mailbox.role,
                total_threads: mailbox.total_threads,
                unread_threads: mailbox.unread_threads,
            }),
        }
    }

    /// Whether an account the list shows is in its first sync: syncing
    /// and never synced before. Its empty list is still filling.
    fn list_first_sync(&self) -> bool {
        self.accounts
            .iter()
            .filter(|account| {
                self.virtual_mailbox.is_some() || Some(account.id) == self.selected_account_id
            })
            .any(|account| {
                account.sync_status.last_success_at.is_none()
                    && matches!(
                        account.sync_status.phase,
                        SyncPhase::Connecting | SyncPhase::Discovering | SyncPhase::Syncing
                    )
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

    /// The selected thread, loaded from the store once per selection or
    /// refresh.
    ///
    /// Sets `store_error` on failure.  This takes `&mut self` because it
    /// fills the cache and records the error.
    pub(crate) fn selected_thread_view(&mut self) -> Option<ThreadView> {
        let thread_id = self.selected_thread_id?;
        if let Some(cached) = self
            .thread_view_cache
            .as_ref()
            .filter(|cached| cached.id == thread_id)
        {
            return Some(cached.clone());
        }
        match self.store.thread_view(thread_id) {
            Ok(thread) => {
                self.thread_view_cache = thread.clone();
                thread
            }
            Err(error) => {
                self.store_error = Some(error.to_string());
                None
            }
        }
    }

    /// The thread the conversation commands act on: the selected thread,
    /// while the reader shows it.
    fn command_thread_id(&self) -> Option<i64> {
        matches!(self.detail_mode, DetailMode::Reader)
            .then_some(self.selected_thread_id)
            .flatten()
    }

    /// Show a status message that dismisses itself after
    /// [`STATUS_TOAST_DURATION`]. A newer message restarts the timer.
    pub(crate) fn show_status(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.status_message = Some(message.into());
        self.status_generation = self.status_generation.wrapping_add(1);
        let generation = self.status_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STATUS_TOAST_DURATION).await;
            let _ = this.update(cx, |view, cx| {
                if view.status_generation == generation {
                    view.status_message = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Dismiss the error banner. Errors stay until the user does this.
    pub(crate) fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.store_error = None;
        cx.notify();
    }

    /// Records an error for the banner.
    pub(crate) fn show_error(&mut self, error: impl ToString, cx: &mut Context<Self>) {
        self.store_error = Some(error.to_string());
        cx.notify();
    }

    /// Marks the thread read once it has stayed open for
    /// [`MARK_READ_DELAY`], through the same local-first store path (and
    /// write-back queue) as the Mark Read button.
    fn schedule_mark_read(&mut self, thread_id: i64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MARK_READ_DELAY).await;
            let _ = this.update(cx, |view, cx| {
                view.mark_read_if_still_open(thread_id, cx);
            });
        })
        .detach();
    }

    fn mark_read_if_still_open(&mut self, thread_id: i64, cx: &mut Context<Self>) {
        if self.selected_thread_id != Some(thread_id)
            || !matches!(self.detail_mode, DetailMode::Reader)
        {
            return;
        }
        let has_unread = self
            .selected_thread_view()
            .is_some_and(|thread| thread.unread_count > 0);
        if !has_unread {
            return;
        }
        if let Err(error) = self.mail_actions.update(cx, |actions, cx| {
            actions.mark_opened_thread_read(thread_id, cx)
        }) {
            self.show_error(error, cx);
        }
    }

    // -----------------------------------------------------------------------
    // Layout
    // -----------------------------------------------------------------------

    /// The pane layout for the current window width and the user's pane
    /// widths.
    pub(crate) fn pane_layout(&self) -> PaneLayout {
        PaneLayout::compute(
            self.viewport_width,
            self.sidebar_width,
            self.list_width,
            self.sidebar_collapsed,
        )
    }

    /// Whether the sidebar is showing, as a pane or as an overlay.
    pub(crate) fn sidebar_shown(&self) -> bool {
        self.pane_layout().sidebar.is_some() || self.sidebar_overlay_open
    }

    /// The detail pane's width as laid out.
    pub(crate) fn detail_width(&self) -> f32 {
        self.pane_layout().detail
    }

    /// Follows a window resize: the sidebar overlay closes once the
    /// sidebar is a pane again (or opens if the sidebar had focus when it
    /// stopped being one), and entering the single-pane layout shows
    /// whichever pane had focus.
    fn on_window_resized(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        if width == self.viewport_width {
            return;
        }
        let before = self.pane_layout();
        self.viewport_width = width;
        let after = self.pane_layout();
        let focused = self.focused_pane(window, cx);
        if after.sidebar.is_some() {
            self.sidebar_overlay_open = false;
        } else if before.sidebar.is_some() && focused == Some(Pane::Sidebar) {
            self.sidebar_overlay_open = true;
        }
        if after.mode == LayoutMode::Single && before.mode != LayoutMode::Single {
            match focused {
                Some(Pane::Detail) => self.single_pane = SinglePane::Detail,
                Some(Pane::MessageList) => self.single_pane = SinglePane::List,
                Some(Pane::Sidebar) | None => {}
            }
        }
        cx.notify();
    }

    /// Show or hide the sidebar. In the wide layout this hides the pane
    /// and remembers the choice; in the narrower ones it opens or closes
    /// the overlay.
    fn toggle_sidebar(&mut self, _: &ToggleSidebar, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane_layout().mode == LayoutMode::Wide && !self.sidebar_overlay_open {
            let hiding = !self.sidebar_collapsed;
            if hiding && self.focused_pane(window, cx) == Some(Pane::Sidebar) {
                self.focus_pane(Pane::MessageList, window, cx);
            }
            self.sidebar_collapsed = hiding;
            self.persist_sidebar_collapsed();
        } else if self.sidebar_overlay_open {
            self.close_sidebar_overlay(window, cx);
        } else {
            // Focusing the sidebar opens it as an overlay.
            self.focus_pane(Pane::Sidebar, window, cx);
        }
        cx.notify();
    }

    /// Closes the sidebar overlay, moving focus to the list if it was in
    /// the sidebar.
    fn close_sidebar_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_overlay_open {
            return;
        }
        if self.focused_pane(window, cx) == Some(Pane::Sidebar) {
            self.focus_pane(Pane::MessageList, window, cx);
        }
        self.sidebar_overlay_open = false;
        cx.notify();
    }

    /// Back, in the single-pane layout: from the detail pane to the list.
    fn go_back(&mut self, _: &GoBack, window: &mut Window, cx: &mut Context<Self>) {
        self.go_back_to_list(window, cx);
    }

    /// Returns whether there was somewhere to go back to.
    fn go_back_to_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.pane_layout().mode != LayoutMode::Single || self.single_pane != SinglePane::Detail {
            return false;
        }
        self.focus_pane(Pane::MessageList, window, cx);
        true
    }

    /// Records a dragged sidebar width and saves it shortly after the
    /// drag settles.
    fn resize_sidebar(&mut self, width: f32, cx: &mut Context<Self>) {
        self.sidebar_width = width;
        self.schedule_pane_widths_save(cx);
        cx.notify();
    }

    /// Records a dragged list width and saves it shortly after the drag
    /// settles.
    fn resize_list(&mut self, width: f32, cx: &mut Context<Self>) {
        self.list_width = width;
        self.schedule_pane_widths_save(cx);
        cx.notify();
    }

    /// Saves the pane widths [`PANE_WIDTHS_SAVE_DELAY`] after the last
    /// change, so a drag writes the preferences once, not per pixel.
    fn schedule_pane_widths_save(&mut self, cx: &mut Context<Self>) {
        self.pane_widths_generation = self.pane_widths_generation.wrapping_add(1);
        let generation = self.pane_widths_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PANE_WIDTHS_SAVE_DELAY).await;
            let _ = this.update(cx, |view, cx| {
                if view.pane_widths_generation == generation {
                    view.persist_pane_widths();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The list pane's title: the mailbox name, Search, Drafts or Outbox.
    fn list_title(&self) -> String {
        match self.list_mode {
            ListMode::Mail => self
                .list_mailbox()
                .map(|mailbox| mailbox.name)
                .unwrap_or_else(|| "Mail".to_owned()),
            ListMode::Search => "Search".to_owned(),
            ListMode::Drafts => "Drafts".to_owned(),
            ListMode::Outbox => "Outbox".to_owned(),
        }
    }

    // -----------------------------------------------------------------------
    // Theme
    // -----------------------------------------------------------------------

    /// Switch to the bundled theme variant that matches the system
    /// appearance, if it is not active already. Returns whether it changed.
    pub(crate) fn follow_system_appearance(&mut self, appearance: WindowAppearance) -> bool {
        let wanted = match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
        };
        if self.bootstrap.theme.appearance != wanted
            && self.bootstrap.alternate_theme.appearance == wanted
        {
            std::mem::swap(
                &mut self.bootstrap.theme,
                &mut self.bootstrap.alternate_theme,
            );
            true
        } else {
            false
        }
    }

    /// Publishes the active theme's colours and redraws every pane: panes
    /// are cached, so a colour change has to bypass their caches.
    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        components::ComponentTheme::sync(&self.bootstrap.theme, cx);
        cx.set_global(UiColors::from_theme(&self.bootstrap.theme));
        window.refresh();
        cx.notify();
    }

    /// Swap the active theme with the bundled alternate variant.
    fn toggle_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_overridden = true;
        std::mem::swap(
            &mut self.bootstrap.theme,
            &mut self.bootstrap.alternate_theme,
        );
        self.persist_theme_choice(window.appearance());
        self.apply_theme(window, cx);
    }

    // -----------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------

    /// Switch the active account and refresh the view.
    pub(crate) fn select_account(&mut self, account_id: i64, cx: &mut Context<Self>) {
        self.selected_account_id = Some(account_id);
        self.virtual_mailbox = None;
        self.list_selection = ListSelection::default();
        self.list_mail(cx);
        if matches!(self.detail_mode, DetailMode::AccountSetup) {
            self.detail_mode = DetailMode::Reader;
        }
        self.refresh_from_store(cx);
        cx.notify();
    }

    /// Switch the active mailbox (of any account) and select its first
    /// thread.
    pub(crate) fn select_mailbox(&mut self, mailbox_id: i64, cx: &mut Context<Self>) {
        let was_virtual = self.virtual_mailbox.take().is_some();
        let account_id = self.mailbox_account_id(mailbox_id);
        let account_changed = account_id.is_some() && account_id != self.selected_account_id;
        if account_changed {
            self.selected_account_id = account_id;
        }
        self.selected_mailbox_id = Some(mailbox_id);
        self.list_selection = ListSelection::default();
        self.selected_thread_id = match self.store.first_thread_id(mailbox_id) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                self.store_error = Some(error.to_string());
                None
            }
        };
        self.list_mail(cx);
        self.detail_mode = DetailMode::Reader;
        if account_changed || was_virtual {
            self.refresh_from_store(cx);
        }
        self.persist_last_mailbox();
        cx.notify();
    }

    /// Show a virtual mailbox (the Unified Inbox, Flagged, Sent) and
    /// select its newest thread.
    pub(crate) fn select_virtual_mailbox(
        &mut self,
        virtual_mailbox: VirtualMailbox,
        cx: &mut Context<Self>,
    ) {
        self.virtual_mailbox = Some(virtual_mailbox);
        self.selected_thread_id = None;
        self.list_mail(cx);
        self.detail_mode = DetailMode::Reader;
        self.refresh_from_store(cx);
        self.persist_last_mailbox();
        cx.notify();
    }

    /// Reopens the mailbox saved in the UI preferences, if it still
    /// exists.
    fn restore_last_mailbox(&mut self, cx: &mut Context<Self>) {
        let Ok(Some(last)) = self
            .store
            .ui_preferences()
            .map(|preferences| preferences.last_mailbox)
        else {
            return;
        };
        match last {
            MailboxSelection::Virtual(virtual_mailbox) => {
                self.select_virtual_mailbox(virtual_mailbox, cx)
            }
            MailboxSelection::Mailbox { mailbox_id, .. } => {
                if self.mailbox_account_id(mailbox_id).is_some() {
                    self.select_mailbox(mailbox_id, cx);
                }
            }
        }
    }

    /// The account a stored mailbox belongs to.
    fn mailbox_account_id(&self, mailbox_id: i64) -> Option<i64> {
        self.all_mailboxes
            .iter()
            .find(|mailbox| mailbox.id == mailbox_id)
            .map(|mailbox| mailbox.account_id)
    }

    /// Select a specific thread (from the list or the search results) and
    /// mark it read once it has stayed open for a moment.
    pub(crate) fn select_thread(
        &mut self,
        thread_id: i64,
        mailbox_id: i64,
        cx: &mut Context<Self>,
    ) {
        self.show_thread(thread_id, mailbox_id, cx);
        self.schedule_mark_read(thread_id, cx);
    }

    /// Shows a thread in the list and reader without marking it read, for
    /// example when the window opens on a given thread.
    pub(crate) fn show_thread(&mut self, thread_id: i64, mailbox_id: i64, cx: &mut Context<Self>) {
        // In a virtual mailbox the thread may be another account's: reply
        // identities, moves and body fetches follow it.
        if let Some(account_id) = self.mailbox_account_id(mailbox_id)
            && Some(account_id) != self.selected_account_id
        {
            self.selected_account_id = Some(account_id);
            self.selected_mailbox_id = Some(mailbox_id);
            self.selected_thread_id = Some(thread_id);
            self.refresh_from_store(cx);
        }
        self.selected_mailbox_id = Some(mailbox_id);
        self.selected_thread_id = Some(thread_id);
        self.detail_mode = DetailMode::Reader;
        self.hydrate_selected_thread_bodies(cx);
        cx.notify();
    }

    /// Opens a draft in the compose pane, or, when it is open in a compose
    /// window, shows that in the pane instead (a draft is edited in one
    /// place only).
    pub(crate) fn open_draft(
        &mut self,
        draft_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.release_draft(draft_id, cx);
        if self.compose_window(draft_id, cx).is_some() {
            self.compose
                .update(cx, |compose, cx| compose.show_in_window(draft_id, cx));
            self.selected_draft_id = Some(draft_id);
            self.detail_mode = DetailMode::Compose;
            cx.notify();
            return;
        }
        match self
            .compose
            .update(cx, |compose, cx| compose.load_draft(draft_id, window, cx))
        {
            Ok(()) => {
                self.selected_draft_id = Some(draft_id);
                self.detail_mode = DetailMode::Compose;
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Shows the mailbox in the list, ending a search, without touching
    /// the detail pane or the selection.
    fn list_mail(&mut self, cx: &mut Context<Self>) {
        if self.list_mode == ListMode::Search {
            self.reset_search(cx);
        }
        self.list_mode = ListMode::Mail;
    }

    /// Switch the list panel mode.  Switching to [`ListMode::Mail`] also
    /// resets the detail pane to the reader.
    pub(crate) fn set_list_mode(&mut self, list_mode: ListMode, cx: &mut Context<Self>) {
        if list_mode != self.list_mode {
            self.list_selection = ListSelection::default();
            if self.list_mode == ListMode::Search {
                self.reset_search(cx);
            }
        }
        self.list_mode = list_mode;
        if matches!(list_mode, ListMode::Mail) {
            self.detail_mode = DetailMode::Reader;
        }
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
                view.refresh_from_store(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // -----------------------------------------------------------------------
    // Sync
    // -----------------------------------------------------------------------

    /// Trigger a sync for the currently-selected account.
    pub(crate) fn sync_selected_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.selected_account_id else {
            self.show_error("Add an account first.", cx);
            return;
        };

        let account_name = self
            .selected_account()
            .map(|account| account.display_name.clone())
            .unwrap_or_default();
        match self.sync_service.sync_account(account_id) {
            Ok(_) => {
                self.show_status(format!("Syncing {account_name}…"), cx);
                self.refresh_from_store(cx);
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
                self.show_status(
                    format!("Syncing {}…", count_noun(started, "account", "accounts")),
                    cx,
                );
                self.refresh_from_store(cx);
                self.schedule_refresh(window, cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Compose workflow
    // -----------------------------------------------------------------------

    /// Open the compose pane for a new draft, creating the draft in the store.
    pub(crate) fn open_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.selected_account_id else {
            self.open_account_setup_with_notice(
                Some("Add a mail account first. You can write messages once it is set up.".into()),
                window,
                cx,
            );
            return;
        };
        let Some(identity) = self
            .identities
            .iter()
            .find(|identity| identity.is_primary)
            .or_else(|| self.identities.first())
            .cloned()
        else {
            self.show_error(
                "This account has no sending identity. Add one in the account settings.",
                cx,
            );
            return;
        };

        let draft = NewDraft {
            account_id,
            identity_id: identity.id,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body: String::new(),
        };
        match self
            .mail_actions
            .update(cx, |actions, cx| actions.create_draft(&draft, cx))
        {
            Ok(draft_id) => {
                self.open_draft_in_window(draft_id, false, window, cx);
                self.refresh_from_store(cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Creates an empty draft and opens it in the compose pane, as the
    /// drafts list does, for tests of the pane.
    #[cfg(test)]
    pub(crate) fn open_compose_in_pane_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> i64 {
        let identity = self.default_compose_identity().expect("an identity");
        let account_id = self.selected_account_id.expect("an account");
        let draft_id = self
            .mail_actions
            .update(cx, |actions, cx| {
                actions.create_draft(
                    &NewDraft {
                        account_id,
                        identity_id: identity.id,
                        to: Vec::new(),
                        cc: Vec::new(),
                        bcc: Vec::new(),
                        subject: String::new(),
                        body: String::new(),
                    },
                    cx,
                )
            })
            .expect("the draft is created");
        self.open_draft(draft_id, window, cx);
        draft_id
    }

    /// The compose window editing `draft_id`, if one is open.
    pub(crate) fn compose_window(
        &mut self,
        draft_id: i64,
        cx: &mut App,
    ) -> Option<gpui::WindowHandle<ComposeWindow>> {
        let (handle, _) = self.compose_windows.get(&draft_id)?;
        let handle = *handle;
        if handle.update(cx, |_, _, _| ()).is_ok() {
            Some(handle)
        } else {
            self.compose_windows.remove(&draft_id);
            None
        }
    }

    /// Opens `draft_id` in a compose window of its own, or brings its window
    /// forward when it already has one. Focus goes to To, or to the start
    /// of the body when `focus_body` is set.
    pub(crate) fn open_draft_in_window(
        &mut self,
        draft_id: i64,
        focus_body: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = self.compose_window(draft_id, cx) {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            return;
        }
        // A draft is edited in one place: the pane and the inline reply let
        // go of it first.
        self.release_draft(draft_id, cx);
        let in_pane = self.compose.read(cx).draft_id() == Some(draft_id);
        if in_pane {
            self.compose
                .update(cx, |compose, cx| compose.show_in_window(draft_id, cx));
        }
        let opened = ComposeWindow::open(
            draft_id,
            focus_body,
            self.store.clone(),
            self.sync_service.clone(),
            self.mail_actions.clone(),
            cx,
        )
        .and_then(|handle| {
            let root = handle.entity(cx)?;
            let compose = root.read(cx).compose().clone();
            Ok((handle, root, compose))
        });
        match opened {
            Ok((handle, root, compose)) => {
                let subscriptions = [
                    cx.subscribe(&compose, |this, _, event: &ComposeEvent, cx| {
                        this.on_window_compose_event(event, cx)
                    }),
                    // The window closed: the pane may edit the draft again.
                    cx.observe_release_in(&root, window, move |this, _, window, cx| {
                        this.compose_windows.remove(&draft_id);
                        if this.compose.read(cx).showing_in_window() == Some(draft_id) {
                            this.open_draft(draft_id, window, cx);
                        }
                        this.refresh_from_store(cx);
                        cx.notify();
                    }),
                ];
                self.compose_windows
                    .insert(draft_id, (handle, subscriptions));
            }
            Err(error) => self.show_error(error.to_string(), cx),
        }
    }

    /// Events from a compose window's editor. Its status and errors show in
    /// that window; once the draft is queued (and the window closes), the
    /// Undo Send toast shows here.
    fn on_window_compose_event(&mut self, event: &ComposeEvent, cx: &mut Context<Self>) {
        if let &ComposeEvent::Queued {
            draft_id,
            job_id,
            undo_delay,
            ..
        } = event
        {
            self.show_undo_send_toast(draft_id, job_id, undo_delay, ReopenIn::Window, cx);
            self.refresh_from_store(cx);
            cx.notify();
        }
    }

    /// The toast for a message waiting out its undo-send window, with
    /// Undo. It stays exactly as long as the window, so Undo is offered
    /// until the message leaves. With no window, a plain status says the
    /// message is on its way.
    fn show_undo_send_toast(
        &mut self,
        draft_id: i64,
        job_id: i64,
        undo_delay: Duration,
        reopen: ReopenIn,
        cx: &mut Context<Self>,
    ) {
        if undo_delay.is_zero() {
            self.show_status("Message queued for sending.".to_owned(), cx);
            return;
        }
        let workspace = cx.entity().downgrade();
        let toast =
            Toast::new("Sending message…")
                .duration(undo_delay)
                .on_undo(move |window, cx| {
                    // The toast host is mid-update while this runs; undo once
                    // it is done, in the main window.
                    let workspace = workspace.clone();
                    let main_window = window.window_handle();
                    cx.defer(move |cx| {
                        let _ = main_window.update(cx, |_, window, cx| {
                            workspace.update(cx, |this, cx| {
                                this.undo_send(draft_id, job_id, reopen, window, cx)
                            })
                        });
                    });
                });
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(toast, cx);
        });
    }

    /// Undo Send: takes the message back and reopens its draft where it
    /// was written.
    fn undo_send(
        &mut self,
        draft_id: i64,
        job_id: i64,
        reopen: ReopenIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self
            .mail_actions
            .update(cx, |actions, cx| actions.cancel_send(job_id, draft_id, cx))
        {
            Ok(true) => {
                match reopen {
                    // A reply goes back below its conversation if that is
                    // still the one showing, else into a window.
                    ReopenIn::Reply(thread_id) if self.selected_thread_id == Some(thread_id) => {
                        self.open_inline_reply(thread_id, draft_id, window, cx);
                    }
                    ReopenIn::Window | ReopenIn::Reply(_) => {
                        self.open_draft_in_window(draft_id, false, window, cx);
                    }
                    ReopenIn::Pane => {
                        self.open_draft(draft_id, window, cx);
                        self.compose
                            .update(cx, |compose, cx| compose.focus_pane(window, cx));
                    }
                }
                self.toasts.update(cx, |toasts, cx| {
                    toasts.push(Toast::new("Sending undone"), cx);
                });
            }
            Ok(false) => self.toasts.update(cx, |toasts, cx| {
                toasts.push(
                    Toast::new("Too late to undo: the message has started sending.")
                        .kind(ToastKind::Error),
                    cx,
                );
            }),
            Err(error) => self.show_error(error.to_string(), cx),
        }
    }

    /// Open a reply or reply-all draft for the selected thread.
    /// Opens a reply draft to the selected thread's message `message_id`,
    /// or to its latest message.
    pub(crate) fn reply_to_selected_thread(
        &mut self,
        reply_all: bool,
        message_id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.selected_thread_view() else {
            self.store_error
                .get_or_insert_with(|| "Select a thread first.".to_owned());
            cx.notify();
            return;
        };

        match self.create_reply_draft(&thread, message_id, reply_all, cx) {
            Ok(draft_id) => {
                // The reply opens below the conversation, in the reader.
                self.detail_mode = DetailMode::Reader;
                self.single_pane = SinglePane::Detail;
                self.open_inline_reply(thread.id, draft_id, window, cx);
                self.refresh_from_store(cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Opens `draft_id` as the inline reply below conversation `thread_id`,
    /// with the cursor at the start of the body, above the quoted message.
    /// Only one conversation has an inline reply at a time; opening another
    /// saves the previous one, which stays in Drafts.
    pub(crate) fn open_inline_reply(
        &mut self,
        thread_id: i64,
        draft_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_inline_reply(cx);
        self.release_draft(draft_id, cx);
        let compose = cx.new(|cx| {
            ComposeView::new(
                self.store.clone(),
                self.sync_service.clone(),
                self.mail_actions.clone(),
                Presentation::Inline,
                cx,
            )
        });
        let loaded = compose.update(cx, |compose, cx| {
            let loaded = compose.load_draft(draft_id, window, cx);
            compose.focus_body_start(window, cx);
            loaded
        });
        if let Err(error) = loaded {
            self.show_error(error.to_string(), cx);
            return;
        }
        let subscription = cx.subscribe_in(&compose, window, move |this, _, event, window, cx| {
            this.on_inline_reply_event(thread_id, event, window, cx)
        });
        self.inline_reply = Some(InlineReply {
            thread_id,
            compose,
            _subscription: subscription,
        });
        self.sync_panes(cx);
        self.reader.update(cx, |reader, cx| reader.reveal_reply(cx));
        cx.notify();
    }

    /// Saves and closes the inline reply, if one is open.
    pub(crate) fn close_inline_reply(&mut self, cx: &mut Context<Self>) {
        if let Some(reply) = self.inline_reply.take() {
            reply
                .compose
                .update(cx, |compose, cx| compose.flush_autosave(cx));
            cx.notify();
        }
    }

    /// A draft is edited in one place: before the pane or a window opens
    /// `draft_id`, the inline reply lets go of it.
    fn release_draft(&mut self, draft_id: i64, cx: &mut Context<Self>) {
        if self
            .inline_reply
            .as_ref()
            .is_some_and(|reply| reply.compose.read(cx).draft_id() == Some(draft_id))
        {
            self.close_inline_reply(cx);
        }
    }

    fn on_inline_reply_event(
        &mut self,
        thread_id: i64,
        event: &ComposeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match *event {
            ComposeEvent::Status(ref message) => self.show_status(message.clone(), cx),
            ComposeEvent::Error(ref error) => self.show_error(error, cx),
            ComposeEvent::Queued {
                draft_id,
                job_id,
                undo_delay,
                ..
            } => {
                self.inline_reply = None;
                self.show_undo_send_toast(
                    draft_id,
                    job_id,
                    undo_delay,
                    ReopenIn::Reply(thread_id),
                    cx,
                );
                self.refresh_from_store(cx);
                cx.notify();
            }
            // The pop-out button: the reply moves into its own window.
            ComposeEvent::PopOut { draft_id } => {
                self.close_inline_reply(cx);
                self.open_draft_in_window(draft_id, true, window, cx);
            }
            ComposeEvent::ShowWindow { .. } => {}
        }
    }

    /// Open a forward draft for the selected thread.
    pub(crate) fn forward_selected_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.selected_thread_view() else {
            self.store_error
                .get_or_insert_with(|| "Select a thread first.".to_owned());
            cx.notify();
            return;
        };

        match self.create_forward_draft(&thread, cx) {
            Ok(draft_id) => {
                self.open_draft_in_window(draft_id, false, window, cx);
                self.refresh_from_store(cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn create_reply_draft(
        &self,
        thread: &ThreadView,
        message_id: Option<i64>,
        reply_all: bool,
        cx: &mut Context<Self>,
    ) -> Result<i64> {
        let latest_message = match message_id {
            Some(message_id) => thread
                .messages
                .iter()
                .find(|message| message.id == message_id)
                .context("the message to reply to is no longer in the thread")?,
            None => thread
                .messages
                .last()
                .context("selected thread has no messages to reply to")?,
        };
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
            quoted_reply_body(latest_message, self.hour_cycle),
            cx,
        )
    }

    fn create_forward_draft(&self, thread: &ThreadView, cx: &mut Context<Self>) -> Result<i64> {
        let latest_message = thread
            .messages
            .last()
            .context("selected thread has no messages to forward")?;

        self.create_thread_draft(
            Vec::new(),
            Vec::new(),
            prefixed_subject("Fwd", &thread.subject),
            forwarded_body(latest_message, self.hour_cycle),
            cx,
        )
    }

    fn create_thread_draft(
        &self,
        to: Vec<String>,
        cc: Vec<String>,
        subject: String,
        body: String,
        cx: &mut Context<Self>,
    ) -> Result<i64> {
        let account_id = self
            .selected_account_id
            .context("select an account before composing")?;
        let identity = self.default_compose_identity()?;
        let draft = NewDraft {
            account_id,
            identity_id: identity.id,
            to,
            cc,
            bcc: Vec::new(),
            subject,
            body,
        };
        self.mail_actions
            .update(cx, |actions, cx| actions.create_draft(&draft, cx))
    }

    fn default_compose_identity(&self) -> Result<IdentitySummary> {
        self.identities
            .iter()
            .find(|identity| identity.is_primary)
            .or_else(|| self.identities.first())
            .cloned()
            .context("select or configure an account identity before composing")
    }

    /// Sends every failed outbox message again (the sidebar's Retry).
    pub(crate) fn retry_failed_outbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let failed = self
            .outbox_jobs
            .iter()
            .filter(|job| is_retryable(job.state))
            .map(|job| job.id)
            .collect::<Vec<_>>();
        if failed.is_empty() {
            return;
        }
        for job_id in &failed {
            if let Err(error) = self
                .mail_actions
                .update(cx, |actions, cx| actions.retry_outbox_job(*job_id, cx))
            {
                self.store_error = Some(error.to_string());
            }
        }
        let _ = self.sync_service.process_outbox(None);
        self.show_status(
            format!(
                "{} queued for another send attempt.",
                count_noun(failed.len(), "message", "messages")
            ),
            cx,
        );
        self.refresh_from_store(cx);
        self.schedule_refresh(window, cx);
        cx.notify();
    }

    /// Expands or collapses an account or folder in the sidebar and saves
    /// the tree.
    fn toggle_sidebar_node(&mut self, node: &TreeNode, cx: &mut Context<Self>) {
        fn toggle<T: PartialEq>(list: &mut Vec<T>, value: T) {
            match list.iter().position(|item| *item == value) {
                Some(index) => {
                    list.remove(index);
                }
                None => list.push(value),
            }
        }
        match node {
            TreeNode::Account(account_id) => {
                toggle(&mut self.sidebar_tree.collapsed_accounts, *account_id)
            }
            TreeNode::Folder(folder) => {
                toggle(&mut self.sidebar_tree.collapsed_folders, folder.clone())
            }
        }
        self.persist_sidebar_tree();
        cx.notify();
    }

    /// Retry the currently-selected outbox job.
    pub(crate) fn retry_selected_outbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job_id) = self.selected_outbox_job_id else {
            self.show_error("Select a message in the outbox first.", cx);
            return;
        };

        match self
            .mail_actions
            .update(cx, |actions, cx| actions.retry_outbox_job(job_id, cx))
        {
            Ok(()) => {
                let _ = self.sync_service.process_outbox(self.selected_account_id);
                self.show_status("Message queued for another send attempt.".to_owned(), cx);
                self.refresh_from_store(cx);
                self.schedule_refresh(window, cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Account setup
    // -----------------------------------------------------------------------

    /// The "Fix…" action on an account whose sync fails: open the account
    /// form with the error. There is no Settings → Accounts page yet
    /// (#120), so this is the form that exists.
    fn fix_account(&mut self, account_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self
            .accounts
            .iter()
            .find(|account| account.id == account_id)
        else {
            return;
        };
        let error = account
            .sync_status
            .last_error_message
            .clone()
            .unwrap_or_else(|| "the last sync failed".to_owned());
        let notice = format!("{} can't sync: {error}", account.display_name);
        self.open_account_setup_with_notice(Some(notice.into()), window, cx);
    }

    /// Switch to the account setup pane.
    pub(crate) fn open_account_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_account_setup_with_notice(None, window, cx);
    }

    /// Switch to the account setup pane, explaining why it opened when the
    /// user did not ask for it.
    pub(crate) fn open_account_setup_with_notice(
        &mut self,
        notice: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.detail_mode = DetailMode::AccountSetup;
        self.single_pane = SinglePane::Detail;
        self.account_setup
            .update(cx, |form, cx| form.open(notice, window, cx));
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Conversation commands
    // -----------------------------------------------------------------------

    /// The conversations the commands act on: the selected conversation,
    /// while the reader shows it.
    fn command_thread_ids(&self) -> Vec<i64> {
        if self.list_selection.is_multiple() && matches!(self.detail_mode, DetailMode::Reader) {
            return self.list_selection.thread_ids.clone();
        }
        self.command_thread_id().into_iter().collect()
    }

    /// Makes `thread_ids` the multi-row selection (or clears it, given
    /// fewer than two) and reads their flag and unread state.
    fn set_list_selection(&mut self, thread_ids: Vec<i64>, cx: &mut Context<Self>) {
        self.list_selection.thread_ids = if thread_ids.len() > 1 {
            thread_ids
        } else {
            Vec::new()
        };
        self.refresh_list_selection();
        cx.notify();
    }

    /// Drops selected threads that left the list (moved out of the shown
    /// mailbox, or gone) and re-reads the selection's flag and unread
    /// state.
    fn refresh_list_selection(&mut self) {
        if self.list_selection.thread_ids.is_empty() {
            self.list_selection = ListSelection::default();
            return;
        }
        let shown = (self.list_mode == ListMode::Mail)
            .then(|| self.mailbox_selection())
            .flatten();
        let listed = match self
            .store
            .thread_marks(&self.list_selection.thread_ids, shown.as_ref())
        {
            Ok(marks) => marks,
            Err(error) => {
                self.store_error = Some(error.to_string());
                return;
            }
        };
        self.list_selection
            .thread_ids
            .retain(|id| listed.iter().any(|mark| mark.id == *id));
        if self.list_selection.thread_ids.len() < 2 {
            self.list_selection = ListSelection::default();
            return;
        }
        self.list_selection.all_flagged = listed.iter().all(|mark| mark.is_flagged);
        self.list_selection.any_unread = listed.iter().any(|mark| mark.is_unread);
        // A cross-account selection (the Unified Inbox) has no one set of
        // folders to move to.
        let account_id = listed[0].account_id;
        self.list_selection.account_id = listed
            .iter()
            .all(|mark| mark.account_id == account_id)
            .then_some(account_id);
    }

    /// Runs a [`MailActions`] change on the conversations the commands act
    /// on. The resulting [`MailEvent`]s refresh the panes. When the change
    /// takes the selected conversation out of the list, the row that takes
    /// its place is selected.
    fn change_threads(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut MailActions, &[i64], &mut Context<MailActions>) -> Result<()>,
    ) -> bool {
        let thread_ids = self.command_thread_ids();
        self.change_these_threads(thread_ids, cx, change)
    }

    /// [`Self::change_threads`] on `thread_ids`, for example conversations
    /// dropped on a sidebar folder.
    fn change_these_threads(
        &mut self,
        thread_ids: Vec<i64>,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut MailActions, &[i64], &mut Context<MailActions>) -> Result<()>,
    ) -> bool {
        if thread_ids.is_empty() {
            return false;
        }
        let selected_row = self
            .message_list
            .update(cx, |list, cx| list.selected_row_index(cx));
        let result = self
            .mail_actions
            .update(cx, |actions, cx| change(actions, &thread_ids, cx));
        if self.list_selection.is_multiple() {
            // The selection keeps the rows still listed; once fewer than
            // two are, the next row is selected as usual.
            self.refresh_list_selection();
            cx.notify();
        }
        if let Some(row) = selected_row
            && !self.list_selection.is_multiple()
        {
            self.select_row_if_selection_left(row, cx);
        }
        match result {
            Ok(()) => true,
            Err(error) => {
                self.show_error(error, cx);
                false
            }
        }
    }

    /// If the selected conversation is no longer in the shown mailbox,
    /// selects the one now at its row `row` (the next one), or the last
    /// one when it was at the end.
    ///
    /// In a virtual mailbox the conversation leaves when it no longer
    /// matches it (archived out of the Unified Inbox, unflagged in
    /// Flagged), and the next row may be another account's.
    fn select_row_if_selection_left(&mut self, row: usize, cx: &mut Context<Self>) {
        let (Some(selection), Some(thread_id)) =
            (self.mailbox_selection(), self.selected_thread_id)
        else {
            return;
        };
        let still_listed = self
            .store
            .selection_contains(&selection, thread_id)
            .unwrap_or(false);
        if self.list_mode != ListMode::Mail || still_listed {
            return;
        }
        let thread_at = |row: usize| {
            self.store
                .list_selection_threads(&selection, row..row + 1)
                .ok()
                .and_then(|threads| threads.into_iter().next())
        };
        let next = thread_at(row).or_else(|| row.checked_sub(1).and_then(thread_at));
        match next {
            Some(next) => self.select_thread(next.id, next.mailbox_id, cx),
            None => {
                self.selected_thread_id = None;
                cx.notify();
            }
        }
    }

    fn archive(&mut self, _: &Archive, _: &mut Window, cx: &mut Context<Self>) {
        self.change_threads(cx, |actions, thread_ids, cx| {
            actions.archive_threads(thread_ids, cx)
        });
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.change_threads(cx, |actions, thread_ids, cx| {
            actions.delete_threads(thread_ids, cx)
        });
    }

    fn toggle_flag(&mut self, _: &ToggleFlag, _: &mut Window, cx: &mut Context<Self>) {
        let flagged = if self.list_selection.is_multiple() {
            self.list_selection.all_flagged
        } else {
            self.selected_thread_view()
                .is_some_and(|thread| thread.is_flagged)
        };
        self.change_threads(cx, |actions, thread_ids, cx| {
            actions.set_threads_flagged(thread_ids, !flagged, cx)
        });
    }

    fn toggle_read(&mut self, _: &ToggleRead, _: &mut Window, cx: &mut Context<Self>) {
        let has_unread = if self.list_selection.is_multiple() {
            self.list_selection.any_unread
        } else {
            self.selected_thread_view()
                .is_some_and(|thread| thread.unread_count > 0)
        };
        self.change_threads(cx, |actions, thread_ids, cx| {
            actions.set_threads_read(thread_ids, has_unread, cx)
        });
    }

    fn move_to_mailbox(&mut self, action: &MoveToMailbox, _: &mut Window, cx: &mut Context<Self>) {
        let mailbox_id = action.mailbox_id;
        self.change_threads(cx, |actions, thread_ids, cx| {
            actions.move_threads(thread_ids, mailbox_id, cx)
        });
    }

    /// Shows "N messages from your last session were sent" once jobs queued
    /// before this window opened have been delivered. Jobs that fail or
    /// retry drop out quietly: the Outbox already shows them.
    fn notice_last_session_sends(&mut self, cx: &mut Context<Self>) {
        if self.last_session_jobs.is_empty() {
            return;
        }
        let mut sent = 0;
        self.last_session_jobs.retain(|job_id| {
            match self.outbox_jobs.iter().find(|job| job.id == *job_id) {
                Some(job) if job.state == OutboxJobState::Sent => {
                    sent += 1;
                    false
                }
                Some(job) => matches!(job.state, OutboxJobState::Queued | OutboxJobState::Sending),
                None => false,
            }
        });
        if sent > 0 {
            let message = format!(
                "{} from your last session {} sent",
                count_noun(sent, "message", "messages"),
                if sent == 1 { "was" } else { "were" }
            );
            self.toasts.update(cx, |toasts, cx| {
                toasts.push(Toast::new(message), cx);
            });
        }
    }

    /// Secondary+Z: reverses the latest mail change.
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let result = self.mail_actions.update(cx, |actions, cx| actions.undo(cx));
        match result {
            Ok(Some(entry)) => self.after_undo(&entry, cx),
            Ok(None) => {
                self.toasts.update(cx, |toasts, cx| {
                    toasts.push(Toast::new("Nothing to undo"), cx)
                });
            }
            Err(error) => self.show_error(error, cx),
        }
    }

    /// The Undo button on the toast for change `id`.
    fn undo_from_toast(&mut self, id: UndoId, cx: &mut Context<Self>) {
        let result = self
            .mail_actions
            .update(cx, |actions, cx| actions.undo_entry(id, cx));
        match result {
            Ok(Some(entry)) => self.after_undo(&entry, cx),
            Ok(None) => {}
            Err(error) => self.show_error(error, cx),
        }
    }

    /// Selects the first conversation the undo brought back into the shown
    /// mailbox, and says what was undone.
    ///
    /// A virtual mailbox selects the first conversation the undo put back
    /// into it, in whichever mailbox of whichever account it now is.
    fn after_undo(&mut self, entry: &UndoEntry, cx: &mut Context<Self>) {
        let restored = match self.mailbox_selection() {
            _ if self.list_mode != ListMode::Mail => None,
            Some(MailboxSelection::Mailbox { mailbox_id, .. }) => entry
                .restores_into(mailbox_id)
                .next()
                .map(|thread_id| (thread_id, mailbox_id)),
            Some(selection @ MailboxSelection::Virtual(_)) => entry
                .thread_ids()
                .filter(|thread_id| {
                    self.store
                        .selection_contains(&selection, *thread_id)
                        .unwrap_or(false)
                })
                .find_map(|thread_id| {
                    let thread = self.store.thread_state(thread_id).ok().flatten()?;
                    Some((thread_id, thread.mailbox_id))
                }),
            None => None,
        };
        if let Some((thread_id, mailbox_id)) = restored {
            self.show_thread(thread_id, mailbox_id, cx);
        }
        let message = format!("Undone: {}", entry.label);
        self.toasts
            .update(cx, |toasts, cx| toasts.push(Toast::new(message), cx));
    }

    /// Shows the toast for a recorded mail change, with Undo when it can
    /// be undone. Only the latest change offers Undo, so the toast before
    /// it closes.
    fn show_undo_toast(&mut self, id: UndoId, label: &str, can_undo: bool, cx: &mut Context<Self>) {
        if let Some((toast_id, _)) = self.undo_toast.take() {
            self.toasts
                .update(cx, |toasts, cx| toasts.dismiss(toast_id, cx));
        }
        let mut toast = Toast::new(label.to_owned()).kind(ToastKind::Success);
        if can_undo {
            let workspace = cx.entity().downgrade();
            toast = toast.on_undo(move |_, cx| {
                // The toast host is mid-update while this runs; undo once
                // it is done.
                let workspace = workspace.clone();
                cx.defer(move |cx| {
                    let _ = workspace.update(cx, |this, cx| this.undo_from_toast(id, cx));
                });
            });
        }
        let toast_id = self.toasts.update(cx, |toasts, cx| toasts.push(toast, cx));
        self.undo_toast = can_undo.then_some((toast_id, id));
    }

    fn reply(&mut self, _: &Reply, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_thread_id().is_some() {
            self.reply_to_selected_thread(false, None, window, cx);
        }
    }

    fn reply_to_message(
        &mut self,
        action: &ReplyToMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_thread_id().is_some() {
            self.reply_to_selected_thread(false, Some(action.message_id), window, cx);
        }
    }

    fn reply_all(&mut self, _: &ReplyAll, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_thread_id().is_some() {
            self.reply_to_selected_thread(true, None, window, cx);
        }
    }

    fn forward(&mut self, _: &Forward, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_thread_id().is_some() {
            self.forward_selected_thread(window, cx);
        }
    }

    fn save_attachments(&mut self, _: &SaveAttachments, _: &mut Window, cx: &mut Context<Self>) {
        if self.command_thread_id().is_some() {
            self.save_all_attachments(None, cx);
        }
    }

    /// "Looks safe" on a sender-safety banner: hides it for that message.
    fn dismiss_sender_warning(
        &mut self,
        action: &DismissSenderWarning,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.store.dismiss_sender_warning(action.message_id) {
            Ok(_) => {
                self.refresh_from_store(cx);
                cx.notify();
            }
            Err(error) => self.show_error(format!("{error:#}"), cx),
        }
    }

    // -----------------------------------------------------------------------
    // Workspace commands
    // -----------------------------------------------------------------------

    fn new_message(&mut self, _: &NewMessage, window: &mut Window, cx: &mut Context<Self>) {
        self.open_compose(window, cx);
    }

    fn add_account(&mut self, _: &AddAccount, window: &mut Window, cx: &mut Context<Self>) {
        self.open_account_setup(window, cx);
    }

    fn show_mail(&mut self, _: &ShowMail, window: &mut Window, cx: &mut Context<Self>) {
        self.set_list_mode(ListMode::Mail, cx);
        self.focus_pane(Pane::MessageList, window, cx);
    }

    fn show_drafts(&mut self, _: &ShowDrafts, window: &mut Window, cx: &mut Context<Self>) {
        self.show_drafts_list(cx);
        self.focus_pane(Pane::MessageList, window, cx);
    }

    /// Lists the drafts, with the selected one in the compose pane.
    fn show_drafts_list(&mut self, cx: &mut Context<Self>) {
        self.set_list_mode(ListMode::Drafts, cx);
        self.detail_mode = DetailMode::Compose;
    }

    fn show_outbox(&mut self, _: &ShowOutbox, window: &mut Window, cx: &mut Context<Self>) {
        self.set_list_mode(ListMode::Outbox, cx);
        self.focus_pane(Pane::MessageList, window, cx);
    }

    fn retry_outbox(&mut self, _: &RetryOutbox, window: &mut Window, cx: &mut Context<Self>) {
        self.retry_selected_outbox(window, cx);
    }

    /// Focuses the search field with its text selected, so typing
    /// replaces it.
    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search_query.update(cx, |input, cx| {
            input.select_range(0..input.text().len(), cx);
            input.focus(window, cx);
        });
    }

    /// Down in the search field moves into the results. Other single-line
    /// fields pass Down on, so it only reaches here from them unhandled.
    fn search_field_down(
        &mut self,
        _: &components::text_input::Down,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_query.focus_handle(cx).is_focused(window) {
            self.search_into_results(window, cx);
        } else {
            cx.propagate();
        }
    }

    fn sync_account_action(
        &mut self,
        _: &SyncAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_selected_account(window, cx);
    }

    fn sync_all_action(&mut self, _: &SyncAll, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_all(window, cx);
    }

    /// Escape: closes the topmost thing in the way, one per press.
    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .search_query
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            // Escape in the search field clears it and leaves search.
            self.end_search(cx);
            self.focus_pane(Pane::MessageList, window, cx);
        } else if self.sidebar_overlay_open {
            self.close_sidebar_overlay(window, cx);
        } else if self.list_selection.is_multiple() {
            self.set_list_selection(Vec::new(), cx);
            if let Some((thread_id, mailbox_id)) = self
                .selected_thread_id
                .zip(self.selected_mailbox_id)
                .filter(|_| self.list_mode == ListMode::Mail)
            {
                self.show_thread(thread_id, mailbox_id, cx);
            }
        } else if self.store_error.is_some() {
            self.dismiss_error(cx);
        } else if matches!(self.detail_mode, DetailMode::AccountSetup) && !self.accounts.is_empty()
        {
            self.detail_mode = DetailMode::Reader;
            self.focus_pane(Pane::MessageList, window, cx);
            cx.notify();
        } else if self.status_message.is_some() {
            self.status_message = None;
            cx.notify();
        } else if self.go_back_to_list(window, cx) {
        } else if matches!(self.list_mode, ListMode::Search) {
            self.end_search(cx);
        }
    }

    // -----------------------------------------------------------------------
    // Pane focus
    // -----------------------------------------------------------------------

    /// The focus handle of whichever pane the detail area shows.
    fn detail_focus_handle(&self, cx: &App) -> FocusHandle {
        match self.detail_mode {
            DetailMode::Reader => self.reader.focus_handle(cx),
            DetailMode::Compose => self.compose.focus_handle(cx),
            DetailMode::AccountSetup => self.account_setup.focus_handle(cx),
        }
    }

    /// The pane that holds keyboard focus, if any (the search field and
    /// the window root are not panes).
    pub(crate) fn focused_pane(&self, window: &Window, cx: &App) -> Option<Pane> {
        if self.sidebar.focus_handle(cx).contains_focused(window, cx) {
            Some(Pane::Sidebar)
        } else if self
            .message_list
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            Some(Pane::MessageList)
        } else if self.detail_focus_handle(cx).contains_focused(window, cx) {
            Some(Pane::Detail)
        } else {
            None
        }
    }

    /// Moves keyboard focus into a pane. Compose and account setup focus
    /// their first field, so typing works straight away.
    ///
    /// Panes the layout is not showing come into view: the sidebar opens
    /// as an overlay, and the single-pane layout switches to the pane.
    pub(crate) fn focus_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        if pane == Pane::Sidebar {
            self.sidebar_overlay_open = self.pane_layout().sidebar.is_none();
        } else {
            self.sidebar_overlay_open = false;
            self.single_pane = if pane == Pane::Detail {
                SinglePane::Detail
            } else {
                SinglePane::List
            };
        }
        match pane {
            Pane::Sidebar => window.focus(&self.sidebar.focus_handle(cx), cx),
            Pane::MessageList => window.focus(&self.message_list.focus_handle(cx), cx),
            Pane::Detail => match self.detail_mode {
                DetailMode::Reader => window.focus(&self.reader.focus_handle(cx), cx),
                DetailMode::Compose => self
                    .compose
                    .update(cx, |compose, cx| compose.focus_pane(window, cx)),
                DetailMode::AccountSetup => self
                    .account_setup
                    .update(cx, |form, cx| form.focus_first_field(window, cx)),
            },
        }
        cx.notify();
    }

    fn cycle_pane(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.focused_pane(window, cx) {
            Some(pane) => pane.step(forward),
            None if forward => Pane::Sidebar,
            None => Pane::Detail,
        };
        self.focus_pane(next, window, cx);
    }

    fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_pane(true, window, cx);
    }

    fn focus_previous_pane(
        &mut self,
        _: &FocusPreviousPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_pane(false, window, cx);
    }

    // -----------------------------------------------------------------------
    // Pane events
    // -----------------------------------------------------------------------

    fn on_mail_event(
        &mut self,
        _: &Entity<MailActions>,
        event: &MailEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // Autosave fires on every keystroke; the drafts list catches up
            // a moment later.
            MailEvent::DraftSaved { .. } => self.schedule_refresh(window, cx),
            MailEvent::UndoRecorded {
                id,
                label,
                can_undo,
            } => self.show_undo_toast(*id, label, *can_undo, cx),
            MailEvent::Undone { id, .. } => {
                if let Some((toast_id, _)) = self.undo_toast.take_if(|(_, undo)| undo == id) {
                    self.toasts
                        .update(cx, |toasts, cx| toasts.dismiss(toast_id, cx));
                }
            }
            MailEvent::ThreadMoved { .. }
            | MailEvent::ThreadDestroyed { .. }
            | MailEvent::ThreadFlagChanged { .. }
            | MailEvent::ThreadReadChanged { .. }
            | MailEvent::DraftCreated { .. }
            | MailEvent::DraftAttachmentsChanged { .. }
            | MailEvent::OutboxJobRetried { .. }
            | MailEvent::SendCancelled { .. } => {
                // A change to many threads emits an event for each; the
                // store is read once, after the last.
                if !self.mail_refresh_queued {
                    self.mail_refresh_queued = true;
                    cx.defer_in(window, |this, _, cx| {
                        this.mail_refresh_queued = false;
                        this.refresh_from_store(cx);
                        cx.notify();
                    });
                }
            }
        }
    }

    fn on_sidebar_event(
        &mut self,
        _: &Entity<SidebarView>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match *event {
            SidebarEvent::SelectMailbox(mailbox_id) => self.select_mailbox(mailbox_id, cx),
            SidebarEvent::SelectVirtual(virtual_mailbox) => {
                self.select_virtual_mailbox(virtual_mailbox, cx)
            }
            SidebarEvent::ShowDrafts => self.show_drafts_list(cx),
            SidebarEvent::ShowOutbox => self.set_list_mode(ListMode::Outbox, cx),
            SidebarEvent::RetryOutbox => self.retry_failed_outbox(window, cx),
            SidebarEvent::ToggleCollapsed(ref node) => self.toggle_sidebar_node(node, cx),
            SidebarEvent::FixAccount(account_id) => self.fix_account(account_id, window, cx),
            // A drop is an ordinary, undoable move.
            SidebarEvent::DropThreads {
                ref thread_ids,
                mailbox_id,
            } => {
                self.change_these_threads(thread_ids.clone(), cx, |actions, thread_ids, cx| {
                    actions.move_threads(thread_ids, mailbox_id, cx)
                });
            }
            SidebarEvent::Activate => self.focus_pane(Pane::MessageList, window, cx),
            // Picking a view in the overlay closes it.
            SidebarEvent::RowClicked => {
                if self.sidebar_overlay_open {
                    self.focus_pane(Pane::MessageList, window, cx);
                }
            }
        }
    }

    fn on_message_list_event(
        &mut self,
        _: &Entity<MessageListView>,
        event: &MessageListEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            MessageListEvent::OpenThread {
                thread_id,
                mailbox_id,
            } => {
                self.list_selection = ListSelection::default();
                self.select_thread(*thread_id, *mailbox_id, cx);
            }
            MessageListEvent::SelectThreads {
                thread_ids,
                thread_id,
                mailbox_id: _,
            } => {
                self.selected_thread_id = Some(*thread_id);
                self.set_list_selection(thread_ids.clone(), cx);
            }
            MessageListEvent::OpenDraft(draft_id) => {
                self.open_draft(*draft_id, window, cx);
                self.list_mode = ListMode::Drafts;
            }
            MessageListEvent::SelectOutboxJob(job_id) => {
                self.selected_outbox_job_id = Some(*job_id);
                cx.notify();
            }
            MessageListEvent::RetryOutboxJob(job_id) => {
                self.selected_outbox_job_id = Some(*job_id);
                self.retry_selected_outbox(window, cx);
            }
            MessageListEvent::Activate => self.focus_pane(Pane::Detail, window, cx),
            // In the single-pane layout a click opens the row's detail;
            // elsewhere the detail is already beside the list.
            MessageListEvent::RowClicked => {
                if self.pane_layout().mode == LayoutMode::Single {
                    self.focus_pane(Pane::Detail, window, cx);
                }
            }
            MessageListEvent::Error(error) => self.show_error(error, cx),
        }
    }

    fn on_compose_event(
        &mut self,
        _: &Entity<ComposeView>,
        event: &ComposeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposeEvent::Status(message) => self.show_status(message.clone(), cx),
            ComposeEvent::Error(error) => self.show_error(error, cx),
            &ComposeEvent::Queued {
                draft_id,
                job_id,
                undo_delay,
                ..
            } => {
                // The pane lets go of the queued draft (Undo brings it back)
                // and, beside the mail list, goes back to the conversation.
                self.compose
                    .update(cx, |compose, cx| compose.close_draft(cx));
                if self.list_mode == ListMode::Mail {
                    self.detail_mode = DetailMode::Reader;
                }
                self.show_undo_send_toast(draft_id, job_id, undo_delay, ReopenIn::Pane, cx);
                self.refresh_from_store(cx);
                self.schedule_refresh(window, cx);
                cx.notify();
            }
            ComposeEvent::PopOut { draft_id } => {
                self.open_draft_in_window(*draft_id, false, window, cx);
            }
            ComposeEvent::ShowWindow { draft_id } => {
                self.open_draft_in_window(*draft_id, false, window, cx);
            }
        }
    }

    fn on_account_setup_event(
        &mut self,
        _: &Entity<AccountSetupView>,
        event: &AccountSetupEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AccountSetupEvent::Added {
                account_id,
                message,
            } => {
                self.selected_account_id = Some(*account_id);
                self.detail_mode = DetailMode::Reader;
                self.list_mail(cx);
                self.show_status(message.clone(), cx);
                self.refresh_from_store(cx);
                self.schedule_refresh(window, cx);
                self.focus_pane(Pane::MessageList, window, cx);
            }
            AccountSetupEvent::Error(error) => self.show_error(error, cx),
        }
    }
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Layout of a cached pane `width` pixels wide. A cached pane is laid out
/// from this style, not measured from its content.
fn fixed_pane_style(width: f32) -> StyleRefinement {
    StyleRefinement::default().w(px(width)).flex_none().h_full()
}

fn detail_style() -> StyleRefinement {
    StyleRefinement::default()
        .flex_1()
        // Long subjects, addresses and bodies wrap or truncate inside the
        // pane instead of widening it past the window edge.
        .min_w(px(DETAIL_MIN_WIDTH))
        .h_full()
}

fn fill_style() -> StyleRefinement {
    StyleRefinement::default().size_full()
}

/// Height of the single-pane layout's back bar.
const BACK_BAR_HEIGHT: f32 = 44.0;

impl WorkspaceView {
    /// The panes for `layout`, each its own cached view, with split
    /// handles between them and the sidebar overlay on top.
    fn render_panes(
        &self,
        layout: PaneLayout,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let detail: AnyView = match self.detail_mode {
            DetailMode::Reader => self.reader.clone().into(),
            DetailMode::Compose => self.compose.clone().into(),
            DetailMode::AccountSetup => self.account_setup.clone().into(),
        };
        let panes = div().relative().flex().flex_1().overflow_hidden();
        let panes = match layout.mode {
            LayoutMode::Single => match self.single_pane {
                SinglePane::List => panes.child(self.message_list.clone().cached(fill_style())),
                SinglePane::Detail => panes.child(
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .child(self.render_back_bar(colors))
                        .child(detail.cached(StyleRefinement::default().flex_1().w_full())),
                ),
            },
            LayoutMode::Wide | LayoutMode::Overlay => panes
                .when_some(layout.sidebar, |this, width| {
                    this.child(self.sidebar.clone().cached(fixed_pane_style(width)))
                        .child(
                            SplitHandle::new("sidebar-split", width)
                                .range(SIDEBAR_MIN_WIDTH, layout.sidebar_max)
                                .default_width(SIDEBAR_WIDTH)
                                .on_resize(cx.listener(|this, width: &f32, _, cx| {
                                    this.resize_sidebar(*width, cx);
                                })),
                        )
                })
                .child(
                    self.message_list
                        .clone()
                        .cached(fixed_pane_style(layout.list)),
                )
                .child(
                    SplitHandle::new("list-split", layout.list)
                        .range(MESSAGE_LIST_MIN_WIDTH, layout.list_max)
                        .default_width(MESSAGE_LIST_WIDTH)
                        .on_resize(cx.listener(|this, width: &f32, _, cx| {
                            this.resize_list(*width, cx);
                        })),
                )
                .child(detail.cached(detail_style())),
        };
        let overlay = self.sidebar_overlay_open && layout.sidebar.is_none();
        let scrim = if components::ComponentTheme::get(cx).is_light {
            hsla(0.0, 0.0, 0.0, 0.25)
        } else {
            hsla(0.0, 0.0, 0.0, 0.45)
        };
        panes.when(overlay, |this| {
            this.child(
                // A click beside the overlay closes it.
                div()
                    .id("sidebar-scrim")
                    .debug_selector(|| "sidebar-scrim".into())
                    .absolute()
                    .inset_0()
                    .bg(scrim)
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.close_sidebar_overlay(window, cx)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(px(layout.overlay_sidebar))
                    .shadow_lg()
                    .occlude()
                    .child(self.sidebar.clone().cached(fill_style())),
            )
        })
    }

    /// The single-pane layout's bar above the detail pane: a back button
    /// to the list, named after it.
    fn render_back_bar(&self, colors: &UiColors) -> impl IntoElement {
        let title = self.list_title();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(BACK_BAR_HEIGHT))
            .px_2()
            .bg(rgb(colors.surface))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                div().debug_selector(|| "single-pane-back".into()).child(
                    IconButton::new("single-pane-back", "arrow-left", format!("Back to {title}"))
                        .shortcut("secondary-[")
                        .on_click(dispatch_from(&self.focus_handle, GoBack)),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .ellipsis()
                    .child(title),
            )
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = UiColors::from_theme(&self.bootstrap.theme);
        // Components read the theme from a global; keep it in step.
        components::ComponentTheme::sync(&self.bootstrap.theme, cx);
        // The panes and text inputs read the palette from this global.
        if cx.try_global::<UiColors>() != Some(&colors) {
            cx.set_global(colors);
        }
        let layout = self.pane_layout();

        let root = div()
            .id("workspace")
            .relative()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::new_message))
            .on_action(cx.listener(Self::add_account))
            .on_action(cx.listener(Self::show_mail))
            .on_action(cx.listener(Self::show_drafts))
            .on_action(cx.listener(Self::show_outbox))
            .on_action(cx.listener(Self::retry_outbox))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::search_field_down))
            .on_action(cx.listener(Self::search_in_folder))
            .on_action(cx.listener(Self::search_in_account))
            .on_action(cx.listener(Self::search_all_accounts))
            .on_action(cx.listener(Self::sync_account_action))
            .on_action(cx.listener(Self::sync_all_action))
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::toggle_density))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::go_back))
            .on_action(cx.listener(Self::focus_next_pane))
            .on_action(cx.listener(Self::focus_previous_pane))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::reply))
            .on_action(cx.listener(Self::reply_to_message))
            .on_action(cx.listener(Self::reply_all))
            .on_action(cx.listener(Self::forward))
            .on_action(cx.listener(Self::archive))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::toggle_flag))
            .on_action(cx.listener(Self::toggle_read))
            .on_action(cx.listener(Self::save_attachments))
            .on_action(cx.listener(Self::open_attachment))
            .on_action(cx.listener(Self::save_attachment_as))
            .on_action(cx.listener(Self::save_message_attachments))
            .on_action(cx.listener(Self::dismiss_sender_warning))
            .on_action(cx.listener(Self::move_to_mailbox))
            .size_full()
            .font_family(PlatformStyle::current().ui_font.family())
            .text_color(rgb(colors.foreground))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    // ---- Title bar, with the toolbar inside it ----------------
                    .child(
                        TitleBar::new("title-bar")
                            .when(
                                PlatformStyle::current().menu != MenuPlacement::MenuBar,
                                |this| this.trailing(self.app_menu.clone()),
                            )
                            .child(self.render_toolbar(&colors, cx)),
                    )
                    // ---- The panes, each its own cached view ------------------
                    .child(self.render_panes(layout, &colors, cx))
                    // ---- Status / error banners at the bottom ----------------
                    .when_some(self.status_message.clone(), |this, message| {
                        this.child(status_banner(
                            &message,
                            "check-circle",
                            colors.success,
                            colors.success_background,
                            &colors,
                        ))
                    })
                    .when_some(self.store_error.clone(), |this, error| {
                        this.child(
                            status_banner(
                                &error,
                                "alert-circle",
                                colors.danger,
                                colors.danger_background,
                                &colors,
                            )
                            .child(
                                div()
                                    .id("dismiss-error")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .px_4()
                                    .cursor_pointer()
                                    .hover_bg(false, &colors)
                                    .on_click(cx.listener(|this: &mut Self, _, _, cx| {
                                        this.dismiss_error(cx);
                                    }))
                                    .child(icon("x", ICON_SIZE_SM, colors.muted)),
                            ),
                        )
                    }),
            )
            .children(self.render_open_warning(cx))
            .child(self.toasts.clone());
        // The Linux client-side decoration frame (nothing elsewhere); it
        // also paints the window background.
        components::window_frame(root, window, cx)
    }
}

// ---------------------------------------------------------------------------
// Private utility functions
// ---------------------------------------------------------------------------

/// Split a comma-separated address string into individual trimmed addresses,
/// discarding empty entries.
/// Whether an outbox message failed and waits for a retry.
fn is_retryable(state: OutboxJobState) -> bool {
    matches!(state, OutboxJobState::Failed | OutboxJobState::Retrying)
}

pub(crate) fn split_addresses(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

/// How long after the last pane resize the widths are saved.
const PANE_WIDTHS_SAVE_DELAY: Duration = Duration::from_millis(300);

/// How long a status message stays before it dismisses itself.
const STATUS_TOAST_DURATION: Duration = Duration::from_secs(4);

/// How long a thread stays open before it is marked read, so skimming
/// past a message with the mouse does not mark it read.
const MARK_READ_DELAY: Duration = Duration::from_millis(1500);

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

fn quoted_reply_body(message: &ThreadMessage, hour_cycle: HourCycle) -> String {
    format!(
        "\n\nOn {}, {} wrote:\n{}",
        format_full_date(&message.received_at, &chrono::Local, hour_cycle),
        message.sender,
        quote_text(&message.rendered_body)
    )
}

fn forwarded_body(message: &ThreadMessage, hour_cycle: HourCycle) -> String {
    format!(
        "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {}\nTo: {}\nSubject: {}\n\n{}",
        message.sender,
        format_full_date(&message.received_at, &chrono::Local, hour_cycle),
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
    use super::{normalize_contact, prefixed_subject, split_contact_summary};

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
