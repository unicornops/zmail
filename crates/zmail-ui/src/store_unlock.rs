//! Unlocking the encrypted store before the workspace opens (ADR 0005).
//!
//! [`start`] runs `LocalStore::unlock` on the background executor: it
//! finds the store key in the keychain, and encrypts a store left by an
//! older Zmail first. A quick unlock opens the workspace straight away;
//! one that takes longer (a migration) opens the "Encrypting your mail"
//! window with its progress. When unlocking fails, the same window says
//! why and what can be done:
//!
//! - **The keychain is unavailable** (locked, access denied, no Secret
//!   Service): Retry. Zmail never opens the store without encryption.
//! - **The store is unreadable** (its key is gone): mail on the servers
//!   is safe, local-only mail (drafts, the outbox, queued changes) is
//!   lost. Start Over moves the old files aside and starts a new store.
//! - **Anything else** (a full disk, a failed check): the error and Retry.

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{
    App, AppContext as _, Bounds, Context, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    Role, SharedString, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div,
    point, prelude::*, px, relative, rgb, size,
};
use zmail_store::{
    LocalStore, MigrationProgress, MigrationReport, StoreKeychain, StoreOpenError,
    move_unreadable_store_aside,
};

use crate::a11y::{AccessibleExt, readable};
use crate::components::title_bar::TRAFFIC_LIGHT_POSITION;
use crate::components::{Banner, BannerTone, Button, ComponentTheme, TitleBar, window_frame};
use crate::styles::{
    SPACE_2, SPACE_3, SPACE_4, SPACE_6, TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TYPE_HEADLINE,
    TypeStyleExt, UiColors,
};

/// The window's title.
pub(crate) const WINDOW_TITLE: &str = "Zmail";
const WINDOW_WIDTH: f32 = 560.0;
const WINDOW_HEIGHT: f32 = 460.0;

/// How often the unlock's progress is read.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Polls before the window opens for an unlock still running: a normal
/// start finishes well inside this and never shows the window.
const POLLS_BEFORE_WINDOW: usize = 4;

/// Opens the workspace on an unlocked store; see [`Unlocker::on_unlocked`].
pub(crate) type OnUnlocked =
    dyn Fn(Arc<LocalStore>, Option<String>, &mut App) -> anyhow::Result<()>;

/// What to unlock, and what to do with the store once it is open.
pub(crate) struct Unlocker {
    /// The store's database.
    pub(crate) path: PathBuf,
    /// Where the store key is kept.
    pub(crate) keychain: Arc<dyn StoreKeychain>,
    /// Opens the workspace on the unlocked store, with a status message to
    /// show there (a finished migration, a fresh start). An error is shown
    /// in the unlock window.
    pub(crate) on_unlocked: Box<OnUnlocked>,
    /// The unlock window, once it has opened.
    window: RefCell<Option<WindowHandle<StoreUnlockView>>>,
    /// A message for the workspace from an earlier step (Start Over).
    notice: RefCell<Option<String>>,
}

impl Unlocker {
    pub(crate) fn new(
        path: PathBuf,
        keychain: Arc<dyn StoreKeychain>,
        on_unlocked: impl Fn(Arc<LocalStore>, Option<String>, &mut App) -> anyhow::Result<()> + 'static,
    ) -> Rc<Self> {
        Rc::new(Self {
            path,
            keychain,
            on_unlocked: Box::new(on_unlocked),
            window: RefCell::new(None),
            notice: RefCell::new(None),
        })
    }
}

/// What the unlock window shows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UnlockPhase {
    /// Unlocking, with the migration's progress once it reports any.
    Working(Option<MigrationProgress>),
    /// The keychain couldn't be reached or refused.
    KeychainUnavailable(String),
    /// The store's key is gone.
    Unreadable,
    /// Anything else.
    Failed(String),
}

/// An unlock in flight, shared with the background task.
enum Status {
    Running(Option<MigrationProgress>),
    Done(Result<(LocalStore, Option<MigrationReport>), StoreOpenError>),
}

/// Unlocks the store on the background executor and follows it: opens the
/// workspace when it succeeds, and the unlock window when it takes a while
/// or fails.
pub(crate) fn start(unlocker: Rc<Unlocker>, cx: &mut App) {
    let status = Arc::new(Mutex::new(Status::Running(None)));
    let path = unlocker.path.clone();
    let keychain = unlocker.keychain.clone();
    let shared = status.clone();
    cx.background_spawn(async move {
        let report = |progress| {
            if let Ok(mut status) = shared.lock() {
                *status = Status::Running(Some(progress));
            }
        };
        let result = LocalStore::unlock(&path, &*keychain, &report);
        if let Ok(mut status) = shared.lock() {
            *status = Status::Done(result);
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        let mut polls = 0;
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            polls += 1;
            let done = {
                let Ok(mut guard) = status.lock() else {
                    break;
                };
                match std::mem::replace(&mut *guard, Status::Running(None)) {
                    Status::Done(result) => Some(result),
                    Status::Running(progress) => {
                        *guard = Status::Running(progress);
                        let unlocker = unlocker.clone();
                        cx.update(|cx| {
                            if polls >= POLLS_BEFORE_WINDOW {
                                show(&unlocker, UnlockPhase::Working(progress), cx);
                            }
                        });
                        None
                    }
                }
            };
            if let Some(result) = done {
                let unlocker = unlocker.clone();
                cx.update(|cx| finish(&unlocker, result, cx));
                break;
            }
        }
    })
    .detach();
}

/// Acts on a finished unlock.
fn finish(
    unlocker: &Rc<Unlocker>,
    result: Result<(LocalStore, Option<MigrationReport>), StoreOpenError>,
    cx: &mut App,
) {
    match result {
        Ok((store, report)) => {
            let notice = unlocker
                .notice
                .borrow_mut()
                .take()
                .or_else(|| report.map(|report| migration_notice(&report)));
            match (unlocker.on_unlocked)(Arc::new(store), notice, cx) {
                Ok(()) => {
                    if let Some(handle) = unlocker.window.borrow_mut().take() {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                }
                Err(error) => show(unlocker, UnlockPhase::Failed(format!("{error:#}")), cx),
            }
        }
        Err(StoreOpenError::Unreadable { .. }) => show(unlocker, UnlockPhase::Unreadable, cx),
        Err(StoreOpenError::Keychain(error)) => show(
            unlocker,
            UnlockPhase::KeychainUnavailable(format!("{error:#}")),
            cx,
        ),
        Err(StoreOpenError::Other(error)) => {
            show(unlocker, UnlockPhase::Failed(format!("{error:#}")), cx);
        }
    }
}

/// What the workspace says after a migration.
pub(crate) fn migration_notice(report: &MigrationReport) -> String {
    let mut notice = "Your mail store is now encrypted.".to_owned();
    if let Some(directory) = report
        .plaintext_backups
        .first()
        .and_then(|backup| backup.parent())
    {
        notice.push_str(&format!(
            " Backups made before encryption are still unencrypted, in {}.",
            directory.display()
        ));
    }
    notice
}

/// Shows `phase` in the unlock window, opening it if needed.
fn show(unlocker: &Rc<Unlocker>, phase: UnlockPhase, cx: &mut App) {
    let existing = *unlocker.window.borrow();
    if let Some(handle) = existing {
        let phase = phase.clone();
        let shown = handle.update(cx, |view, _, cx| view.set_phase(phase, cx));
        if shown.is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let view_unlocker = unlocker.clone();
    let opened = cx.open_window(window_options(bounds), move |window, cx| {
        // Without a store there is nothing to go back to: closing the
        // window quits.
        window.on_window_should_close(cx, |_, cx| {
            cx.quit();
            true
        });
        cx.new(|cx| StoreUnlockView::new(phase, Some(view_unlocker), window, cx))
    });
    match opened {
        Ok(handle) => *unlocker.window.borrow_mut() = Some(handle),
        Err(error) => {
            eprintln!("zmail: the unlock window could not open: {error:#}");
            cx.quit();
        }
    }
}

/// Opens the unlock window on `phase` with inert buttons, for the
/// screenshot fixture.
pub(crate) fn show_fixture(phase: UnlockPhase, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let opened = cx.open_window(window_options(bounds), move |window, cx| {
        cx.new(|cx| StoreUnlockView::new(phase, None, window, cx))
    });
    if let Err(error) = opened {
        panic!("failed to open the unlock window: {error:#}");
    }
}

/// The unlock window's options: the main window's chrome, smaller.
fn window_options(bounds: Bounds<gpui::Pixels>) -> WindowOptions {
    WindowOptions {
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
        window_min_size: Some(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT))),
        app_id: Some(crate::APP_ID.to_owned()),
        ..Default::default()
    }
}

/// The unlock window's content.
pub(crate) struct StoreUnlockView {
    phase: UnlockPhase,
    /// `None` in the screenshot fixture, whose buttons do nothing.
    unlocker: Option<Rc<Unlocker>>,
    focus_handle: FocusHandle,
}

impl StoreUnlockView {
    pub(crate) fn new(
        phase: UnlockPhase,
        unlocker: Option<Rc<Unlocker>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            phase,
            unlocker,
            focus_handle,
        }
    }

    /// What the window shows.
    #[cfg(test)]
    pub(crate) fn phase(&self) -> &UnlockPhase {
        &self.phase
    }

    fn set_phase(&mut self, phase: UnlockPhase, cx: &mut Context<Self>) {
        if self.phase != phase {
            self.phase = phase;
            cx.notify();
        }
    }

    /// Retry: unlock again, as at start.
    pub(crate) fn retry(&mut self, cx: &mut Context<Self>) {
        self.set_phase(UnlockPhase::Working(None), cx);
        if let Some(unlocker) = self.unlocker.clone() {
            cx.defer(move |cx| start(unlocker, cx));
        }
    }

    /// Start Over: move the unreadable store aside and start a new one.
    pub(crate) fn start_over(&mut self, cx: &mut Context<Self>) {
        let Some(unlocker) = self.unlocker.clone() else {
            return;
        };
        match move_unreadable_store_aside(&unlocker.path) {
            Ok(aside) => {
                *unlocker.notice.borrow_mut() = Some(format!(
                    "Started a new mail store. The unreadable one was moved to {}; delete it \
                     once you're sure its key won't turn up.",
                    aside.display()
                ));
                self.retry(cx);
            }
            Err(error) => self.set_phase(
                UnlockPhase::Failed(format!("The old store could not be moved aside: {error:#}")),
                cx,
            ),
        }
    }

    fn render_body(&self, colors: &UiColors, cx: &mut Context<Self>) -> gpui::Div {
        match &self.phase {
            UnlockPhase::Working(progress) => {
                let (status, fraction) = progress_text(*progress);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_4))
                    .child(heading(
                        "Encrypting your mail",
                        "Zmail now encrypts everything it keeps on this computer. This happens \
                         once; quitting is safe, and it carries on next time.",
                        colors,
                    ))
                    .child(progress_bar(fraction, colors))
                    .child(
                        div()
                            .id("store-unlock-status")
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.muted))
                            .child(readable("store-unlock-status-text", status)),
                    )
            }
            UnlockPhase::KeychainUnavailable(detail) => div()
                .flex()
                .flex_col()
                .gap(px(SPACE_4))
                .child(heading(
                    "Zmail can't reach the keychain",
                    "The key that unlocks your mail is kept in the system keychain. Unlock the \
                     keychain (or allow Zmail to use it), then try again. Zmail never opens \
                     your mail unencrypted instead.",
                    colors,
                ))
                .child(
                    Banner::new("store-unlock-keychain-error", detail.clone())
                        .tone(BannerTone::Caution),
                )
                .child(self.footer(&[Action::Quit, Action::Retry], cx)),
            UnlockPhase::Unreadable => div()
                .flex()
                .flex_col()
                .gap(px(SPACE_4))
                .child(heading(
                    "Zmail can't open your mail store",
                    "Its key is no longer in the system keychain, for example after the keychain \
                     was reset or the data was copied from another computer. Without the key \
                     nobody can read it.",
                    colors,
                ))
                .child(
                    Banner::new(
                        "store-unlock-unreadable",
                        "Mail on your servers is safe and downloads again. Drafts, mail waiting \
                         in the outbox, changes not yet sent to a server, and your account \
                         settings exist only in this store and are lost.",
                    )
                    .title("Starting over")
                    .tone(BannerTone::Danger),
                )
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(
                            "Start Over keeps the old files in a folder beside the new store, \
                             in case the key turns up again.",
                        ),
                )
                .child(self.footer(&[Action::Quit, Action::StartOver], cx)),
            UnlockPhase::Failed(detail) => div()
                .flex()
                .flex_col()
                .gap(px(SPACE_4))
                .child(heading(
                    "Zmail couldn't open your mail",
                    "Nothing was lost; the store is as it was. Fix the problem below (free some \
                     disk space, for example) and try again.",
                    colors,
                ))
                .child(Banner::new("store-unlock-error", detail.clone()).tone(BannerTone::Caution))
                .child(self.footer(&[Action::Quit, Action::Retry], cx)),
        }
    }

    fn footer(&self, actions: &[Action], cx: &mut Context<Self>) -> gpui::Div {
        let mut footer = div().flex().justify_end().gap(px(SPACE_2)).pt(px(SPACE_2));
        for action in actions {
            let button = match action {
                Action::Quit => Button::new("store-unlock-quit", "Quit")
                    .on_click(cx.listener(|_, _, _, cx| cx.quit())),
                Action::Retry => Button::new("store-unlock-retry", "Try Again")
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                Action::StartOver => Button::new("store-unlock-start-over", "Start Over")
                    .danger()
                    .on_click(cx.listener(|this, _, _, cx| this.start_over(cx))),
            };
            let id = action.id();
            footer = footer.child(div().debug_selector(move || id.into()).child(button));
        }
        footer
    }
}

/// The buttons the window can show.
#[derive(Clone, Copy)]
enum Action {
    Quit,
    Retry,
    StartOver,
}

impl Action {
    fn id(self) -> &'static str {
        match self {
            Self::Quit => "store-unlock-quit",
            Self::Retry => "store-unlock-retry",
            Self::StartOver => "store-unlock-start-over",
        }
    }
}

/// The status line and the bar's fill for `progress`.
fn progress_text(progress: Option<MigrationProgress>) -> (String, Option<f32>) {
    match progress {
        None => ("Unlocking…".to_owned(), None),
        Some(MigrationProgress::Database { written, total }) => {
            let fraction = if total == 0 {
                1.0
            } else {
                (written as f64 / total as f64).clamp(0.0, 1.0) as f32
            };
            (
                format!("Encrypting messages: {}%", (fraction * 100.0).round()),
                Some(fraction),
            )
        }
        Some(MigrationProgress::Verifying) => ("Checking the encrypted copy…".to_owned(), None),
        Some(MigrationProgress::Attachments { done, total }) => (
            format!("Encrypting attachments: {done} of {total}"),
            Some(if total == 0 {
                1.0
            } else {
                done as f32 / total as f32
            }),
        ),
        Some(MigrationProgress::Finished) => ("Opening your mail…".to_owned(), Some(1.0)),
    }
}

fn heading(title: &'static str, detail: &'static str, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_2))
        .child(
            div()
                .id("store-unlock-heading")
                .accessible(Role::Heading, SharedString::from(title))
                .aria_level(1)
                .type_style(TYPE_HEADLINE)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .type_style(TYPE_BODY)
                .text_color(rgb(colors.muted))
                .child(detail),
        )
}

/// A bar filled to `fraction`, or a full, quiet track while the amount is
/// unknown.
fn progress_bar(fraction: Option<f32>, colors: &UiColors) -> gpui::Div {
    div()
        .debug_selector(|| "store-unlock-progress".into())
        .h(px(6.0))
        .w_full()
        .rounded_full()
        .bg(rgb(colors.border))
        .child(
            div()
                .h_full()
                .rounded_full()
                .bg(rgb(colors.accent))
                .w(relative(fraction.unwrap_or(0.0))),
        )
}

impl Focusable for StoreUnlockView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for StoreUnlockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let body = self.render_body(&colors, cx);
        let root = div()
            .id("store-unlock-window")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme.platform.ui_font.family())
            .bg(rgb(colors.background))
            .text_color(rgb(colors.foreground))
            .child(
                TitleBar::new("store-unlock-title-bar").child(
                    div()
                        .flex_1()
                        .px(px(SPACE_2))
                        .type_style(TYPE_BODY_STRONG)
                        .child(WINDOW_TITLE),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(SPACE_6))
                    .py(px(SPACE_3))
                    .child(body),
            );
        window_frame(root, window, cx)
    }
}
