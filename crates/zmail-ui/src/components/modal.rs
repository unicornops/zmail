//! `Modal` (a centred dialog), `Sheet` (attached to the top of the
//! window, macOS style) and `Palette` (a bare card near the top of the
//! window, for the command palette), with a backdrop and a focus trap.
//!
//! - Focus moves into the modal when it opens ([`ModalState::open`]) and
//!   back to where it was when it closes ([`ModalState::close`]).
//! - Tab and Shift+Tab cycle through the modal's controls only; they never
//!   reach the window behind it.
//! - Escape closes it; so does a click on the backdrop unless that is
//!   turned off.
//! - The backdrop blocks the pointer from the rest of the window.
//!
//! A view keeps a [`ModalState`] and renders the modal while it is open:
//!
//! ```text
//! .when(self.confirm.is_open(), |this| this.child(
//!     Modal::new("confirm-delete", self.confirm.focus_handle())
//!         .title("Delete 3 messages?")
//!         .child("They move to Trash.")
//!         .footer(Button::new("cancel", "Cancel").on_click(...))
//!         .footer(Button::new("delete", "Delete").danger().on_click(...))
//!         .on_dismiss(cx.listener(|this, _, window, cx| this.confirm.close(window, cx))),
//! ))
//! ```

use std::rc::Rc;

use gpui::{
    AnyElement, App, DismissEvent, ElementId, FocusHandle, IntoElement, MouseButton, ParentElement,
    Pixels, RenderOnce, Role, SharedString, Window, anchored, deferred, div, hsla, point,
    prelude::*, px, rgb,
};

use crate::a11y::{A11yFlags, AccessibleExt};
use crate::styles::{
    RADIUS_XL, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TYPE_BODY, TYPE_TITLE, TypeStyleExt,
    rems_from_px,
};

use super::actions::{Cancel, FocusNext, FocusPrevious, MODAL_CONTEXT};
use super::theme::ComponentTheme;

/// Upper bound on the Tab presses the trap simulates while looking for
/// the next control inside the modal.
const MAX_TAB_STOPS: usize = 512;

/// Called when a modal asks to close.
pub(crate) type DismissHandler = Rc<dyn Fn(&DismissEvent, &mut Window, &mut App)>;

/// Width of a default dialog.
pub(crate) const MODAL_WIDTH: f32 = 440.0;

/// How the modal sits in the window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModalKind {
    /// Centred in the window.
    #[default]
    Dialog,
    /// Hanging from the top edge of the window (macOS sheet).
    Sheet,
    /// A fixed distance below the top of the window, so it stays put as
    /// its content changes height, with no header or padding: the content
    /// is the whole card. The title is still its accessible name.
    Palette,
}

/// Space between the top of the window and a [`ModalKind::Palette`] card,
/// as a fraction of the window height.
const PALETTE_TOP: f32 = 0.12;

/// Open/closed state and focus bookkeeping for one modal.
pub(crate) struct ModalState {
    focus_handle: FocusHandle,
    restore_focus: Option<FocusHandle>,
    open: bool,
}

impl ModalState {
    /// A closed modal.
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            restore_focus: None,
            open: false,
        }
    }

    /// Whether the modal is showing.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The handle the [`Modal`] tracks.
    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Show the modal, remember the current focus, and move focus to the
    /// modal's first control once it has been drawn.
    pub(crate) fn open(&mut self, window: &mut Window, cx: &mut App) {
        if !self.show(window, cx) {
            return;
        }
        window.focus(&self.focus_handle, cx);
        let container = self.focus_handle.clone();
        // Frame callbacks run before that frame draws, so wait for the
        // frame after the one that first draws the modal.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                if container.is_focused(window) {
                    cycle_focus_within(&container, true, window, cx);
                }
            });
        });
    }

    /// Show the modal with focus on `control` (for example the safe
    /// choice in a destructive confirmation).
    pub(crate) fn open_focusing(
        &mut self,
        control: &FocusHandle,
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.show(window, cx) {
            window.focus(control, cx);
        }
    }

    fn show(&mut self, window: &mut Window, cx: &mut App) -> bool {
        if self.open {
            return false;
        }
        self.open = true;
        self.restore_focus = window
            .focused(cx)
            .filter(|focused| *focused != self.focus_handle);
        window.refresh();
        true
    }

    /// Hide the modal and give focus back to where it was.
    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut App) {
        if !self.open {
            return;
        }
        self.open = false;
        if let Some(previous) = self.restore_focus.take() {
            window.focus(&previous, cx);
        } else if self.focus_handle.contains_focused(window, cx) {
            window.blur(cx);
        }
        window.refresh();
    }
}

/// Move focus to the next (or previous) tab stop inside `container`,
/// wrapping around within it. With nothing focusable inside, focus stays
/// on the container itself.
pub(crate) fn cycle_focus_within(
    container: &FocusHandle,
    forward: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let start = window.focused(cx);
    for _ in 0..MAX_TAB_STOPS {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        let Some(now) = window.focused(cx) else {
            break;
        };
        if Some(&now) == start.as_ref() {
            // Went all the way round (or nothing can take focus).
            break;
        }
        if now != *container && container.contains_focused(window, cx) {
            return;
        }
    }
    window.focus(container, cx);
}

/// A dialog or sheet drawn over the window. See the module documentation.
#[derive(IntoElement)]
pub(crate) struct Modal {
    id: ElementId,
    focus_handle: FocusHandle,
    kind: ModalKind,
    title: SharedString,
    description: Option<SharedString>,
    width: Pixels,
    children: Vec<AnyElement>,
    footer: Vec<AnyElement>,
    dismiss_on_backdrop: bool,
    on_dismiss: Option<DismissHandler>,
}

impl Modal {
    /// A dialog tracking `focus_handle` (from [`ModalState::focus_handle`]).
    pub(crate) fn new(id: impl Into<ElementId>, focus_handle: &FocusHandle) -> Self {
        Self {
            id: id.into(),
            focus_handle: focus_handle.clone(),
            kind: ModalKind::Dialog,
            title: SharedString::default(),
            description: None,
            width: px(MODAL_WIDTH),
            children: Vec::new(),
            footer: Vec::new(),
            dismiss_on_backdrop: true,
            on_dismiss: None,
        }
    }

    /// Hang the modal from the top of the window instead of centring it.
    pub(crate) fn sheet(mut self) -> Self {
        self.kind = ModalKind::Sheet;
        self
    }

    /// A bare card near the top of the window (see [`ModalKind::Palette`]).
    pub(crate) fn palette(mut self) -> Self {
        self.kind = ModalKind::Palette;
        self
    }

    /// The heading, also the dialog's accessible name.
    pub(crate) fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// A line of supporting text under the title.
    pub(crate) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The card width at the default text size (default
    /// [`MODAL_WIDTH`]); it grows with the text size, up to the window's.
    pub(crate) fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    /// Add an element to the footer row (buttons, trailing-aligned).
    pub(crate) fn footer(mut self, element: impl IntoElement) -> Self {
        self.footer.push(element.into_any_element());
        self
    }

    /// Keep the modal open when the backdrop is clicked.
    pub(crate) fn persistent(mut self) -> Self {
        self.dismiss_on_backdrop = false;
        self
    }

    /// Called on Escape and backdrop clicks. The owner closes the modal.
    /// Takes an event argument so `cx.listener` fits.
    pub(crate) fn on_dismiss(
        mut self,
        handler: impl Fn(&DismissEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl ParentElement for Modal {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Modal {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let viewport = window.viewport_size();
        let backdrop = hsla(0.0, 0.0, 0.0, if theme.is_light { 0.3 } else { 0.5 });
        let sheet = self.kind == ModalKind::Sheet;
        let palette = self.kind == ModalKind::Palette;

        let dismiss = self.on_dismiss.clone();
        let backdrop_dismiss = self
            .dismiss_on_backdrop
            .then(|| self.on_dismiss.clone())
            .flatten();
        let container = self.focus_handle.clone();
        let container_back = self.focus_handle.clone();

        let card = div()
            .id(self.id)
            .key_context(MODAL_CONTEXT)
            // The card holds focus while nothing inside it does, but Tab
            // skips it and goes to its first control. (A tracked handle
            // carries its own tab settings; the element's are ignored.)
            .track_focus(&self.focus_handle.clone().tab_index(0).tab_stop(false))
            .accessible(Role::Dialog, self.title.clone())
            .when_some(self.description.clone(), |this, description| {
                this.aria_description(description)
            })
            .a11y_flags(A11yFlags {
                modal: true,
                ..A11yFlags::default()
            })
            .on_action(move |_: &Cancel, window, cx| {
                if let Some(dismiss) = &dismiss {
                    dismiss(&DismissEvent, window, cx);
                }
            })
            .on_action(move |_: &FocusNext, window, cx| {
                cycle_focus_within(&container, true, window, cx);
            })
            .on_action(move |_: &FocusPrevious, window, cx| {
                cycle_focus_within(&container_back, false, window, cx);
            })
            // Presses inside the card must not count as backdrop clicks.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex()
            .flex_col()
            .w(rems_from_px(f32::from(self.width)))
            .max_w(viewport.width - px(SPACE_6 * 2.0))
            .map(|this| {
                // The palette hangs lower, so it has less room.
                let top = if palette {
                    viewport.height * PALETTE_TOP
                } else {
                    px(SPACE_6)
                };
                this.max_h(viewport.height - top - px(SPACE_6))
            })
            .bg(rgb(colors.background))
            .border_1()
            .border_color(rgb(colors.border))
            .shadow_xl()
            .map(|this| {
                if sheet {
                    this.rounded_b(px(RADIUS_XL))
                } else {
                    this.rounded(px(RADIUS_XL))
                }
            })
            .text_color(rgb(colors.foreground))
            .when(palette, |this| {
                this.overflow_hidden()
                    .type_style(TYPE_BODY)
                    .children(self.children.drain(..))
            })
            .when(!palette, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(SPACE_2))
                        .px(px(SPACE_6))
                        .pt(px(SPACE_6))
                        .pb(px(SPACE_4))
                        .child(div().type_style(TYPE_TITLE).child(self.title))
                        .when_some(self.description, |this, description| {
                            this.child(
                                div()
                                    .type_style(TYPE_BODY)
                                    .text_color(rgb(colors.muted))
                                    .child(description),
                            )
                        }),
                )
            })
            .when(!self.children.is_empty(), |this| {
                this.child(
                    div()
                        .id("modal-body")
                        .flex()
                        .flex_col()
                        .gap(px(SPACE_3))
                        .px(px(SPACE_6))
                        .pb(px(SPACE_4))
                        .overflow_y_scroll()
                        .type_style(TYPE_BODY)
                        .children(self.children),
                )
            })
            .when(!self.footer.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(SPACE_2))
                        .px(px(SPACE_6))
                        .py(px(SPACE_4))
                        .border_t_1()
                        .border_color(rgb(colors.border))
                        .children(self.footer),
                )
            });

        deferred(
            anchored().position(point(px(0.0), px(0.0))).child(
                div()
                    .id("modal-backdrop")
                    .w(viewport.width)
                    .h(viewport.height)
                    .flex()
                    .justify_center()
                    .map(|this| {
                        if sheet {
                            this.items_start()
                        } else if palette {
                            this.items_start().pt(viewport.height * PALETTE_TOP)
                        } else {
                            this.items_center()
                        }
                    })
                    .bg(backdrop)
                    // Nothing behind the modal reacts to the pointer.
                    .occlude()
                    .when_some(backdrop_dismiss, |this, dismiss| {
                        this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            dismiss(&DismissEvent, window, cx);
                        })
                    })
                    .child(card),
            ),
        )
        .with_priority(2)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Entity, Render, TestAppContext, VisualTestContext};

    use super::*;
    use crate::components::button::Button;
    use crate::components::test_support::init;

    struct Host {
        modal: ModalState,
        behind: FocusHandle,
        first: FocusHandle,
        second: FocusHandle,
        dismissed: usize,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(Button::new("behind", "Behind").track_focus(&self.behind))
                .when(self.modal.is_open(), |this| {
                    this.child(
                        Modal::new("test-modal", self.modal.focus_handle())
                            .title("Delete 3 messages?")
                            .child("They move to Trash.")
                            .footer(Button::new("cancel", "Cancel").track_focus(&self.first))
                            .footer(
                                Button::new("delete", "Delete")
                                    .danger()
                                    .track_focus(&self.second),
                            )
                            .on_dismiss(cx.listener(|host, _, window, cx| {
                                host.dismissed += 1;
                                host.modal.close(window, cx);
                                cx.notify();
                            })),
                    )
                })
        }
    }

    fn open(cx: &mut TestAppContext) -> (Entity<Host>, &mut VisualTestContext) {
        init(cx);
        let (host, cx) = cx.add_window_view(|_, cx| Host {
            modal: ModalState::new(cx),
            behind: cx.focus_handle(),
            first: cx.focus_handle(),
            second: cx.focus_handle(),
            dismissed: 0,
        });
        host.update_in(cx, |host, window, cx| {
            window.focus(&host.behind, cx);
            let first = host.first.clone();
            host.modal.open_focusing(&first, window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        (host, cx)
    }

    fn focused(host: &Entity<Host>, cx: &mut VisualTestContext) -> &'static str {
        host.update_in(cx, |host, window, _| {
            if host.first.is_focused(window) {
                "cancel"
            } else if host.second.is_focused(window) {
                "delete"
            } else if host.behind.is_focused(window) {
                "behind"
            } else if host.modal.focus_handle().is_focused(window) {
                "modal"
            } else {
                "other"
            }
        })
    }

    #[gpui::test]
    fn tab_cycles_inside_the_modal_only(cx: &mut TestAppContext) {
        let (host, cx) = open(cx);
        // Opening moves focus to the first control.
        assert_eq!(focused(&host, cx), "cancel");
        let mut seen = Vec::new();
        for _ in 0..5 {
            cx.simulate_keystrokes("tab");
            seen.push(focused(&host, cx));
        }
        assert_eq!(seen, ["delete", "cancel", "delete", "cancel", "delete"]);
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(focused(&host, cx), "cancel");
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(focused(&host, cx), "delete", "wraps backwards too");
    }

    /// Plain `open` focuses the dialog itself until its first frame is
    /// drawn; Tab from there enters the dialog, never the window behind.
    #[gpui::test]
    fn tab_from_the_dialog_itself_enters_it(cx: &mut TestAppContext) {
        let (host, cx) = open(cx);
        host.update_in(cx, |host, window, cx| {
            host.modal.close(window, cx);
            host.modal.open(window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(focused(&host, cx), "modal");
        cx.simulate_keystrokes("tab");
        assert_eq!(focused(&host, cx), "cancel");
        cx.simulate_keystrokes("escape");
        assert_eq!(focused(&host, cx), "behind");
    }

    #[gpui::test]
    fn escape_closes_and_restores_focus(cx: &mut TestAppContext) {
        let (host, cx) = open(cx);
        cx.simulate_keystrokes("tab escape");
        host.read_with(cx, |host, _| {
            assert!(!host.modal.is_open());
            assert_eq!(host.dismissed, 1);
        });
        assert_eq!(focused(&host, cx), "behind");
    }
}
