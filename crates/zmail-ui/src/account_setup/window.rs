//! `AddAccountWindow`: the add-account flow in a window of its own.
//!
//! There is at most one; opening it again brings it forward. When the flow
//! stores an account, every main window refreshes and selects it. The flow
//! closes the window when it finishes, and Escape on its first step or the
//! window's close button cancel it before anything is stored.

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Global, IntoElement, Render, SharedString,
    Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div, point,
    prelude::*, px, rgb, size,
};

use super::{AccountSetupEvent, AccountSetupView};
use crate::WorkspaceView;
use crate::components::title_bar::TRAFFIC_LIGHT_POSITION;
use crate::components::{ComponentTheme, TitleBar, Toast, ToastHost, ToastKind, window_frame};
use crate::styles::{SPACE_2, TYPE_BODY_STRONG, TypeStyleExt};

/// The window's title, which the screenshot script also finds it by.
pub(crate) const WINDOW_TITLE: &str = "Add Account";
const WINDOW_WIDTH: f32 = 640.0;
const WINDOW_HEIGHT: f32 = 780.0;
const WINDOW_MIN_WIDTH: f32 = 480.0;
const WINDOW_MIN_HEIGHT: f32 = 520.0;

/// The open Add Account window, if any.
struct OpenWindow(WindowHandle<AddAccountWindow>);

impl Global for OpenWindow {}

/// The root view of the Add Account window.
pub(crate) struct AddAccountWindow {
    setup: Entity<AccountSetupView>,
    toasts: Entity<ToastHost>,
    _subscription: Subscription,
}

/// Opens the window, or brings the open one forward with `notice`.
pub(super) fn open(notice: Option<SharedString>, cx: &mut App) {
    if let Some(OpenWindow(handle)) = cx.try_global::<OpenWindow>() {
        let handle = *handle;
        let notice = notice.clone();
        let reused = handle.update(cx, |this, window, cx| {
            window.activate_window();
            if notice.is_some() {
                this.setup
                    .update(cx, |setup, cx| setup.set_notice(notice, cx));
            }
        });
        if reused.is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let opened = cx.open_window(
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
            window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
            app_id: Some(crate::APP_ID.to_owned()),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| AddAccountWindow::new(notice, window, cx)),
    );
    match opened {
        Ok(handle) => cx.set_global(OpenWindow(handle)),
        Err(error) => {
            let message = format!("The Add Account window could not open: {error:#}");
            cx.defer(move |cx| for_each_workspace(cx, |view, cx| view.show_error(&message, cx)));
        }
    }
}

/// The add-account window the app has open, for tests.
#[cfg(test)]
pub(crate) fn open_window(cx: &App) -> Option<WindowHandle<AddAccountWindow>> {
    cx.try_global::<OpenWindow>()
        .map(|OpenWindow(handle)| *handle)
}

/// Runs `update` on every main window's view.
fn for_each_workspace(
    cx: &mut App,
    update: impl Fn(&mut WorkspaceView, &mut Context<WorkspaceView>),
) {
    for handle in cx.windows() {
        if let Some(workspace) = handle.downcast::<WorkspaceView>() {
            let _ = workspace.update(cx, |view, _, cx| update(view, cx));
        }
    }
}

impl AddAccountWindow {
    fn new(notice: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let setup = cx.new(|cx| AccountSetupView::new(false, window, cx));
        setup.update(cx, |setup, cx| {
            setup.set_notice(notice, cx);
            setup.focus_stage(window, cx);
        });
        let toasts = cx.new(|_| ToastHost::new());
        let subscription = cx.subscribe_in(&setup, window, Self::on_setup_event);
        Self {
            setup,
            toasts,
            _subscription: subscription,
        }
    }

    /// The flow this window hosts.
    #[cfg(test)]
    pub(crate) fn setup(&self) -> &Entity<AccountSetupView> {
        &self.setup
    }

    fn on_setup_event(
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
                let (account_id, message) = (*account_id, message.clone());
                // After this update: the main windows are updated in turn.
                cx.defer(move |cx| {
                    for_each_workspace(cx, |view, cx| view.account_added(account_id, &message, cx));
                });
            }
            AccountSetupEvent::Finished { .. } => window.remove_window(),
            AccountSetupEvent::Error(error) => {
                self.toasts.update(cx, |toasts, cx| {
                    toasts.push(Toast::new(error.clone()).kind(ToastKind::Error), cx);
                });
            }
        }
    }
}

impl Render for AddAccountWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let root = div()
            .id("add-account-window")
            .relative()
            .size_full()
            .font_family(theme.platform.ui_font.family())
            .text_color(rgb(theme.colors.foreground))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(
                        TitleBar::new("add-account-title-bar").child(
                            div()
                                .flex_1()
                                .px(px(SPACE_2))
                                .type_style(TYPE_BODY_STRONG)
                                .child(WINDOW_TITLE),
                        ),
                    )
                    .child(div().flex_1().min_h_0().child(self.setup.clone())),
            )
            .child(self.toasts.clone());
        window_frame(root, window, cx)
    }
}
