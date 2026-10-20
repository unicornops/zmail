//! `ToastHost`: short notices at the bottom of the window, with an
//! optional Undo action, that dismiss themselves.
//!
//! The plan replaces confirmation dialogs with undo: archive, delete, move
//! and mark actions push a toast whose Undo button reverses them. Phase 0
//! (#128) added a single transient status banner (`show_status` in the
//! workspace); `ToastHost` is its replacement once the per-pane views
//! adopt it, and the banner keeps working until then.
//!
//! ```text
//! let toasts = cx.new(|_| ToastHost::new());
//! toasts.update(cx, |toasts, cx| {
//!     toasts.push(Toast::new("Archived 3 conversations").on_undo(move |window, cx| ...), cx)
//! });
//! // In render, as the last child of a `.relative()` root:
//! .child(self.toasts.clone())
//! ```
//!
//! Toasts are announced politely to screen readers (`Role::Status`).

use std::{rc::Rc, time::Duration};

use gpui::{
    App, Context, EventEmitter, IntoElement, Render, Role, SharedString, Task, Window, div,
    prelude::*, px, rgb,
};

use crate::a11y::AccessibleExt;
use crate::styles::{ICON_SIZE_SM, RADIUS_LG, SPACE_2, SPACE_3, SPACE_4, TYPE_BODY, TypeStyleExt};
use crate::widgets::icon;

use super::button::{Button, ButtonSize, IconButton};
use super::theme::ComponentTheme;

/// How long a toast stays without an action.
pub(crate) const TOAST_DURATION: Duration = Duration::from_secs(4);
/// How long a toast with Undo stays, long enough to read and react.
pub(crate) const UNDO_TOAST_DURATION: Duration = Duration::from_secs(8);
/// The most toasts shown at once; older ones are dropped.
pub(crate) const MAX_TOASTS: usize = 3;
/// Widest a toast gets.
const TOAST_MAX_WIDTH: f32 = 480.0;

/// Identifies a toast pushed onto a [`ToastHost`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ToastId(u64);

/// The tone of a toast.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastKind {
    /// Neutral information.
    #[default]
    Info,
    /// Something finished.
    Success,
    /// Something failed.
    Error,
}

/// What a toast's Undo button runs.
pub(crate) type UndoHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// One notice.
#[derive(Clone)]
pub(crate) struct Toast {
    message: SharedString,
    kind: ToastKind,
    undo: Option<(SharedString, UndoHandler)>,
    duration: Option<Duration>,
}

impl Toast {
    /// An info toast showing `message`.
    pub(crate) fn new(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            kind: ToastKind::Info,
            undo: None,
            duration: None,
        }
    }

    /// Set the tone.
    pub(crate) fn kind(mut self, kind: ToastKind) -> Self {
        self.kind = kind;
        self
    }

    /// Add an Undo button that runs `handler` and closes the toast.
    pub(crate) fn on_undo(self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.action("Undo", handler)
    }

    /// Add an action button labelled `label` (for example "Retry").
    pub(crate) fn action(
        mut self,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.undo = Some((label.into(), Rc::new(handler)));
        self
    }

    /// Stay for `duration` instead of the default.
    pub(crate) fn duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// The time before the toast dismisses itself.
    pub(crate) fn effective_duration(&self) -> Duration {
        self.duration.unwrap_or(if self.undo.is_some() {
            UNDO_TOAST_DURATION
        } else {
            TOAST_DURATION
        })
    }
}

/// Emitted by a [`ToastHost`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastEvent {
    /// The toast timed out or was closed without acting.
    Dismissed(ToastId),
    /// The toast's action (Undo) ran.
    Undone(ToastId),
}

struct ActiveToast {
    id: ToastId,
    toast: Toast,
    _timer: Task<()>,
}

/// Shows a stack of toasts at the bottom centre of its `.relative()`
/// parent.
pub(crate) struct ToastHost {
    toasts: Vec<ActiveToast>,
    next_id: u64,
}

impl EventEmitter<ToastEvent> for ToastHost {}

impl Default for ToastHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ToastHost {
    /// An empty host.
    pub(crate) fn new() -> Self {
        Self {
            toasts: Vec::new(),
            next_id: 0,
        }
    }

    /// Show `toast`. It closes itself after its duration.
    pub(crate) fn push(&mut self, toast: Toast, cx: &mut Context<Self>) -> ToastId {
        let id = ToastId(self.next_id);
        self.next_id += 1;
        let duration = toast.effective_duration();
        let timer = cx.spawn(async move |host, cx| {
            cx.background_executor().timer(duration).await;
            host.update(cx, |host, cx| host.dismiss(id, cx)).ok();
        });
        self.toasts.push(ActiveToast {
            id,
            toast,
            _timer: timer,
        });
        while self.toasts.len() > MAX_TOASTS {
            let oldest = self.toasts.remove(0);
            cx.emit(ToastEvent::Dismissed(oldest.id));
        }
        cx.notify();
        id
    }

    /// Close toast `id` without acting. Does nothing if it is gone.
    pub(crate) fn dismiss(&mut self, id: ToastId, cx: &mut Context<Self>) {
        if self.remove(id).is_some() {
            cx.emit(ToastEvent::Dismissed(id));
            cx.notify();
        }
    }

    /// Run toast `id`'s action and close it. Does nothing if the toast is
    /// gone (it timed out) or has no action.
    pub(crate) fn undo(&mut self, id: ToastId, window: &mut Window, cx: &mut Context<Self>) {
        let has_action = self
            .toasts
            .iter()
            .any(|active| active.id == id && active.toast.undo.is_some());
        if !has_action {
            return;
        }
        if let Some(active) = self.remove(id)
            && let Some((_, handler)) = active.toast.undo
        {
            handler(window, cx);
            cx.emit(ToastEvent::Undone(id));
            cx.notify();
        }
    }

    /// Run the action of the newest toast that has one, as Secondary+Z
    /// will. Returns whether anything was undone.
    pub(crate) fn undo_latest(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let latest = self
            .toasts
            .iter()
            .rev()
            .find(|active| active.toast.undo.is_some())
            .map(|active| active.id);
        match latest {
            Some(id) => {
                self.undo(id, window, cx);
                true
            }
            None => false,
        }
    }

    /// The ids and messages of the visible toasts, oldest first.
    #[cfg(test)]
    pub(crate) fn visible(&self) -> Vec<(ToastId, SharedString)> {
        self.toasts
            .iter()
            .map(|active| (active.id, active.toast.message.clone()))
            .collect()
    }

    fn remove(&mut self, id: ToastId) -> Option<ActiveToast> {
        let index = self.toasts.iter().position(|active| active.id == id)?;
        Some(self.toasts.remove(index))
    }
}

impl Render for ToastHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let toasts: Vec<_> = self
            .toasts
            .iter()
            .map(|active| {
                let id = active.id;
                let (icon_name, accent) = match active.toast.kind {
                    ToastKind::Info => ("info", colors.info),
                    ToastKind::Success => ("check-circle", colors.success),
                    ToastKind::Error => ("alert-circle", colors.danger),
                };
                div()
                    .id(("toast", id.0))
                    .accessible(Role::Status, active.toast.message.clone())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_3))
                    .max_w(px(TOAST_MAX_WIDTH))
                    .pl(px(SPACE_4))
                    .pr(px(SPACE_2))
                    .py(px(SPACE_2))
                    .rounded(px(RADIUS_LG))
                    .bg(rgb(colors.surface))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .shadow_lg()
                    .occlude()
                    .type_style(TYPE_BODY)
                    .text_color(rgb(colors.foreground))
                    .child(icon(icon_name, ICON_SIZE_SM, accent))
                    .child(div().flex_1().min_w_0().child(active.toast.message.clone()))
                    .when_some(active.toast.undo.as_ref(), |this, (label, _)| {
                        this.child(
                            Button::new(("toast-action", id.0), label.clone())
                                .ghost()
                                .size(ButtonSize::Small)
                                .on_click(cx.listener(move |host, _, window, cx| {
                                    host.undo(id, window, cx);
                                })),
                        )
                    })
                    .child(
                        IconButton::new(("toast-close", id.0), "x", "Dismiss")
                            .size(ButtonSize::Small)
                            .on_click(cx.listener(move |host, _, _, cx| host.dismiss(id, cx))),
                    )
            })
            .collect();

        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(SPACE_4))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_2))
            .px(px(SPACE_4))
            .children(toasts)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{Entity, Subscription, TestAppContext, VisualTestContext};

    use super::*;

    struct Root {
        host: Entity<ToastHost>,
        events: Rc<RefCell<Vec<ToastEvent>>>,
        _subscription: Subscription,
    }

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().child(self.host.clone())
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<Root>, &mut VisualTestContext) {
        cx.add_window_view(|_, cx| {
            let host = cx.new(|_| ToastHost::new());
            let events = Rc::new(RefCell::new(Vec::new()));
            let log = events.clone();
            let subscription = cx.subscribe(&host, move |_, _, event: &ToastEvent, _| {
                log.borrow_mut().push(*event);
            });
            Root {
                host,
                events,
                _subscription: subscription,
            }
        })
    }

    #[gpui::test]
    fn toasts_dismiss_themselves(cx: &mut TestAppContext) {
        let (root, cx) = setup(cx);
        let host = root.read_with(cx, |root, _| root.host.clone());
        let id = host.update(cx, |host, cx| host.push(Toast::new("Saved draft"), cx));
        cx.run_until_parked();
        assert_eq!(host.read_with(cx, |host, _| host.visible().len()), 1);

        cx.executor()
            .advance_clock(TOAST_DURATION - Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(
            host.read_with(cx, |host, _| host.visible().len()),
            1,
            "not yet"
        );

        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(host.read_with(cx, |host, _| host.visible().is_empty()));
        let events = root.read_with(cx, |root, _| root.events.borrow().clone());
        assert_eq!(events, [ToastEvent::Dismissed(id)]);
    }

    #[gpui::test]
    fn undo_runs_the_action_once_and_closes(cx: &mut TestAppContext) {
        let (root, cx) = setup(cx);
        let host = root.read_with(cx, |root, _| root.host.clone());
        let undone = Rc::new(RefCell::new(0));
        let counter = undone.clone();
        let id = host.update(cx, |host, cx| {
            host.push(
                Toast::new("Archived 3 conversations")
                    .kind(ToastKind::Success)
                    .on_undo(move |_, _| *counter.borrow_mut() += 1),
                cx,
            )
        });
        cx.run_until_parked();

        // Undo toasts outlive plain ones.
        cx.executor().advance_clock(TOAST_DURATION);
        cx.run_until_parked();
        assert_eq!(host.read_with(cx, |host, _| host.visible().len()), 1);

        host.update_in(cx, |host, window, cx| {
            assert!(host.undo_latest(window, cx));
            // A second undo finds nothing.
            host.undo(id, window, cx);
            assert!(!host.undo_latest(window, cx));
        });
        cx.run_until_parked();
        assert_eq!(*undone.borrow(), 1);
        assert!(host.read_with(cx, |host, _| host.visible().is_empty()));

        // The timer firing later does not report a second dismissal.
        cx.executor().advance_clock(UNDO_TOAST_DURATION);
        cx.run_until_parked();
        let events = root.read_with(cx, |root, _| root.events.borrow().clone());
        assert_eq!(events, [ToastEvent::Undone(id)]);
    }

    #[gpui::test]
    fn undo_after_timeout_does_nothing(cx: &mut TestAppContext) {
        let (root, cx) = setup(cx);
        let host = root.read_with(cx, |root, _| root.host.clone());
        let undone = Rc::new(RefCell::new(false));
        let flag = undone.clone();
        let id = host.update(cx, |host, cx| {
            host.push(
                Toast::new("Deleted").on_undo(move |_, _| *flag.borrow_mut() = true),
                cx,
            )
        });
        cx.executor().advance_clock(UNDO_TOAST_DURATION);
        cx.run_until_parked();
        host.update_in(cx, |host, window, cx| host.undo(id, window, cx));
        assert!(!*undone.borrow());
    }

    #[gpui::test]
    fn only_the_newest_toasts_stay(cx: &mut TestAppContext) {
        let (root, cx) = setup(cx);
        let host = root.read_with(cx, |root, _| root.host.clone());
        host.update(cx, |host, cx| {
            for index in 0..5 {
                host.push(Toast::new(format!("Toast {index}")), cx);
            }
        });
        let visible: Vec<String> = host.read_with(cx, |host, _| {
            host.visible()
                .into_iter()
                .map(|(_, message)| message.to_string())
                .collect()
        });
        assert_eq!(visible, ["Toast 2", "Toast 3", "Toast 4"]);
    }
}
