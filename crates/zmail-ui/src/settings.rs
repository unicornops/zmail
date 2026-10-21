//! The Settings window: one per app, opened from the app menu (Settings…
//! in the macOS application menu) or with Secondary+, (comma).
//!
//! [`SettingsWindow`] is the window's root view: a section list on the
//! leading side and the open section beside it. Each section is its own
//! entity in a submodule:
//!
//! | Module | Section |
//! |--------|---------|
//! | `accounts` | each account's status, colour, cache budget, Re-authenticate and Remove; Add Account |
//! | `appearance` | theme mode, imported Zed theme, row density, reading pane |
//! | `keyboard` | Standard or Gmail shortcuts, and the shortcut list with search |
//! | `privacy` | remote content (always blocked), the sender-warning level |
//! | `sending` | the undo-send delay |
//! | `about` | version, upstream pins, queues, undelivered changes, Copy diagnostics |
//!
//! Settings are local-first like the rest of the UI: sections read the
//! store and write [`UiPreferences`] back to it, then ask the main window
//! to apply them ([`apply_to_workspace`]), which redraws every open window
//! with the new theme or layout. When the main window changes a preference
//! itself (the View menu's theme and density toggles), it calls
//! [`preferences_changed`] so an open Settings window shows the new value.
//!
//! [`UiPreferences`]: zmail_core::preferences::UiPreferences

mod about;
mod accounts;
mod appearance;
mod keyboard;
mod privacy;
mod sending;

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use gpui::{
    AnyView, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Global, IntoElement, Orientation, Render, Role, ScrollHandle, SharedString,
    Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div, point,
    prelude::*, px, relative, rgb, size,
};
use zmail_core::{AccountColor, AppBootstrap, UpstreamPins};
use zmail_security::SecretStore;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use self::about::AboutSettings;
use self::accounts::AccountsSettings;
use self::appearance::AppearanceSettings;
use self::keyboard::KeyboardSettings;
use self::privacy::PrivacySettings;
use self::sending::SendingSettings;
use crate::WorkspaceView;
use crate::a11y::{AccessibleExt, readable};
use crate::actions::{SelectNext, SelectPrevious};
use crate::components::title_bar::TRAFFIC_LIGHT_POSITION;
use crate::components::{
    ComponentTheme, HitTargetExt, Scrollbar, TitleBar, Toast, ToastHost, ToastKind, window_frame,
};
use crate::styles::{
    ICON_SIZE_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TYPE_BODY, TYPE_BODY_STRONG,
    TYPE_CAPTION, TYPE_TITLE, TextScale, TypeStyleExt, UiColors, rems_from_px,
};
use crate::widgets::{EllipsisExt, HoverExt, account_dot, icon, interactive_background};

/// The key context of the Settings window's root.
pub(crate) const KEY_CONTEXT: &str = "SettingsWindow";
/// The key context of the section list.
pub(crate) const NAV_KEY_CONTEXT: &str = "SettingsNav";
/// The Settings window's title (the screenshot script finds it by this).
pub(crate) const WINDOW_TITLE: &str = "Settings";
/// A new Settings window's size.
const SETTINGS_WINDOW_WIDTH: f32 = 820.0;
const SETTINGS_WINDOW_HEIGHT: f32 = 600.0;
/// The smallest the Settings window gets.
const SETTINGS_WINDOW_MIN_WIDTH: f32 = 620.0;
const SETTINGS_WINDOW_MIN_HEIGHT: f32 = 420.0;
/// Width of the section list.
const NAV_WIDTH: f32 = 200.0;
/// Height of a row in the section list.
const NAV_ROW_HEIGHT: f32 = 32.0;
/// Widest a section's content gets, so rows stay readable in a wide window.
const CONTENT_MAX_WIDTH: f32 = 680.0;
/// Narrowest a setting's label and description get before its control
/// wraps below them.
const SETTING_TEXT_MIN_WIDTH: f32 = 240.0;
/// How often an open section re-reads the store (sync and outbox state
/// change in the background).
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// A section of the Settings window, in list order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// Each account: status, colour, cache budget, Re-authenticate and
    /// Remove.
    #[default]
    Accounts,
    /// Theme, imported theme, density and reading pane.
    Appearance,
    /// The keymap and the shortcut list.
    Keyboard,
    /// Remote content and sender warnings.
    Privacy,
    /// The undo-send delay.
    Sending,
    /// Version, queues, undelivered changes and diagnostics.
    About,
}

impl SettingsSection {
    /// Every section, in list order.
    pub(crate) const ALL: [SettingsSection; 6] = [
        SettingsSection::Accounts,
        SettingsSection::Appearance,
        SettingsSection::Keyboard,
        SettingsSection::Privacy,
        SettingsSection::Sending,
        SettingsSection::About,
    ];

    /// The name in the section list and the section's heading.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Accounts => "Accounts",
            Self::Appearance => "Appearance",
            Self::Keyboard => "Keyboard",
            Self::Privacy => "Privacy",
            Self::Sending => "Sending",
            Self::About => "About & Diagnostics",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Accounts => "user",
            Self::Appearance => "sun",
            Self::Keyboard => "keyboard",
            Self::Privacy => "shield",
            Self::Sending => "send",
            Self::About => "info",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Accounts => "accounts",
            Self::Appearance => "appearance",
            Self::Keyboard => "keyboard",
            Self::Privacy => "privacy",
            Self::Sending => "sending",
            Self::About => "about",
        }
    }
}

// ---------------------------------------------------------------------------
// The host: what Settings needs from the app
// ---------------------------------------------------------------------------

/// What the Settings window works with: the store, the keychain, the sync
/// service, the app's build details and the main window to apply changes to. The main
/// window installs it when it opens.
pub(crate) struct SettingsHost {
    pub(crate) store: Arc<LocalStore>,
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) sync_service: SyncService,
    pub(crate) app_version: String,
    pub(crate) upstream: UpstreamPins,
    pub(crate) worker_threads: usize,
    workspace: WindowHandle<WorkspaceView>,
    window: Option<WindowHandle<SettingsWindow>>,
}

impl Global for SettingsHost {}

impl SettingsHost {
    /// Install the host for the main window `workspace`.
    pub(crate) fn install(
        bootstrap: &AppBootstrap,
        store: Arc<LocalStore>,
        secrets: Arc<dyn SecretStore>,
        sync_service: SyncService,
        workspace: WindowHandle<WorkspaceView>,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            store,
            secrets,
            sync_service,
            app_version: bootstrap.app_version.clone(),
            upstream: bootstrap.upstream.clone(),
            worker_threads: bootstrap.runtime.worker_threads,
            workspace,
            window: None,
        });
    }

    /// The installed host. Settings views only exist once it is.
    pub(crate) fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

/// Opens the Settings window on `section` (or where it was), or brings the
/// open one to the front. Does nothing before the main window exists.
pub(crate) fn open(section: Option<SettingsSection>, cx: &mut App) {
    let Some(host) = cx.try_global::<SettingsHost>() else {
        return;
    };
    if let Some(handle) = host.window {
        let shown = handle.update(cx, |root, window, cx| {
            if let Some(section) = section {
                root.show_section(section, cx);
            }
            window.activate_window();
        });
        if shown.is_ok() {
            return;
        }
    }
    match SettingsWindow::open(section.unwrap_or_default(), cx) {
        Ok(handle) => cx.global_mut::<SettingsHost>().window = Some(handle),
        Err(error) => {
            let workspace = host_workspace(cx);
            let _ = workspace.update(cx, |view, _, cx| {
                view.show_error(format!("Settings could not open: {error:#}"), cx);
            });
        }
    }
}

/// Opens Settings → Accounts on `account_id`: the sidebar's Fix… on an
/// account that cannot sync.
pub(crate) fn open_account(account_id: i64, cx: &mut App) {
    open(Some(SettingsSection::Accounts), cx);
    if let Some(handle) = window(cx) {
        let _ = handle.update(cx, |root, _, cx| {
            root.accounts
                .update(cx, |accounts, cx| accounts.focus_account(account_id, cx));
        });
    }
}

/// Runs `update` on the main window's shell, for example to re-read the
/// store after Settings changed an account.
pub(crate) fn update_workspace(
    cx: &mut App,
    update: impl FnOnce(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>),
) {
    let Some(host) = cx.try_global::<SettingsHost>() else {
        return;
    };
    let workspace = host.workspace;
    let _ = workspace.update(cx, update);
}

/// The open Settings window, if any.
pub(crate) fn window(cx: &App) -> Option<WindowHandle<SettingsWindow>> {
    cx.try_global::<SettingsHost>()?.window
}

fn host_workspace(cx: &App) -> WindowHandle<WorkspaceView> {
    SettingsHost::get(cx).workspace
}

/// Applies the saved preferences to the main window (and through it every
/// window). Sections call this after each change they save.
pub(crate) fn apply_to_workspace(cx: &mut App) {
    let Some(host) = cx.try_global::<SettingsHost>() else {
        return;
    };
    let workspace = host.workspace;
    let _ = workspace.update(cx, |view, window, cx| view.apply_preferences(window, cx));
}

/// Tells an open Settings window that the preferences changed elsewhere,
/// so it shows the current values.
pub(crate) fn preferences_changed(cx: &mut App) {
    if let Some(handle) = window(cx) {
        let _ = handle.update(cx, |root, _, cx| root.reload(cx));
    }
}

// ---------------------------------------------------------------------------
// Events from the sections
// ---------------------------------------------------------------------------

/// What a section tells the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsEvent {
    /// Confirm something with a toast.
    Status(String),
    /// Report a failure with an error toast.
    Error(String),
}

// ---------------------------------------------------------------------------
// SettingsWindow
// ---------------------------------------------------------------------------

/// The Settings window's root view.
pub(crate) struct SettingsWindow {
    section: SettingsSection,
    nav_focus: FocusHandle,
    accounts: Entity<AccountsSettings>,
    appearance: Entity<AppearanceSettings>,
    keyboard: Entity<KeyboardSettings>,
    privacy: Entity<PrivacySettings>,
    sending: Entity<SendingSettings>,
    about: Entity<AboutSettings>,
    toasts: Entity<ToastHost>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindow {
    fn open(section: SettingsSection, cx: &mut App) -> Result<WindowHandle<Self>> {
        let bounds = Bounds::centered(
            None,
            size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT)),
            cx,
        );
        cx.open_window(
            WindowOptions {
                // Same chrome as the main window: Zmail draws the title bar.
                titlebar: Some(TitlebarOptions {
                    title: Some(WINDOW_TITLE.into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(
                        px(TRAFFIC_LIGHT_POSITION.0),
                        px(TRAFFIC_LIGHT_POSITION.1),
                    )),
                }),
                app_owns_titlebar_drag: true,
                window_decorations: Some(crate::window_decorations()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(
                    px(SETTINGS_WINDOW_MIN_WIDTH),
                    px(SETTINGS_WINDOW_MIN_HEIGHT),
                )),
                app_id: Some(crate::APP_ID.to_owned()),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| Self::new(section, window, cx)),
        )
    }

    fn new(section: SettingsSection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let accounts = cx.new(|cx| AccountsSettings::new(window, cx));
        let appearance = cx.new(|cx| AppearanceSettings::new(window, cx));
        let keyboard = cx.new(KeyboardSettings::new);
        let privacy = cx.new(PrivacySettings::new);
        let sending = cx.new(SendingSettings::new);
        let about = cx.new(AboutSettings::new);
        let toasts = cx.new(|_| ToastHost::new());
        let subscriptions = vec![
            cx.subscribe(&accounts, Self::on_section_event),
            cx.subscribe(&appearance, Self::on_section_event),
            cx.subscribe(&sending, Self::on_section_event),
            cx.subscribe(&keyboard, Self::on_section_event),
            cx.subscribe(&privacy, Self::on_section_event),
            cx.subscribe(&about, Self::on_section_event),
        ];
        let nav_focus = cx.focus_handle();
        window.focus(&nav_focus, cx);

        // Sync and the outbox change in the background; the open section
        // follows them. A weak handle ends the loop with the window.
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                loop {
                    cx.background_executor().timer(REFRESH_INTERVAL).await;
                    if entity.update(cx, |root, cx| root.refresh(cx)).is_err() {
                        break;
                    }
                }
            })
            .detach();

        Self {
            section,
            nav_focus,
            accounts,
            appearance,
            keyboard,
            privacy,
            sending,
            about,
            toasts,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The open section.
    #[cfg(test)]
    pub(crate) fn section(&self) -> SettingsSection {
        self.section
    }

    /// Show `section`.
    pub(crate) fn show_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        if self.section != section {
            self.section = section;
            self.scroll.set_offset(point(px(0.0), px(0.0)));
            self.refresh(cx);
            if section == SettingsSection::Accounts {
                self.accounts
                    .update(cx, |accounts, cx| accounts.measure_usage(cx));
            }
        }
        cx.notify();
    }

    /// Re-read every section's preferences (they changed elsewhere).
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.appearance.update(cx, |section, cx| section.reload(cx));
        self.sending.update(cx, |section, cx| section.reload(cx));
        self.privacy.update(cx, |section, cx| section.reload(cx));
        self.keyboard.update(cx, |section, cx| section.reload(cx));
    }

    /// Re-read the open section's store state.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        match self.section {
            SettingsSection::Accounts => self
                .accounts
                .update(cx, |accounts, cx| accounts.refresh(cx)),
            SettingsSection::About => self.about.update(cx, |about, cx| about.refresh(cx)),
            SettingsSection::Appearance
            | SettingsSection::Keyboard
            | SettingsSection::Privacy
            | SettingsSection::Sending => {}
        }
    }

    /// The Accounts section.
    #[cfg(test)]
    pub(crate) fn accounts(&self) -> &Entity<AccountsSettings> {
        &self.accounts
    }

    /// The Appearance section.
    #[cfg(test)]
    pub(crate) fn appearance(&self) -> &Entity<AppearanceSettings> {
        &self.appearance
    }

    /// The Keyboard section.
    #[cfg(test)]
    pub(crate) fn keyboard(&self) -> &Entity<KeyboardSettings> {
        &self.keyboard
    }

    /// The Privacy section.
    #[cfg(test)]
    pub(crate) fn privacy(&self) -> &Entity<PrivacySettings> {
        &self.privacy
    }

    /// The Sending section.
    #[cfg(test)]
    pub(crate) fn sending(&self) -> &Entity<SendingSettings> {
        &self.sending
    }

    /// The About & Diagnostics section.
    #[cfg(test)]
    pub(crate) fn about(&self) -> &Entity<AboutSettings> {
        &self.about
    }

    /// The messages of the toasts showing.
    #[cfg(test)]
    pub(crate) fn toast_messages(&self, cx: &App) -> Vec<String> {
        self.toasts
            .read(cx)
            .visible()
            .into_iter()
            .map(|(_, message)| message.to_string())
            .collect()
    }

    fn on_section_event<T>(&mut self, _: Entity<T>, event: &SettingsEvent, cx: &mut Context<Self>) {
        let (message, kind) = match event {
            SettingsEvent::Status(message) => (message, ToastKind::Info),
            SettingsEvent::Error(message) => (message, ToastKind::Error),
        };
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(Toast::new(message.clone()).kind(kind), cx);
        });
    }

    fn step_section(&mut self, forward: bool, cx: &mut Context<Self>) {
        let sections = SettingsSection::ALL;
        let index = sections
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or(0);
        let next = if forward {
            (index + 1).min(sections.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.show_section(sections[next], cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step_section(true, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.step_section(false, cx);
    }

    fn render_nav(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let focused = self.nav_focus.is_focused(window);
        div()
            .id("settings-nav")
            .key_context(NAV_KEY_CONTEXT)
            .track_focus(&self.nav_focus.clone().tab_index(0).tab_stop(true))
            .accessible(Role::TabList, SharedString::from("Settings sections"))
            .aria_orientation(Orientation::Vertical)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_1))
            // Grows with the text size, but leaves the content most of
            // the window.
            .w(rems_from_px(NAV_WIDTH))
            .max_w(relative(0.34))
            .h_full()
            .p(px(SPACE_2))
            .bg(rgb(colors.surface))
            .border_r_1()
            .border_color(rgb(colors.border))
            .children(SettingsSection::ALL.into_iter().map(|section| {
                let selected = section == self.section;
                // A full accent row while the list has focus, grey when
                // it does not (plan §2).
                let (fill, text) = if selected && focused {
                    (colors.selection_background, colors.selection_foreground)
                } else {
                    (
                        interactive_background(selected, colors.surface, &colors),
                        colors.foreground,
                    )
                };
                let id = format!("settings-nav-{}", section.id());
                div()
                    .id(SharedString::from(id.clone()))
                    .hit_target(move || id.clone())
                    .accessible(Role::Tab, SharedString::from(section.label()))
                    .aria_selected(selected)
                    // Up and Down move between sections while focus stays
                    // on the list.
                    .when(selected && focused, |this| this.aria_active_descendant())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .min_h(TextScale::get(cx).fit(NAV_ROW_HEIGHT, TYPE_BODY))
                    .px(px(SPACE_2))
                    .rounded(px(ComponentTheme::get(cx).platform.control_radius))
                    .bg(rgb(fill))
                    .hover_bg(selected, &colors)
                    .cursor_pointer()
                    .type_style(if selected {
                        TYPE_BODY_STRONG
                    } else {
                        TYPE_BODY
                    })
                    .text_color(rgb(text))
                    .on_click(cx.listener(move |root, _, window, cx| {
                        window.focus(&root.nav_focus, cx);
                        root.show_section(section, cx);
                    }))
                    .child(icon(section.icon(), ICON_SIZE_SM, text))
                    .child(div().flex_1().min_w_0().ellipsis().child(section.label()))
            }))
    }

    fn section_view(&self) -> AnyView {
        match self.section {
            SettingsSection::Accounts => self.accounts.clone().into(),
            SettingsSection::Appearance => self.appearance.clone().into(),
            SettingsSection::Keyboard => self.keyboard.clone().into(),
            SettingsSection::Privacy => self.privacy.clone().into(),
            SettingsSection::Sending => self.sending.clone().into(),
            SettingsSection::About => self.about.clone().into(),
        }
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let root = div()
            .id("settings-window")
            .relative()
            .key_context(KEY_CONTEXT)
            .size_full()
            .font_family(theme.platform.ui_font.family())
            .text_color(rgb(colors.foreground))
            .bg(rgb(colors.background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(
                        TitleBar::new("settings-title-bar").child(
                            div()
                                .flex_1()
                                .px(px(SPACE_2))
                                .type_style(TYPE_BODY_STRONG)
                                .child(WINDOW_TITLE),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .child(self.render_nav(window, cx))
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(
                                        div()
                                            .id("settings-content")
                                            .accessible(
                                                Role::TabPanel,
                                                SharedString::from(self.section.label()),
                                            )
                                            .size_full()
                                            .overflow_y_scroll()
                                            .track_scroll(&self.scroll)
                                            .child(
                                                div()
                                                    .w_full()
                                                    .max_w(rems_from_px(CONTENT_MAX_WIDTH))
                                                    .px(px(SPACE_6))
                                                    .py(px(SPACE_4))
                                                    .child(self.section_view()),
                                            ),
                                    )
                                    .child(Scrollbar::vertical("settings-scrollbar", &self.scroll)),
                            ),
                    ),
            )
            .child(self.toasts.clone());
        window_frame(root, window, cx)
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.nav_focus.clone()
    }
}

// ---------------------------------------------------------------------------
// Shared section layout
// ---------------------------------------------------------------------------

/// A setting label with an account's colour dot before it.
pub(crate) fn dotted_label(color: AccountColor, label: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_2))
        .child(account_dot(color))
        .child(div().min_w_0().child(label))
}

/// A muted value on the trailing side of a row, such as a version.
pub(crate) fn row_value(value: impl IntoElement, colors: &UiColors) -> gpui::Div {
    div()
        .type_style(TYPE_CAPTION)
        .text_color(rgb(colors.muted))
        .child(value)
}

/// A section's heading.
pub(crate) fn section_heading(title: &str) -> gpui::Stateful<gpui::Div> {
    div()
        .id("settings-section-heading")
        .pb(px(SPACE_2))
        .type_style(TYPE_TITLE)
        .accessible(Role::Heading, title.to_owned())
        .aria_level(1)
        .child(title.to_owned())
}

/// A group heading inside a section, such as "Undelivered changes".
pub(crate) fn group_heading(title: &str, colors: &UiColors) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(format!("settings-group-{title}")))
        .accessible(Role::Heading, title.to_owned())
        .aria_level(2)
        .pt(px(SPACE_6))
        .pb(px(SPACE_1))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(colors.muted))
        .child(title.to_uppercase())
}

/// One setting: its label and a sentence about it on the leading side, the
/// control on the trailing side, and a divider below. When the two do not
/// fit side by side (large text in a narrow window), the control wraps
/// below the text.
pub(crate) fn setting_row(
    label: impl IntoElement,
    description: Option<SharedString>,
    control: impl IntoElement,
    colors: &UiColors,
) -> gpui::Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SPACE_4))
        .py(px(SPACE_3))
        .border_b_1()
        .border_color(rgb(colors.border))
        .child(setting_text(label, description, colors).min_w(rems_from_px(SETTING_TEXT_MIN_WIDTH)))
        .child(div().flex_none().max_w_full().child(control))
}

/// A setting's label and description.
pub(crate) fn setting_text(
    label: impl IntoElement,
    description: Option<SharedString>,
    colors: &UiColors,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_1))
        .flex_1()
        .min_w_0()
        .child(div().type_style(TYPE_BODY_STRONG).child(label))
        .when_some(description, |this, description| {
            this.child(
                div()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(readable(
                        SharedString::from(format!("setting-description-{description}")),
                        description,
                    )),
            )
        })
}

impl EventEmitter<SettingsEvent> for AccountsSettings {}
impl EventEmitter<SettingsEvent> for AppearanceSettings {}
impl EventEmitter<SettingsEvent> for KeyboardSettings {}
impl EventEmitter<SettingsEvent> for PrivacySettings {}
impl EventEmitter<SettingsEvent> for SendingSettings {}
impl EventEmitter<SettingsEvent> for AboutSettings {}
