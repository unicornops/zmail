//! The detail pane beside the Outbox: the selected unsent message, why it
//! has not gone, and what can be done about it.
//!
//! [`OutboxDetailView`] is its own entity with its own focus handle and the
//! `OutboxDetail` key context, so the conversation commands (Archive,
//! Delete, Reply…) bound in the reader and the mail list never reach it:
//! with the Outbox showing, they have no conversation to act on. It shows
//! the message's recipients and subject, its sending status, and the
//! server's last error in full (the row only has room for a summary), with:
//!
//! - Retry Now, for a message waiting for a retry or one that gave up;
//! - Re-authenticate, when the server refused the account's sign-in;
//! - Edit, which takes a waiting message back (as Undo Send does) or opens
//!   a failed one, in a compose window;
//! - Delete, which asks first, with Cancel focused, then deletes the
//!   unsent message and its draft.
//!
//! The shell pushes the state with [`OutboxDetailView::set_state`] and acts
//! on [`OutboxDetailEvent`]s.

use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    Render, Role, Window, div, prelude::*, rgb,
};
use zmail_core::{DraftState, DraftSummary, OutboxJobSummary};

use crate::a11y::AccessibleExt;
use crate::components::{Button, Modal, ModalState};
use crate::message_list::outbox_status;
use crate::pane::pane_focus_indicator;
use crate::styles::{TYPE_BODY, TypeStyleExt, UiColors};
use crate::widgets::{empty_state, status_banner};

/// The key context the pane sets.
pub(crate) const KEY_CONTEXT: &str = "OutboxDetail";

/// What the pane shows: the selected Outbox message and its draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OutboxDetailState {
    pub(crate) job: Option<OutboxJobSummary>,
    pub(crate) draft: Option<DraftSummary>,
}

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutboxDetailEvent {
    /// Retry Now.
    Retry(i64),
    /// Re-authenticate the account.
    Reauthenticate(i64),
    /// Edit the message: take it back first when `take_back` is set.
    Edit {
        job_id: i64,
        draft_id: i64,
        take_back: bool,
    },
    /// Delete the message (confirmed): take it back first when
    /// `take_back` is set.
    Delete {
        job_id: i64,
        draft_id: i64,
        take_back: bool,
    },
}

/// Which changes a message allows in its current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Allowed {
    /// It waits for its first attempt: it can be taken back.
    take_back: bool,
    /// Edit and Delete: it gave up, or can be taken back. A message that
    /// is sending, or waits for a retry, cannot change until that settles.
    change: bool,
}

fn allowed(job: &OutboxJobSummary, draft: Option<&DraftSummary>) -> Allowed {
    use zmail_core::OutboxJobState::*;
    let take_back = job.state == Queued;
    let failed =
        job.state == Failed && draft.is_some_and(|draft| draft.state == DraftState::Failed);
    Allowed {
        take_back,
        change: take_back || failed,
    }
}

pub(crate) struct OutboxDetailView {
    focus_handle: FocusHandle,
    state: OutboxDetailState,
    /// "Delete this message?", and its Cancel button, focused when it
    /// opens.
    delete_modal: ModalState,
    delete_cancel: FocusHandle,
}

impl EventEmitter<OutboxDetailEvent> for OutboxDetailView {}

impl Focusable for OutboxDetailView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl OutboxDetailView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            state: OutboxDetailState::default(),
            delete_modal: ModalState::new(cx),
            delete_cancel: cx.focus_handle(),
        }
    }

    /// Replaces what the pane shows; re-renders only when it changed.
    pub(crate) fn set_state(&mut self, state: OutboxDetailState, cx: &mut Context<Self>) {
        if self.state != state {
            self.state = state;
            cx.notify();
        }
    }

    /// What the pane shows now.
    #[cfg(test)]
    pub(crate) fn state(&self) -> &OutboxDetailState {
        &self.state
    }

    /// Whether the delete question shows, and whether its Cancel has
    /// focus.
    #[cfg(test)]
    pub(crate) fn delete_confirmation(&self, window: &Window) -> Option<bool> {
        self.delete_modal
            .is_open()
            .then(|| self.delete_cancel.is_focused(window))
    }

    /// Delete: asks first, when the message can be deleted now.
    pub(crate) fn request_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job) = self.state.job.as_ref() else {
            return;
        };
        if !allowed(job, self.state.draft.as_ref()).change {
            return;
        }
        let cancel = self.delete_cancel.clone();
        self.delete_modal.open_focusing(&cancel, window, cx);
        cx.notify();
    }

    fn close_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_modal.close(window, cx);
        cx.notify();
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_delete(window, cx);
        let Some(job) = self.state.job.as_ref() else {
            return;
        };
        let allowed = allowed(job, self.state.draft.as_ref());
        if allowed.change {
            cx.emit(OutboxDetailEvent::Delete {
                job_id: job.id,
                draft_id: job.draft_id,
                take_back: allowed.take_back,
            });
        }
    }
}

impl Render for OutboxDetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.global::<UiColors>();
        let focused = self.focus_handle.contains_focused(window, cx);
        div()
            .id("outbox-detail")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .accessible(Role::Pane, "Outbox message")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            .child(
                pane_focus_indicator(
                    div()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(rgb(colors.border)),
                    focused,
                    &colors,
                )
                .child("Outbox"),
            )
            .child(self.render_message(&colors, cx))
            .children(self.render_delete_dialog(cx))
    }
}

impl OutboxDetailView {
    fn render_message(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let Some(job) = self.state.job.as_ref() else {
            return empty_state(
                "No message selected",
                "Pick a message in the Outbox to see why it hasn't gone yet.",
                "send",
                None,
                colors,
            );
        };
        let draft = self.state.draft.as_ref();
        let subject = draft
            .map(|draft| draft.subject.trim())
            .filter(|subject| !subject.is_empty())
            .unwrap_or("(no subject)")
            .to_owned();
        let to = draft
            .map(|draft| draft.recipient_summary.clone())
            .filter(|to| !to.is_empty())
            .unwrap_or_else(|| "No recipients".to_owned());
        let status = outbox_status(job, &chrono::Local::now());
        let allowed = allowed(job, draft);
        let (job_id, draft_id, account_id) = (job.id, job.draft_id, job.account_id);

        let mut buttons: Vec<AnyElement> = Vec::new();
        if status.needs_reauth {
            buttons.push(button(
                "outbox-detail-reauth",
                Button::new("outbox-detail-reauth", "Re-authenticate")
                    .primary()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(OutboxDetailEvent::Reauthenticate(account_id))
                    })),
            ));
        } else if status.can_retry && status.key_change.is_none() {
            buttons.push(button(
                "outbox-detail-retry",
                Button::new("outbox-detail-retry", "Retry Now")
                    .primary()
                    .icon("refresh-cw")
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(OutboxDetailEvent::Retry(job_id))),
                    ),
            ));
        }
        if allowed.change {
            let take_back = allowed.take_back;
            buttons.push(button(
                "outbox-detail-edit",
                Button::new("outbox-detail-edit", "Edit")
                    .icon("pen-square")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(OutboxDetailEvent::Edit {
                            job_id,
                            draft_id,
                            take_back,
                        })
                    })),
            ));
            buttons.push(button(
                "outbox-detail-delete",
                Button::new("outbox-detail-delete", "Delete…")
                    .ghost()
                    .icon("trash-2")
                    .on_click(cx.listener(|this, _, window, cx| this.request_delete(window, cx))),
            ));
        }

        div()
            .id("outbox-detail-scroll")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .overflow_y_scroll()
            .px_4()
            .py_4()
            .child(div().text_xl().font_weight(FontWeight::BOLD).child(subject))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .type_style(TYPE_BODY)
                    .child(format!("To: {to}"))
                    .child(
                        div()
                            .debug_selector(|| "outbox-detail-status".into())
                            .text_color(rgb(if status.failed {
                                colors.danger
                            } else {
                                colors.muted
                            }))
                            .child(status.text.clone()),
                    ),
            )
            .when_some(job.last_error_message.clone(), |this, error| {
                this.child(
                    div()
                        .debug_selector(|| "outbox-detail-error".into())
                        .w_full()
                        .min_w_0()
                        .child(status_banner(
                            "outbox-detail-error",
                            Role::Alert,
                            &format!("The server said: {error}"),
                            "alert-circle",
                            colors.danger,
                            colors.danger_background,
                            colors,
                        )),
                )
            })
            .when(!buttons.is_empty(), |this| {
                this.child(div().flex().flex_wrap().gap_2().children(buttons))
            })
            .into_any_element()
    }

    fn render_delete_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.delete_modal.is_open() {
            return None;
        }
        Some(
            Modal::new("outbox-delete-dialog", self.delete_modal.focus_handle())
                .title("Delete this message?")
                .description(
                    "It hasn't been sent. The message and its attachments are deleted from Zmail. This can't be undone.",
                )
                .footer(div().debug_selector(|| "outbox-delete-cancel".into()).child(
                    Button::new("outbox-delete-cancel", "Cancel")
                        .track_focus(&self.delete_cancel)
                        .on_click(cx.listener(|this, _, window, cx| this.close_delete(window, cx))),
                ))
                .footer(
                    div()
                        .debug_selector(|| "outbox-delete-confirm".into())
                        .child(
                            Button::new("outbox-delete-confirm", "Delete Message")
                                .danger()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_delete(window, cx)
                                })),
                        ),
                )
                .on_dismiss(cx.listener(|this, _, window, cx| this.close_delete(window, cx)))
                .into_any_element(),
        )
    }
}

/// `button` wrapped so tests can find it by `selector`.
fn button(selector: &'static str, button: Button) -> AnyElement {
    div()
        .debug_selector(move || selector.into())
        .child(button)
        .into_any_element()
}
