//! Native GPUI shell for Zmail.
//!
//! The window is one [`WorkspaceView`] shell that owns a pane entity per
//! area of the screen. Each pane has its own focus handle and key context,
//! re-renders on its own, and talks to the shell through events; every
//! command is a GPUI action (see [`actions`]) bound in the default keymap.
//!
//! | Module | Content |
//! |--------|---------|
//! | `workspace` | the shell: store refresh, selection, pane coordination, action handlers |
//! | `toolbar` | the shell's toolbar and search field |
//! | `sidebar` | `SidebarView`: accounts and mailboxes |
//! | `message_list` | `MessageListView`: threads, search results, drafts, outbox |
//! | `reader` | `ReaderView`: the open conversation and its actions |
//! | `compose` | `ComposeView`: the open draft |
//! | `account_setup` | `AccountSetupView`: the new-account form |
//! | `mail_actions` | `MailActions`: store writes, `MailEvent`s and undo |
//! | `actions` | every command as a GPUI action |
//! | `keymap` | loads `keymaps/default.json` (and `keymaps/macos.json` on macOS) |
//! | `menus` | `MenuModel`, the command menus; the native macOS menu bar; app-wide commands (Quit, Hide, …) |
//! | `app_menu` | `AppMenuButton`: the menu model behind a hamburger/overflow button (Windows, Linux) |
//! | `fonts` | registers the bundled Inter font on Linux |
//! | `pane` | pane focus order and the focused-pane indicator |
//! | `components` | the component set: `Button`, `Menu`, `Select`, `Modal`, `ToastHost`, `SplitHandle`, `TextInput`, … and the gallery |
//! | `styles` | design tokens (type scale, spacing, density, `PlatformStyle`), `UiColors`, contrast helpers |
//! | `preferences` | loading and saving the persisted UI preferences (theme mode so far) |
//! | `widgets` | `toolbar_button`, `input_card`, `list_row`, `icon`, … |
//! | `diagnostics` | internal counters kept for the future Settings → About page |

use std::sync::Arc;

use gpui::{
    App, AppContext as _, AssetSource, Bounds, Entity, FocusHandle, Subscription, TitlebarOptions,
    WindowBounds, WindowDecorations, WindowOptions, point, px, size,
};
use zmail_core::{
    APP_NAME, AccountOverview, AccountSummary, AppBootstrap, DraftSummary, IdentitySummary,
    MailboxSummary, OutboxJobSummary, SearchHit, StoreStats,
};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

mod styles;
use crate::styles::{WINDOW_HEIGHT, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH, WINDOW_WIDTH};

mod components;
use crate::components::TextInput;
use crate::components::title_bar::TRAFFIC_LIGHT_POSITION;
pub use components::gallery::run_gallery;

mod account_setup;
mod actions;
mod app_menu;
mod compose;
mod diagnostics;
mod fonts;
mod keymap;
mod mail_actions;
mod menus;
mod message_list;
mod pane;
mod preferences;
mod reader;
mod sidebar;
mod toolbar;
mod tooltip;
#[cfg(test)]
mod view_tests;
mod widgets;
mod workspace;

use crate::account_setup::AccountSetupView;
use crate::app_menu::AppMenuButton;
use crate::compose::ComposeView;
use crate::mail_actions::MailActions;
use crate::message_list::MessageListView;
use crate::reader::ReaderView;
use crate::sidebar::SidebarView;

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
            fonts::register_ui_font(cx);
            components::init(cx);
            components::text_input::init(cx);
            keymap::bind_default_keymap(cx);
            // After the keymap: the macOS menu bar reads key equivalents
            // from it.
            menus::init(cx);
            let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
            let bootstrap = bootstrap.clone();
            let store = store.clone();
            let sync_service = sync_service.clone();
            let secrets = secrets.clone();

            if let Err(error) = cx.open_window(
                WindowOptions {
                    // Zmail draws its own title bar (`components::title_bar`)
                    // with the toolbar inside it: transparent over the
                    // traffic lights on macOS, custom caption buttons on
                    // Windows, and a client-side header bar on Linux when
                    // the compositor supports it.
                    titlebar: Some(TitlebarOptions {
                        title: Some(APP_NAME.into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(
                            px(TRAFFIC_LIGHT_POSITION.0),
                            px(TRAFFIC_LIGHT_POSITION.1),
                        )),
                    }),
                    // The title bar moves the window itself
                    // (`start_window_move`), so AppKit must not also drag
                    // it or delay its clicks (macOS only).
                    app_owns_titlebar_drag: true,
                    window_decorations: Some(window_decorations()),
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

/// The decorations to ask the window system for (only Linux reads this;
/// macOS and Windows ignore it). `Window::window_decorations` says which
/// the window actually got.
///
/// - GNOME on Wayland has no server-side decorations, so GPUI always draws
///   client-side ones there, whatever is asked for.
/// - KDE draws its own (Breeze) title bars and supports them on Wayland and
///   X11, so Zmail asks for server-side decorations there.
/// - Elsewhere it asks for client-side ones; X11 without a compositor (or a
///   window manager without `_GTK_FRAME_EXTENTS`) falls back to the window
///   manager's title bar.
///
/// `ZMAIL_WINDOW_DECORATIONS=server` or `=client` overrides the choice.
fn window_decorations() -> WindowDecorations {
    decorations_for(
        std::env::var("ZMAIL_WINDOW_DECORATIONS").ok().as_deref(),
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
    )
}

/// [`window_decorations`] for the given override and desktop.
fn decorations_for(requested: Option<&str>, desktop: Option<&str>) -> WindowDecorations {
    match requested {
        Some("server") => WindowDecorations::Server,
        Some("client") => WindowDecorations::Client,
        _ if desktop
            .unwrap_or_default()
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("KDE")) =>
        {
            WindowDecorations::Server
        }
        _ => WindowDecorations::Client,
    }
}

// ---------------------------------------------------------------------------
// Enums shared across modules
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListMode {
    Mail,
    Search,
    Drafts,
    Outbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailMode {
    Reader,
    Compose,
    AccountSetup,
}

// ---------------------------------------------------------------------------
// WorkspaceView — the shell that owns the panes
// ---------------------------------------------------------------------------

/// The window's root view. It reads the local store, keeps the selection,
/// pushes what each pane shows into it, and handles the commands that span
/// panes (replying needs the selected thread and the account's identities,
/// for example). See `workspace.rs`.
struct WorkspaceView {
    bootstrap: AppBootstrap,
    store: Arc<LocalStore>,
    sync_service: SyncService,
    notifier: Arc<zmail_notify::NewMailNotifier<zmail_notify::OsNotificationSink>>,
    /// The root focus handle (`Workspace` key context).
    focus_handle: FocusHandle,
    mail_actions: Entity<MailActions>,
    sidebar: Entity<SidebarView>,
    message_list: Entity<MessageListView>,
    reader: Entity<ReaderView>,
    compose: Entity<ComposeView>,
    account_setup: Entity<AccountSetupView>,
    /// The toolbar search field.
    search_query: Entity<TextInput>,
    /// The command menu button (Windows and Linux; macOS uses the menu
    /// bar).
    app_menu: Entity<AppMenuButton>,
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
    list_mode: ListMode,
    detail_mode: DetailMode,
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
    _subscriptions: Vec<Subscription>,
}

#[cfg(test)]
mod tests {
    use gpui::WindowDecorations;

    use super::decorations_for;

    #[test]
    fn kde_gets_its_own_title_bar_and_the_rest_draw_one() {
        let is_server = |requested, desktop| {
            matches!(
                decorations_for(requested, desktop),
                WindowDecorations::Server
            )
        };
        assert!(!is_server(None, Some("GNOME")));
        assert!(!is_server(None, Some("ubuntu:GNOME")));
        assert!(!is_server(None, None));
        assert!(is_server(None, Some("KDE")));
        assert!(is_server(Some("server"), Some("GNOME")));
        assert!(!is_server(Some("client"), Some("KDE")));
    }
}
