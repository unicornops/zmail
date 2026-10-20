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
//! | `message_list` | `MessageListView`: threads, search results, drafts, outbox; its multi-row selection and bulk bar |
//! | `reader` | `ReaderView`: the open conversation and its actions |
//! | `compose` | `ComposeView`: the draft editor; `ComposeWindow`, one window per draft; `RecipientField` chips |
//! | `account_setup` | `AccountSetupView`: first-run onboarding and the add-account flow, with autodiscovery; `AddAccountWindow` |
//! | `mail_actions` | `MailActions`: store writes, `MailEvent`s and undo |
//! | `attachments` | opening and saving received attachments (Open, Save As, Save All) |
//! | `drag` | `DraggedThreads`: conversations dragged from the list onto sidebar folders |
//! | `move_picker` | `MovePicker`: the searchable Move to… folder picker |
//! | `command_palette` | `CommandPalette`: Secondary+K's search over commands, folders and accounts |
//! | `fuzzy` | the palette's fuzzy matcher |
//! | `cheat_sheet` | the keyboard shortcut cheat sheet, grouped from the bound keymap |
//! | `actions` | every command as a GPUI action |
//! | `keymap` | loads `keymaps/default.json` (and `keymaps/macos.json` on macOS); binds and switches the saved keymap |
//! | `menus` | `MenuModel`, the command menus; the native macOS menu bar; app-wide commands (Quit, Hide, …) |
//! | `app_menu` | `AppMenuButton`: the menu model behind a hamburger/overflow button (Windows, Linux) |
//! | `fonts` | registers the bundled Inter font on Linux |
//! | `pane` | pane focus order and the focused-pane indicator |
//! | `layout` | the responsive pane layout: breakpoints and pane widths |
//! | `components` | the component set: `Button`, `Menu`, `Select`, `Modal`, `ToastHost`, `SplitHandle`, `TextInput`, … and the gallery |
//! | `styles` | design tokens (type scale, spacing, density, `PlatformStyle`), `UiColors`, contrast helpers |
//! | `preferences` | loading and saving the persisted UI preferences (theme mode, sidebar, pane widths, density) |
//! | `widgets` | `toolbar_button`, `input_card`, `empty_state`, `icon`, … |
//! | `settings` | `SettingsWindow`: Accounts, Appearance, Privacy, Sending, About & Diagnostics |

use std::sync::Arc;

use gpui::{
    App, AppContext as _, AssetSource, Bounds, Entity, FocusHandle, Subscription, TitlebarOptions,
    WindowBounds, WindowDecorations, WindowOptions, point, px, size,
};
use zmail_core::{
    APP_NAME, AccountOverview, AccountSummary, AppBootstrap, DraftSummary, IdentitySummary,
    MailboxSummary, OutboxJobSummary, SearchHit, StoreStats, ThreadCounts, VirtualMailbox,
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
pub use settings::SettingsSection;

mod account_setup;
pub use account_setup::OnboardingStep;
mod actions;
mod app_menu;
mod attachments;
mod cheat_sheet;
mod command_palette;
mod compose;
mod drag;
mod fonts;
mod fuzzy;
mod keymap;
mod layout;
mod mail_actions;
mod menus;
mod message_list;
mod move_picker;
mod pane;
mod preferences;
mod reader;
mod search;
mod settings;
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
    /// (or the first-run onboarding when no account exists yet).
    #[default]
    Inbox,
    /// This stored draft in a compose window.
    Draft(i64),
    /// This stored draft just sent from the compose pane, with the Undo
    /// Send toast showing.
    Sending(i64),
    /// This thread in the reader with an inline reply open below it.
    Reply(i64),
    /// The reader on this thread, without marking it read.
    Thread(i64),
    /// The reader on this thread with every message expanded.
    ThreadExpanded(i64),
    /// The reader on this thread with the second paragraph of its current
    /// message selected.
    ThreadSelection(i64),
    /// The reader on this thread with the open-link dialog showing for the
    /// first link of its current message.
    ThreadLinkPrompt(i64),
    /// The Add Account window over the inbox, at its provider step.
    AddAccount,
    /// The first-run onboarding at this step, even when accounts exist
    /// (the colour and first-sync steps need one).
    Onboarding(OnboardingStep),
    /// The inbox with keyboard focus in the sidebar, which opens it as an
    /// overlay in a window too narrow for the sidebar pane.
    Sidebar,
    /// The inbox with its three newest threads selected, so the bulk
    /// action bar shows.
    MultiSelect,
    /// The outbox list.
    Outbox,
    /// The last mailbox with this query searched in it.
    Search(&'static str),
    /// The last mailbox with the command palette open and this query typed
    /// into it.
    CommandPalette(&'static str),
    /// The last mailbox with the keyboard shortcut cheat sheet open.
    KeyboardShortcuts,
    /// The inbox, with the Settings window open on this section.
    Settings(SettingsSection),
    /// The inbox, with Settings → Accounts open on this account, as the
    /// sidebar's Fix… opens it.
    SettingsAccount(i64),
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
            keymap::bind_keymap(saved_keymap(&store), cx);
            // After the keymap: the macOS menu bar reads key equivalents
            // from it.
            menus::init(cx);
            let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
            let bootstrap = bootstrap.clone();
            let store = store.clone();
            let sync_service = sync_service.clone();
            let secrets = secrets.clone();
            // The add-account flow reads these from any window.
            cx.set_global(account_setup::AccountSetupServices {
                store: store.clone(),
                secrets: secrets.clone(),
                sync_service: sync_service.clone(),
                network: zmail_autodiscover::Network::system(),
            });

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

/// The keymap saved in the UI preferences. An unreadable record means the
/// standard keymap; the workspace reports the store error when it loads the
/// rest of the preferences.
fn saved_keymap(store: &LocalStore) -> zmail_core::preferences::KeymapName {
    store
        .ui_preferences()
        .map(|preferences| preferences.keymap)
        .unwrap_or_default()
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
pub(crate) fn window_decorations() -> WindowDecorations {
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
    /// The first-run onboarding, which fills the window instead of the
    /// panes until it finishes. Present when the window opened with no
    /// account.
    onboarding: Option<(Entity<AccountSetupView>, Subscription)>,
    /// The toolbar search field.
    search_query: Entity<TextInput>,
    /// The command menu button (Windows and Linux; macOS uses the menu
    /// bar).
    app_menu: Entity<AppMenuButton>,
    /// Undo toasts at the bottom of the window.
    toasts: Entity<components::ToastHost>,
    /// The command palette, while it is open.
    command_palette: Option<command_palette::OpenPalette>,
    /// The palette's modal (focus trap and restore).
    palette_modal: components::ModalState,
    /// The keyboard shortcut cheat sheet's modal.
    shortcuts_modal: components::ModalState,
    /// What the open cheat sheet lists, read from the keymap as it opened.
    shortcut_groups: Vec<cheat_sheet::ShortcutGroup>,
    /// Opening and saving attachments: the warning before opening a
    /// program, and where Save As starts.
    attachments: attachments::AttachmentState,
    /// The toast offering to undo the latest mail change, and that change.
    undo_toast: Option<(components::ToastId, mail_actions::UndoId)>,
    /// Outbox jobs that were waiting to send when the window opened, so
    /// queued in an earlier session (for example during its Undo Send
    /// window). A toast says so once they have gone.
    last_session_jobs: Vec<i64>,
    /// The inline reply below a conversation, if one is open. The reader
    /// shows it while that conversation is open.
    inline_reply: Option<workspace::InlineReply>,
    /// The open compose windows, by draft, with the subscriptions to their
    /// editors.
    compose_windows: std::collections::HashMap<
        i64,
        (
            gpui::WindowHandle<compose::ComposeWindow>,
            [Subscription; 2],
        ),
    >,
    /// Set while a store refresh for mail events is queued, so a change to
    /// many threads refreshes once.
    mail_refresh_queued: bool,
    account: Option<AccountOverview>,
    accounts: Vec<AccountSummary>,
    identities: Vec<IdentitySummary>,
    stats: StoreStats,
    /// Every account's mailboxes, for the sidebar.
    all_mailboxes: Vec<MailboxSummary>,
    /// The selected account's mailboxes.
    mailboxes: Vec<MailboxSummary>,
    drafts: Vec<DraftSummary>,
    outbox_jobs: Vec<OutboxJobSummary>,
    search_results: Vec<SearchHit>,
    /// The query that produced `search_results`, shown in the list header
    /// (the search field itself may have been edited since).
    search_results_query: String,
    /// The query the latest search runs, whose results may still be
    /// loading (`search_results_query` is the one they show).
    search_pending_query: String,
    /// Where the shown search looks (folder, account or every account)
    /// and its running server searches.
    search_context: search::SearchContext,
    /// The live search waiting for typing to pause (see `search.rs`). Each
    /// edit replaces it, which cancels the one before.
    search_typing: gpui::Task<()>,
    /// The local search read running in the background. A newer read
    /// replaces it, which cancels it.
    search_read: gpui::Task<()>,
    /// The account the shell acts on: the selected mailbox's, or in a
    /// virtual mailbox the open thread's.
    selected_account_id: Option<i64>,
    /// The mailbox the list shows, or in a virtual mailbox the open
    /// thread's mailbox.
    selected_mailbox_id: Option<i64>,
    /// Set while the list shows a virtual mailbox (the Unified Inbox,
    /// Flagged, Sent) instead of `selected_mailbox_id`.
    virtual_mailbox: Option<VirtualMailbox>,
    /// The virtual mailbox's thread counts, read with the rest of the
    /// store state.
    virtual_counts: ThreadCounts,
    /// Unread conversations in Flagged, for the sidebar.
    flagged_unread: usize,
    /// Collapsed accounts and folders in the sidebar (persisted).
    sidebar_tree: zmail_core::preferences::SidebarTree,
    selected_thread_id: Option<i64>,
    /// The message list's multi-row selection; empty unless more than one
    /// row is selected. The conversation commands then act on all of them.
    list_selection: message_list::ListSelection,
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
    /// The bundled theme variants the app started with (`bootstrap.theme`
    /// is the active one).
    bundled_themes: [zmail_core::ThemeSummary; 2],
    /// The Zed theme family imported in Settings → Appearance, whose
    /// variants replace the bundled ones.
    imported_theme: Option<zmail_theme::ImportedTheme>,
    /// 12- or 24-hour clock for list and reader dates, from the locale.
    hour_cycle: zmail_core::time::HourCycle,
    /// Whether the user hid the sidebar (persisted in the UI preferences).
    sidebar_collapsed: bool,
    /// Message-list row density (persisted).
    density: zmail_core::preferences::Density,
    /// Whether the reader sits beside the list or replaces it (persisted).
    reading_pane: zmail_core::preferences::ReadingPanePosition,
    /// Which sender-safety warnings the reader shows (persisted).
    sender_warnings: zmail_core::preferences::SenderWarnings,
    /// The user's sidebar width (persisted); the layout clamps it to
    /// what the window fits.
    sidebar_width: f32,
    /// The user's message-list width (persisted).
    list_width: f32,
    /// Bumped by every pane resize so only the last one's timer saves.
    pane_widths_generation: u64,
    /// Whether the sidebar is open as an overlay (when it is not a pane).
    sidebar_overlay_open: bool,
    /// The pane the single-pane layout shows.
    single_pane: layout::SinglePane,
    /// The window's content width, which the pane layout follows.
    viewport_width: f32,
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
