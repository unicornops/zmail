//! Native GPUI shell for Zmail through Phase 4.
//!
//! This crate is organised as a set of focused modules:
//!
//! | Module | Content |
//! |--------|---------|
//! | `styles` | Design tokens (type scale, spacing, density, `PlatformStyle`), `UiColors`, contrast helpers |
//! | `components` | the component set: `Button`, `Menu`, `Select`, `Modal`, `ToastHost`, `SplitHandle`, `TextInput`, … and the gallery |
//! | `input` | `UiBuffer`, `EditorField`, `InputOutcome` |
//! | `widgets` | `toolbar_button`, `input_card`, `list_row`, `icon`, … |
//! | `workspace` | `impl WorkspaceView` — all business-logic methods |
//! | `toolbar` | `render_toolbar()` — action buttons + search |
//! | `diagnostics` | internal counters kept for the future Settings → About page |
//! | `sidebar` | `render_sidebar()` — accounts + mailbox list |
//! | `message_list` | `render_message_list()` — threads / search / drafts / outbox |
//! | `reader` | `render_reader_detail()`, `reader_action_buttons()` |
//! | `compose` | `ComposeState`, `render_compose_detail()`, `compose_action_buttons()` |
//! | `account_setup` | `AccountSetupState`, `render_account_setup_detail()`, `account_protocol_buttons()` |
//! | `preferences` | loading and saving the persisted UI preferences (theme mode so far) |

use std::sync::Arc;

use gpui::{
    AnyElement, App, AssetSource, Bounds, Context, FocusHandle, IntoElement, Render, Styled,
    TitlebarOptions, UniformListScrollHandle, Window, WindowBounds, WindowOptions, div, prelude::*,
    px, rgb, size,
};
use zmail_core::{
    APP_NAME, AccountOverview, AccountSummary, AppBootstrap, DraftSummary, IdentitySummary,
    MailboxSummary, OutboxJobSummary, SearchHit, StoreStats,
};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

mod styles;
use crate::styles::{
    DETAIL_MIN_WIDTH, ICON_SIZE_SM, UiColors, WINDOW_HEIGHT, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH,
    WINDOW_WIDTH,
};

mod input;
use crate::input::{EditorField, UiBuffer};

mod widgets;
use crate::widgets::{HoverExt, icon, status_banner, toolbar_button};

mod compose;
use crate::compose::{ComposeState, compose_action_buttons, render_compose_detail};

mod account_setup;
use crate::account_setup::{AccountSetupState, render_account_setup_detail};

mod reader;
use crate::reader::{reader_move_buttons, reader_primary_action_buttons, render_reader_detail};

mod components;
pub use components::gallery::run_gallery;
mod diagnostics;
mod message_list;
mod preferences;
mod sidebar;
mod toolbar;
mod tooltip;
mod workspace;

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Application id used for the Linux window class / Wayland app id. It
/// matches the macOS bundle identifier and the packaged desktop entry name.
pub const APP_ID: &str = "ie.unicornops.zmail";

/// What the window shows when it first opens.
///
/// Normal launches use [`InitialView::Inbox`]; a `mailto:` launch opens its
/// draft. The other variants exist for `zmail-app launch-ui-fixture`, which
/// `scripts/capture-release-screenshot.sh --matrix` uses to put the window
/// into each state of the UI screenshot matrix without clicking through it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InitialView {
    /// The first mailbox of the first account, with its newest thread open
    /// (or account setup when no account exists yet).
    #[default]
    Inbox,
    /// The compose pane on this stored draft.
    Draft(i64),
    /// The reader on this thread, without marking it read.
    Thread(i64),
    /// The account setup pane, even when accounts already exist.
    AccountSetup,
}

/// Runs the native Phase 4 shell.
pub fn run(
    asset_source: impl AssetSource,
    bootstrap: AppBootstrap,
    store: Arc<LocalStore>,
    sync_service: SyncService,
    secrets: Arc<dyn SecretStore>,
    initial_view: InitialView,
) {
    let notifier = Arc::new(zmail_notify::NewMailNotifier::os_backed());
    sync_service.set_notifier(notifier.clone());
    // `gpui_platform::application()` picks the OS backend (macOS, Windows,
    // or Linux Wayland/X11) and leaves AccessKit enabled; never switch to
    // `Application::new_inaccessible`, which turns screen readers off.
    gpui_platform::application()
        .with_assets(asset_source)
        .run(move |cx: &mut App| {
            components::init(cx);
            components::text_input::init(cx);
            let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
            let bootstrap = bootstrap.clone();
            let store = store.clone();
            let sync_service = sync_service.clone();
            let secrets = secrets.clone();

            if let Err(error) = cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some(APP_NAME.into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
                    // Matches the macOS bundle id and the packaged
                    // `ie.unicornops.zmail.desktop`, so Linux docks
                    // (StartupWMClass / Wayland app id) find the window.
                    app_id: Some(APP_ID.to_owned()),
                    ..Default::default()
                },
                move |window, cx| {
                    let bootstrap = bootstrap.clone();
                    let store = store.clone();
                    let sync_service = sync_service.clone();
                    let secrets = secrets.clone();
                    let notifier = notifier.clone();
                    cx.new(|cx| {
                        WorkspaceView::new(
                            bootstrap,
                            store,
                            sync_service,
                            secrets,
                            notifier,
                            initial_view,
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
    notifier: Arc<zmail_notify::NewMailNotifier<zmail_notify::OsNotificationSink>>,
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
    /// The query that produced `search_results`, shown in the list header
    /// (the search field itself may have been edited since).
    search_results_query: String,
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
    /// The last error. It stays until the user dismisses it.
    store_error: Option<String>,
    /// A transient status message; [`WorkspaceView::show_status`] clears it
    /// after a few seconds.
    status_message: Option<String>,
    /// Bumped by every status message so only the latest one's timer
    /// clears it.
    status_generation: u64,
    /// The selected thread's view, loaded once per selection or store
    /// refresh instead of on every frame.
    thread_view_cache: Option<zmail_core::ThreadView>,
    refresh_scheduled: bool,
    /// Set while the user's saved theme mode forces light or dark; while
    /// unset the theme follows the system light/dark appearance.
    theme_overridden: bool,
    /// 12- or 24-hour clock for list and reader dates, from the locale.
    hour_cycle: zmail_core::time::HourCycle,
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = UiColors::from_theme(&self.bootstrap.theme);
        // Components read the theme from a global; keep it in step.
        components::ComponentTheme::sync(&self.bootstrap.theme, cx);

        // Mutable store queries must complete before immutable borrows below.
        let selected_thread = self.selected_thread_view();

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
                            .child(self.render_detail_pane(&colors, selected_thread, cx)),
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_1()
            // Long subjects, addresses and bodies wrap or truncate inside
            // the pane instead of widening it past the window edge.
            .min_w(px(DETAIL_MIN_WIDTH))
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
                    // A flex column stretches its content to the pane width,
                    // so form fields and long text size to the pane rather
                    // than to their own content.
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .px_4()
                    .py_4()
                    .child(self.render_detail_content(colors, &selected_thread, cx)),
            )
            // ---- Action buttons bar at the bottom ------------------------
            .child(
                div()
                    .flex()
                    .flex_wrap()
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
                            .children(compose_action_buttons(self.compose.is_some(), colors, cx)),
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
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.detail_mode {
            // ---- Reader ---------------------------------------------------
            DetailMode::Reader => render_reader_detail(
                selected_thread.as_ref(),
                self.hour_cycle,
                &self.own_addresses(),
                colors,
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
