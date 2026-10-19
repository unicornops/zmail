//! `ComposeWindow`: a window of its own for one draft.
//!
//! New mail opens here, one window per draft. The window's title bar shows
//! the draft's subject (or "New Message") and, on macOS and Linux, Attach
//! and Send at its trailing end; Windows puts Send at the bottom-left of
//! the compose body instead ([`PlatformStyle::send_button`]).
//!
//! Escape, the window's close button, or sending closes the window. The
//! draft is saved first, so closing never loses anything: the draft stays
//! in Drafts. Status and errors show as toasts in the window itself.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Focusable, IntoElement, Render, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div, point, prelude::*, px,
    rgb, size,
};
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use super::{ComposeEvent, ComposeView, Presentation};
use crate::actions::{AddAttachment, Cancel, Send};
use crate::components::title_bar::TRAFFIC_LIGHT_POSITION;
use crate::components::{
    Button, ComponentTheme, IconButton, TitleBar, Toast, ToastHost, ToastKind, window_frame,
};
use crate::mail_actions::MailActions;
use crate::pane::dispatch_from;
use crate::styles::{PlatformStyle, SPACE_2, SendButtonPlacement, TYPE_BODY_STRONG, TypeStyleExt};
use crate::widgets::EllipsisExt;

/// The key context the compose window's root sets.
pub(crate) const KEY_CONTEXT: &str = "ComposeWindow";
/// A new compose window's size.
const COMPOSE_WINDOW_WIDTH: f32 = 760.0;
const COMPOSE_WINDOW_HEIGHT: f32 = 680.0;
/// The smallest a compose window gets.
const COMPOSE_WINDOW_MIN_WIDTH: f32 = 480.0;
const COMPOSE_WINDOW_MIN_HEIGHT: f32 = 360.0;

/// The root view of a compose window.
pub(crate) struct ComposeWindow {
    compose: Entity<ComposeView>,
    toasts: Entity<ToastHost>,
    /// The title last given to the window system.
    window_title: String,
    _subscriptions: Vec<Subscription>,
}

impl ComposeWindow {
    /// Opens `draft_id` in a new compose window, with focus in To (or in
    /// the body, above the quoted message, when `focus_body` is set).
    pub(crate) fn open(
        draft_id: i64,
        focus_body: bool,
        store: Arc<LocalStore>,
        sync_service: SyncService,
        mail_actions: Entity<MailActions>,
        cx: &mut App,
    ) -> Result<WindowHandle<Self>> {
        // Fail here, before a window flashes up, if the draft has gone.
        store
            .draft_detail(draft_id)?
            .context("This draft no longer exists.")?;
        let bounds = Bounds::centered(
            None,
            size(px(COMPOSE_WINDOW_WIDTH), px(COMPOSE_WINDOW_HEIGHT)),
            cx,
        );
        cx.open_window(
            WindowOptions {
                // Same chrome as the main window: Zmail draws the title bar.
                titlebar: Some(TitlebarOptions {
                    title: Some("New Message".into()),
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
                    px(COMPOSE_WINDOW_MIN_WIDTH),
                    px(COMPOSE_WINDOW_MIN_HEIGHT),
                )),
                app_id: Some(crate::APP_ID.to_owned()),
                ..Default::default()
            },
            move |window, cx| {
                cx.new(|cx| {
                    Self::new(
                        draft_id,
                        focus_body,
                        store,
                        sync_service,
                        mail_actions,
                        window,
                        cx,
                    )
                })
            },
        )
    }

    fn new(
        draft_id: i64,
        focus_body: bool,
        store: Arc<LocalStore>,
        sync_service: SyncService,
        mail_actions: Entity<MailActions>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let compose = cx.new(|cx| {
            ComposeView::new(store, sync_service, mail_actions, Presentation::Window, cx)
        });
        let toasts = cx.new(|_| ToastHost::new());
        let loaded = compose.update(cx, |compose, cx| {
            let loaded = compose.load_draft(draft_id, window, cx);
            if focus_body {
                compose.focus_body_start(window, cx);
            } else {
                compose.focus_to(window, cx);
            }
            loaded
        });
        if let Err(error) = loaded {
            toasts.update(cx, |toasts, cx| {
                toasts.push(Toast::new(error.to_string()).kind(ToastKind::Error), cx)
            });
        }

        // The window's own close button saves the draft first.
        let saving = compose.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = saving.update(cx, |compose, cx| compose.flush_autosave(cx));
            true
        });

        let subscriptions = vec![
            cx.subscribe_in(&compose, window, Self::on_compose_event),
            // The title follows the subject.
            cx.observe(&compose, |_, _, cx| cx.notify()),
        ];
        Self {
            compose,
            toasts,
            window_title: String::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The draft editor.
    pub(crate) fn compose(&self) -> &Entity<ComposeView> {
        &self.compose
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

    fn on_compose_event(
        &mut self,
        _: &Entity<ComposeView>,
        event: &ComposeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposeEvent::Status(message) => self.toast(message, ToastKind::Info, cx),
            ComposeEvent::Error(error) => self.toast(error, ToastKind::Error, cx),
            // The main window confirms the send; this one's job is done.
            ComposeEvent::Queued { .. } => window.remove_window(),
            ComposeEvent::PopOut { .. } | ComposeEvent::ShowWindow { .. } => {}
        }
    }

    fn toast(&mut self, message: &str, kind: ToastKind, cx: &mut Context<Self>) {
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(Toast::new(message.to_owned()).kind(kind), cx);
        });
    }

    /// Escape: save the draft and close the window.
    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.compose
            .update(cx, |compose, cx| compose.flush_autosave(cx));
        window.remove_window();
    }

    /// The title bar's content: the title, then Attach and Send where the
    /// platform puts Send in the title bar.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let compose = self.compose.read(cx);
        let focus_handle = compose.focus_handle(cx);
        let title = compose.title(cx);
        let draft_open = compose.draft_id().is_some();
        let send_in_bar = matches!(
            PlatformStyle::current().send_button,
            SendButtonPlacement::ToolbarTrailing | SendButtonPlacement::HeaderBarTrailing
        );

        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(SPACE_2))
            .px(px(SPACE_2))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .type_style(TYPE_BODY_STRONG)
                    .text_color(rgb(theme.colors.foreground))
                    .ellipsis()
                    .child(title),
            )
            .when(draft_open && send_in_bar, |this| {
                this.child(
                    div().occlude().child(
                        IconButton::new("compose-window-attach", "paperclip", "Attach files")
                            .on_click(dispatch_from(&focus_handle, AddAttachment)),
                    ),
                )
                .child(
                    div()
                        .debug_selector(|| "compose-window-send".into())
                        .occlude()
                        .child(
                            Button::new("compose-window-send", "Send")
                                .primary()
                                .icon("send")
                                .shortcut("secondary-enter")
                                .on_click(dispatch_from(&focus_handle, Send)),
                        ),
                )
            })
    }
}

impl Render for ComposeWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let title = self.compose.read(cx).title(cx);
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }

        let root = div()
            .id("compose-window")
            .relative()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::cancel))
            .size_full()
            .font_family(theme.platform.ui_font.family())
            .text_color(rgb(theme.colors.foreground))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(TitleBar::new("compose-title-bar").child(self.render_toolbar(cx)))
                    .child(div().flex_1().min_h_0().child(self.compose.clone())),
            )
            .child(self.toasts.clone());
        window_frame(root, window, cx)
    }
}
