//! Native GPUI shell for Zmail through Phase 4.
//!
//! This crate is organised as a set of focused modules:
//!
//! | Module | Content |
//! |--------|---------|
//! | `styles` | Design tokens, `UiColors`, colour helpers |
//! | `input` | `UiBuffer`, `EditorField`, `InputOutcome` |
//! | `widgets` | `toolbar_button`, `input_card`, `list_row`, `icon`, … |
//! | `workspace` | `impl WorkspaceView` — all business-logic methods |
//! | `toolbar` | `render_toolbar()` — header strip + action buttons |
//! | `sidebar` | `render_sidebar()` — accounts + mailbox list |
//! | `message_list` | `render_message_list()` — threads / search / drafts / outbox |
//! | `reader` | `render_reader_detail()`, `reader_action_buttons()` |
//! | `compose` | `ComposeState`, `render_compose_detail()`, `compose_focus_buttons()` |
//! | `account_setup` | `AccountSetupState`, `render_account_setup_detail()`, `account_protocol_buttons()` |

use std::sync::Arc;

use gpui::{
    AnyElement, App, Application, AssetSource, Bounds, Context, FocusHandle, IntoElement, Render,
    SharedString, Styled, TitlebarOptions, UniformListScrollHandle, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, rgb, size,
};
use zmail_core::{
    APP_NAME, AccountOverview, AccountSummary, AppBootstrap, DraftSummary, IdentitySummary,
    MailboxSummary, OutboxJobSummary, PHASE_LABEL, SearchHit, StoreStats,
};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

mod styles;
use crate::styles::{UiColors, WINDOW_HEIGHT, WINDOW_WIDTH};

mod input;
use crate::input::{EditorField, UiBuffer};

mod widgets;
use crate::widgets::{status_banner, toolbar_button};

mod compose;
use crate::compose::{ComposeState, compose_focus_buttons, render_compose_detail};

mod account_setup;
use crate::account_setup::{AccountSetupState, render_account_setup_detail};

mod reader;
use crate::reader::{reader_move_buttons, reader_primary_action_buttons, render_reader_detail};

mod message_list;
mod sidebar;
mod toolbar;
mod workspace;

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Runs the native Phase 4 shell.
pub fn run(
    asset_source: impl AssetSource,
    bootstrap: AppBootstrap,
    store: Arc<LocalStore>,
    sync_service: SyncService,
    secrets: Arc<dyn SecretStore>,
    initial_draft_id: Option<i64>,
) {
    Application::new()
        .with_assets(asset_source)
        .run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
            let bootstrap = bootstrap.clone();
            let store = store.clone();
            let sync_service = sync_service.clone();
            let secrets = secrets.clone();
            let initial_draft_id = initial_draft_id;

            if let Err(error) = cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some(format!("{APP_NAME} {PHASE_LABEL}").into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                move |window, cx| {
                    let bootstrap = bootstrap.clone();
                    let store = store.clone();
                    let sync_service = sync_service.clone();
                    let secrets = secrets.clone();
                    let initial_draft_id = initial_draft_id;
                    cx.new(|cx| {
                        WorkspaceView::new(
                            bootstrap,
                            store,
                            sync_service,
                            secrets,
                            initial_draft_id,
                            window,
                            cx,
                        )
                    })
                },
            ) {
                panic!("failed to open the Phase 4 window: {error:#}");
            }
        });
}

// ---------------------------------------------------------------------------
// Enums shared across modules
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// WorkspaceView — the single GPUI entity for the entire workspace
// ---------------------------------------------------------------------------

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
    /// Number of mailbox operations still queued for server write-back on
    /// the selected account.
    pending_op_count: usize,
    /// Number of write-back operations that exhausted their retry budget
    /// on the selected account.
    dead_op_count: usize,
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
    hovered_element: Option<SharedString>,
    refresh_scheduled: bool,
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = UiColors::from_theme(&self.bootstrap.theme);

        // Mutable store queries must complete before immutable borrows below.
        let selected_thread = self.selected_thread_view();
        let selected_account = self.selected_account().cloned();

        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .bg(rgb(colors.background))
            .text_color(rgb(colors.foreground))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    // ---- Top section: header info strip + toolbar buttons ----
                    .child(self.render_toolbar(&colors, cx))
                    // ---- Three-column content area ---------------------------
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .overflow_hidden()
                            // Left: accounts + mailboxes
                            .child(self.render_sidebar(&colors, cx))
                            // Centre: thread/search/draft/outbox list
                            .child(self.render_message_list(&colors, cx))
                            // Right: detail pane
                            .child(self.render_detail_pane(
                                &colors,
                                selected_thread,
                                selected_account,
                                cx,
                            )),
                    )
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
                        this.child(status_banner(
                            &format!("Issue: {error}"),
                            "alert-circle",
                            colors.danger,
                            colors.warning_background,
                            &colors,
                        ))
                    }),
            )
    }
}

// ---------------------------------------------------------------------------
// Detail pane — coordinates reader / compose / account-setup sub-views
// ---------------------------------------------------------------------------

impl WorkspaceView {
    /// Render the right-hand detail pane, dispatching to the appropriate
    /// sub-view based on [`DetailMode`].
    fn render_detail_pane(
        &self,
        colors: &UiColors,
        selected_thread: Option<zmail_core::ThreadView>,
        selected_account: Option<AccountSummary>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            // ---- Mode header label ----------------------------------------
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(match self.detail_mode {
                        DetailMode::Reader => "Reader",
                        DetailMode::Compose => "Compose",
                        DetailMode::AccountSetup => "Account Setup",
                    }),
            )
            .when(matches!(self.detail_mode, DetailMode::Reader), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(rgb(colors.border))
                        .children(reader_primary_action_buttons(&selected_thread, colors, cx)),
                )
            })
            // ---- Scrollable content area ----------------------------------
            .child(
                div()
                    .id("detail-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_4()
                    .py_4()
                    .child(self.render_detail_content(
                        colors,
                        &selected_thread,
                        &selected_account,
                        cx,
                    )),
            )
            // ---- Action buttons bar at the bottom ------------------------
            .child(
                div()
                    .flex()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .child(match self.detail_mode {
                        DetailMode::Reader => {
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .children(reader_move_buttons(
                                    &selected_thread,
                                    &self.mailboxes,
                                    colors,
                                    cx,
                                ))
                        }
                        DetailMode::Compose => div()
                            .flex()
                            .gap_2()
                            .children(compose_focus_buttons(colors, cx)),
                        DetailMode::AccountSetup => div().flex().gap_2().child(toolbar_button(
                            "Save Account",
                            Some("check-circle"),
                            colors.button_background,
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.submit_account_setup(window, cx);
                            }),
                        )),
                    }),
            )
    }

    /// Render the scrollable content inside the detail pane.
    fn render_detail_content(
        &self,
        colors: &UiColors,
        selected_thread: &Option<zmail_core::ThreadView>,
        selected_account: &Option<AccountSummary>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.detail_mode {
            // ---- Reader ---------------------------------------------------
            DetailMode::Reader => render_reader_detail(
                selected_thread.as_ref(),
                selected_account.as_ref(),
                &self.mailboxes,
                &self.identities,
                colors,
                cx,
            ),

            // ---- Compose -------------------------------------------------
            DetailMode::Compose => render_compose_detail(
                self.compose.as_ref(),
                &self.identities,
                self.active_field,
                colors,
                cx,
            ),

            // ---- Account Setup -------------------------------------------
            DetailMode::AccountSetup => {
                render_account_setup_detail(&self.account_setup, self.active_field, colors, cx)
                    .into_any_element()
            }
        }
    }
}
