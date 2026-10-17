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
    IntoElement, MouseButton, Render, StyleRefinement, Styled, Window, WindowAppearance, div, hsla,
    prelude::*, px, rgb,
};
use std::sync::Arc;
use zmail_core::{
    AccountSummary, AppBootstrap, IdentitySummary, MailboxSummary, NewDraft, StoreStats,
    ThemeAppearance, ThreadMessage, ThreadView,
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
    AddAccount, Archive, Cancel, Delete, FocusNextPane, FocusPreviousPane, FocusSearch, Forward,
    GoBack, MoveToMailbox, NewMessage, Reply, ReplyAll, RetryOutbox, RunSearch, SaveAttachments,
    ShowDrafts, ShowMail, ShowOutbox, SyncAccount, SyncAll, ToggleFlag, ToggleRead, ToggleSidebar,
    ToggleTheme, Undo,
};
use crate::app_menu::AppMenuButton;
use crate::components::{self, IconButton, SplitHandle, TextInput, TextInputEvent, TitleBar};
use crate::compose::{ComposeEvent, ComposeView};
use crate::layout::{LayoutMode, PaneLayout, SinglePane};
use crate::mail_actions::{MailActions, MailEvent};
use crate::message_list::{MessageListEvent, MessageListState, MessageListView};
use crate::pane::{Pane, dispatch_from};
use crate::reader::{ReaderState, ReaderView};
use crate::sidebar::{SidebarEvent, SidebarHighlight, SidebarState, SidebarView};
use crate::styles::{
    DETAIL_MIN_WIDTH, ICON_SIZE_SM, MESSAGE_LIST_MIN_WIDTH, MESSAGE_LIST_WIDTH, MenuPlacement,
    PlatformStyle, READER_COMPACT_ACTIONS_BELOW, SIDEBAR_MIN_WIDTH, SIDEBAR_WIDTH, UiColors,
};
use crate::widgets::{EllipsisExt, HoverExt, icon, status_banner};

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
        let message_list = cx.new(|cx| MessageListView::new(store.clone(), hour_cycle, cx));
        let reader = cx.new(|cx| ReaderView::new(hour_cycle, cx));
        let compose = cx.new(|cx| {
            ComposeView::new(
                store.clone(),
                sync_service.clone(),
                mail_actions.clone(),
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
                .label("Search mail")
                .placeholder("Search mail")
        });

        let focus_handle = cx.focus_handle();
        let app_menu = cx.new(|_| AppMenuButton::new(focus_handle.clone()));

        let subscriptions = vec![
            cx.subscribe(&search_query, |this, _, event: &TextInputEvent, cx| {
                if *event == TextInputEvent::Submitted {
                    this.run_search(cx);
                }
            }),
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
            search_results_query: String::new(),
            server_search_pending: None,
            selected_account_id: None,
            selected_mailbox_id: None,
            selected_thread_id: None,
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
            sidebar_width: SIDEBAR_WIDTH,
            list_width: MESSAGE_LIST_WIDTH,
            pane_widths_generation: 0,
            sidebar_overlay_open: false,
            single_pane: SinglePane::List,
            viewport_width: f32::from(window.viewport_size().width),
            _subscriptions: subscriptions,
        };

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
                    Some("Add your first mail account to get started."),
                    window,
                    cx,
                );
            });
        } else {
            window.focus(&view.message_list.focus_handle(cx), cx);
        }

        match initial_view {
            InitialView::Inbox => {}
            // A mailto launch pre-selects the draft's account and opens the
            // compose pane on the pre-filled draft.
            InitialView::Draft(draft_id) => {
                if let Ok(Some(draft)) = view.store.draft_detail(draft_id) {
                    view.select_account(draft.account_id, cx);
                    view.open_draft(draft_id, window, cx);
                    view.focus_pane(Pane::Detail, window, cx);
                }
            }
            // `show_thread`, not `select_thread`, which would mark the thread
            // read after a delay.
            InitialView::Thread(thread_id) => {
                if let Ok(Some(thread)) = view.store.thread_view(thread_id) {
                    view.show_thread(thread_id, thread.mailbox_id, cx);
                    view.refresh_from_store(cx);
                    view.focus_pane(Pane::Detail, window, cx);
                }
            }
            InitialView::AccountSetup => view.open_account_setup(window, cx),
            InitialView::Sidebar => view.focus_pane(Pane::Sidebar, window, cx),
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
            selected_account_id: self.selected_account_id,
            mailboxes: self.mailboxes.clone(),
            selected_mailbox_id: self.selected_mailbox_id,
            highlight: match self.list_mode {
                ListMode::Mail => SidebarHighlight::Mailbox,
                ListMode::Drafts => SidebarHighlight::Drafts,
                ListMode::Outbox => SidebarHighlight::Outbox,
                ListMode::Search => SidebarHighlight::None,
            },
            draft_count: self.drafts.len(),
            outbox_count: self.outbox_jobs.len(),
        };
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_state(sidebar_state, cx));

        let own_addresses = self.own_addresses();
        let list_state = MessageListState {
            mode: self.list_mode,
            has_accounts: !self.accounts.is_empty(),
            mailbox: self.selected_mailbox().cloned(),
            own_addresses: own_addresses.clone(),
            selected_thread_id: self.selected_thread_id,
            search_results: self.search_results.clone(),
            search_query: self.search_results_query.clone(),
            searching_server: self.server_search_pending.is_some(),
            drafts: self.drafts.clone(),
            selected_draft_id: self.selected_draft_id,
            outbox_jobs: self.outbox_jobs.clone(),
            selected_outbox_job_id: self.selected_outbox_job_id,
        };
        self.message_list
            .update(cx, |list, cx| list.set_state(list_state, cx));

        let reader_state = ReaderState {
            thread: self.selected_thread_view(),
            mailboxes: self.mailboxes.clone(),
            own_addresses,
            compact_actions: self.detail_width() < READER_COMPACT_ACTIONS_BELOW,
        };
        self.reader
            .update(cx, |reader, cx| reader.set_state(reader_state, cx));

        let identities = self.identities.clone();
        self.compose
            .update(cx, |compose, cx| compose.set_identities(identities, cx));
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
        // Re-run the shown search so read state, flags and moves made since
        // it ran show up in the results.
        if matches!(self.list_mode, ListMode::Search)
            && !self.search_results_query.is_empty()
            && let Some(account_id) = self.selected_account_id
            && let Ok(results) =
                self.store
                    .search_threads(account_id, &self.search_results_query, 100)
        {
            self.search_results = results;
        }
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

        // Only the store-owned parts of the open draft refresh: the text
        // inputs keep the user's text, cursor and undo history.
        self.compose
            .update(cx, |compose, cx| compose.refresh_from_store(cx));
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
    fn show_error(&mut self, error: impl ToString, cx: &mut Context<Self>) {
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
                .selected_mailbox()
                .map(|mailbox| mailbox.name.clone())
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
        self.list_mode = ListMode::Mail;
        if matches!(self.detail_mode, DetailMode::AccountSetup) {
            self.detail_mode = DetailMode::Reader;
        }
        self.refresh_from_store(cx);
        cx.notify();
    }

    /// Switch the active mailbox and select its first thread.
    pub(crate) fn select_mailbox(&mut self, mailbox_id: i64, cx: &mut Context<Self>) {
        self.selected_mailbox_id = Some(mailbox_id);
        self.selected_thread_id = match self.store.first_thread_id(mailbox_id) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                self.store_error = Some(error.to_string());
                None
            }
        };
        self.list_mode = ListMode::Mail;
        self.detail_mode = DetailMode::Reader;
        cx.notify();
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
        self.selected_mailbox_id = Some(mailbox_id);
        self.selected_thread_id = Some(thread_id);
        self.detail_mode = DetailMode::Reader;
        self.hydrate_selected_thread_bodies(cx);
        cx.notify();
    }

    /// Opens a draft in the compose pane.
    pub(crate) fn open_draft(
        &mut self,
        draft_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Switch the list panel mode.  Switching to [`ListMode::Mail`] also
    /// resets the detail pane to the reader.
    pub(crate) fn set_list_mode(&mut self, list_mode: ListMode, cx: &mut Context<Self>) {
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

    /// Execute the current search query against the local full-text index.
    pub(crate) fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(account_id) = self.selected_account_id else {
            self.show_error("Select or add an account before searching.", cx);
            return;
        };
        let query = self.search_query.read(cx).text().trim().to_owned();
        if query.is_empty() {
            self.search_results.clear();
            self.list_mode = ListMode::Mail;
            cx.notify();
            return;
        }

        match self.store.search_threads(account_id, &query, 100) {
            Ok(results) => {
                self.search_results = results;
                self.search_results_query = query.clone();
                self.list_mode = ListMode::Search;
                self.detail_mode = DetailMode::Reader;
                self.search_server_if_needed(account_id, query, cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Asks the server too when the local index cannot fully answer the
    /// query (bodies not indexed yet). The hits are recorded in the store
    /// and the results re-read from it, so the list still shows only local
    /// state. Runs once per submitted query.
    fn search_server_if_needed(&mut self, account_id: i64, query: String, cx: &mut Context<Self>) {
        if !self
            .store
            .search_needs_server(account_id, &query)
            .unwrap_or(false)
        {
            self.server_search_pending = None;
            return;
        }
        self.server_search_pending = Some((account_id, query.clone()));
        let service = self.sync_service.clone();
        cx.spawn(async move |this, cx| {
            // Offline or failing servers leave the local results as they are.
            let _ = service.search_server_and_wait(account_id, &query).await;
            let _ = this.update(cx, |view, cx| {
                if view.server_search_pending.as_ref() != Some(&(account_id, query.clone())) {
                    return;
                }
                view.server_search_pending = None;
                if view.selected_account_id == Some(account_id)
                    && view.search_results_query == query
                    && let Ok(results) = view.store.search_threads(account_id, &query, 100)
                {
                    view.search_results = results;
                }
                view.refresh_from_store(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // -----------------------------------------------------------------------
    // Compose workflow
    // -----------------------------------------------------------------------

    /// Open the compose pane for a new draft, creating the draft in the store.
    pub(crate) fn open_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.selected_account_id else {
            self.open_account_setup_with_notice(
                Some("Add a mail account first. You can write messages once it is set up."),
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
                self.open_draft(draft_id, window, cx);
                self.list_mode = ListMode::Drafts;
                self.single_pane = SinglePane::Detail;
                self.compose
                    .update(cx, |compose, cx| compose.focus_to(window, cx));
                self.refresh_from_store(cx);
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

        match self.create_reply_draft(&thread, reply_all, cx) {
            Ok(draft_id) => {
                self.open_draft(draft_id, window, cx);
                self.list_mode = ListMode::Drafts;
                self.single_pane = SinglePane::Detail;
                // The reply starts above the quoted message.
                self.compose
                    .update(cx, |compose, cx| compose.focus_body_start(window, cx));
                self.show_status(
                    if reply_all {
                        "Reply-all draft ready.".to_owned()
                    } else {
                        "Reply draft ready.".to_owned()
                    },
                    cx,
                );
                self.refresh_from_store(cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
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
                self.open_draft(draft_id, window, cx);
                self.list_mode = ListMode::Drafts;
                self.single_pane = SinglePane::Detail;
                self.compose
                    .update(cx, |compose, cx| compose.focus_to(window, cx));
                self.show_status("Forward draft ready.".to_owned(), cx);
                self.refresh_from_store(cx);
            }
            Err(error) => self.store_error = Some(error.to_string()),
        }
        cx.notify();
    }

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
            self.show_status(
                format!(
                    "Saved {} to {}.",
                    count_noun(saved, "attachment", "attachments"),
                    destination.display()
                ),
                cx,
            );
        }
        cx.notify();
    }

    fn create_reply_draft(
        &self,
        thread: &ThreadView,
        reply_all: bool,
        cx: &mut Context<Self>,
    ) -> Result<i64> {
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

    /// Switch to the account setup pane.
    pub(crate) fn open_account_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_account_setup_with_notice(None, window, cx);
    }

    /// Switch to the account setup pane, explaining why it opened when the
    /// user did not ask for it.
    pub(crate) fn open_account_setup_with_notice(
        &mut self,
        notice: Option<&'static str>,
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

    /// Runs a [`MailActions`] change on the conversation the commands act
    /// on. The resulting [`MailEvent`] refreshes the panes.
    fn change_thread(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut MailActions, i64, &mut Context<MailActions>) -> Result<()>,
    ) -> bool {
        let Some(thread_id) = self.command_thread_id() else {
            return false;
        };
        match self
            .mail_actions
            .update(cx, |actions, cx| change(actions, thread_id, cx))
        {
            Ok(()) => true,
            Err(error) => {
                self.show_error(error, cx);
                false
            }
        }
    }

    fn archive(&mut self, _: &Archive, _: &mut Window, cx: &mut Context<Self>) {
        self.change_thread(cx, |actions, thread_id, cx| {
            actions.archive_thread(thread_id, cx)
        });
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.change_thread(cx, |actions, thread_id, cx| {
            actions.delete_thread(thread_id, cx)
        });
    }

    fn toggle_flag(&mut self, _: &ToggleFlag, _: &mut Window, cx: &mut Context<Self>) {
        let flagged = self
            .selected_thread_view()
            .is_some_and(|thread| thread.is_flagged);
        self.change_thread(cx, |actions, thread_id, cx| {
            actions.set_thread_flagged(thread_id, !flagged, cx)
        });
    }

    fn toggle_read(&mut self, _: &ToggleRead, _: &mut Window, cx: &mut Context<Self>) {
        let has_unread = self
            .selected_thread_view()
            .is_some_and(|thread| thread.unread_count > 0);
        self.change_thread(cx, |actions, thread_id, cx| {
            actions.set_thread_read(thread_id, has_unread, cx)
        });
    }

    fn move_to_mailbox(&mut self, action: &MoveToMailbox, _: &mut Window, cx: &mut Context<Self>) {
        let mailbox_id = action.mailbox_id;
        if self.change_thread(cx, |actions, thread_id, cx| {
            actions.move_thread(thread_id, mailbox_id, cx)
        }) {
            // The list follows the conversation to its new mailbox.
            self.selected_mailbox_id = Some(mailbox_id);
            cx.notify();
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        match self.mail_actions.update(cx, |actions, cx| actions.undo(cx)) {
            Ok(Some(label)) => self.show_status(format!("Undone: {label}."), cx),
            Ok(None) => self.show_status("Nothing to undo.", cx),
            Err(error) => self.show_error(error, cx),
        }
    }

    fn reply(&mut self, _: &Reply, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_thread_id().is_some() {
            self.reply_to_selected_thread(false, window, cx);
        }
    }

    fn reply_all(&mut self, _: &ReplyAll, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_thread_id().is_some() {
            self.reply_to_selected_thread(true, window, cx);
        }
    }

    fn forward(&mut self, _: &Forward, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_thread_id().is_some() {
            self.forward_selected_thread(window, cx);
        }
    }

    fn save_attachments(&mut self, _: &SaveAttachments, _: &mut Window, cx: &mut Context<Self>) {
        if self.command_thread_id().is_some() {
            self.save_selected_thread_attachments(cx);
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

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search_query
            .update(cx, |input, cx| input.focus(window, cx));
    }

    fn run_search_action(&mut self, _: &RunSearch, _: &mut Window, cx: &mut Context<Self>) {
        self.run_search(cx);
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
            self.focus_pane(Pane::MessageList, window, cx);
        } else if self.sidebar_overlay_open {
            self.close_sidebar_overlay(window, cx);
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
            self.set_list_mode(ListMode::Mail, cx);
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
            MailEvent::UndoRecorded { .. } | MailEvent::Undone { .. } => {}
            MailEvent::ThreadMoved { .. }
            | MailEvent::ThreadDestroyed { .. }
            | MailEvent::ThreadFlagChanged { .. }
            | MailEvent::ThreadReadChanged { .. }
            | MailEvent::DraftCreated { .. }
            | MailEvent::DraftAttachmentsChanged { .. }
            | MailEvent::OutboxJobRetried { .. } => {
                self.refresh_from_store(cx);
                cx.notify();
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
            SidebarEvent::SelectAccount(account_id) => self.select_account(account_id, cx),
            SidebarEvent::SelectMailbox(mailbox_id) => self.select_mailbox(mailbox_id, cx),
            SidebarEvent::ShowDrafts => self.show_drafts_list(cx),
            SidebarEvent::ShowOutbox => self.set_list_mode(ListMode::Outbox, cx),
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
            } => self.select_thread(*thread_id, *mailbox_id, cx),
            MessageListEvent::OpenDraft(draft_id) => {
                self.open_draft(*draft_id, window, cx);
                self.list_mode = ListMode::Drafts;
            }
            MessageListEvent::SelectOutboxJob(job_id) => {
                self.selected_outbox_job_id = Some(*job_id);
                cx.notify();
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
            ComposeEvent::Queued { .. } => {
                self.refresh_from_store(cx);
                self.schedule_refresh(window, cx);
                cx.notify();
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
                self.list_mode = ListMode::Mail;
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
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::new_message))
            .on_action(cx.listener(Self::add_account))
            .on_action(cx.listener(Self::show_mail))
            .on_action(cx.listener(Self::show_drafts))
            .on_action(cx.listener(Self::show_outbox))
            .on_action(cx.listener(Self::retry_outbox))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::run_search_action))
            .on_action(cx.listener(Self::sync_account_action))
            .on_action(cx.listener(Self::sync_all_action))
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::go_back))
            .on_action(cx.listener(Self::focus_next_pane))
            .on_action(cx.listener(Self::focus_previous_pane))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::reply))
            .on_action(cx.listener(Self::reply_all))
            .on_action(cx.listener(Self::forward))
            .on_action(cx.listener(Self::archive))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::toggle_flag))
            .on_action(cx.listener(Self::toggle_read))
            .on_action(cx.listener(Self::save_attachments))
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
            );
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
