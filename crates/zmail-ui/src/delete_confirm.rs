//! Asking before a delete that cannot be undone.
//!
//! Delete moves conversations to their account's Trash, at once and with
//! Undo. From the Trash itself, or on an account with no Trash, it deletes
//! them for good (from the server too, once the write-back runs), so the
//! shell asks first: the dialog names how many go for good and opens with
//! Cancel focused. [`zmail_actions::MailActions::permanent_deletes`]
//! decides, before anything changes.

use gpui::{AnyElement, App, Context, FocusHandle, IntoElement, Window, div, prelude::*};
use zmail_core::text::count_noun;

use crate::WorkspaceView;
use crate::components::{Button, Modal, ModalState};

/// The dialog's state: open while a delete waits for an answer.
pub(crate) struct DeleteConfirm {
    modal: ModalState,
    /// Its Cancel button, which has focus when it opens.
    cancel: FocusHandle,
    pending: Option<PendingDelete>,
}

/// A delete waiting for the user's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingDelete {
    thread_ids: Vec<i64>,
    /// How many of them go for good; the rest move to Trash.
    permanent: usize,
}

impl DeleteConfirm {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            modal: ModalState::new(cx),
            cancel: cx.focus_handle(),
            pending: None,
        }
    }

    /// The dialog's title while it shows, and whether its Cancel has
    /// focus.
    #[cfg(test)]
    pub(crate) fn showing(&self, window: &Window) -> Option<(String, bool)> {
        let pending = self.pending.as_ref().filter(|_| self.modal.is_open())?;
        Some((title(pending.permanent), self.cancel.is_focused(window)))
    }
}

/// The dialog's title, for example "Delete 3 conversations for good?".
fn title(permanent: usize) -> String {
    if permanent == 1 {
        "Delete this conversation for good?".to_owned()
    } else {
        format!(
            "Delete {} for good?",
            count_noun(permanent, "conversation", "conversations")
        )
    }
}

/// What the dialog says under its title.
fn description(permanent: usize, total: usize) -> String {
    let mut text = if permanent == 1 {
        "It is deleted from the server, not moved to Trash. This can't be undone.".to_owned()
    } else {
        "They are deleted from the server, not moved to Trash. This can't be undone.".to_owned()
    };
    let to_trash = total.saturating_sub(permanent);
    if to_trash > 0 {
        text.push_str(&format!(
            " The other {} {} to Trash.",
            count_noun(to_trash, "conversation", "conversations"),
            if to_trash == 1 { "moves" } else { "move" }
        ));
    }
    text
}

impl WorkspaceView {
    /// Deletes `thread_ids`: at once when they all move to Trash, else
    /// after the dialog's yes.
    pub(crate) fn request_delete(
        &mut self,
        thread_ids: Vec<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if thread_ids.is_empty() {
            return;
        }
        let permanent = match self.mail_actions.read(cx).permanent_deletes(&thread_ids) {
            Ok(permanent) => permanent,
            Err(error) => {
                self.show_error(error, cx);
                return;
            }
        };
        if permanent == 0 {
            self.delete_now(thread_ids, cx);
            return;
        }
        self.delete_confirm.pending = Some(PendingDelete {
            thread_ids,
            permanent,
        });
        let cancel = self.delete_confirm.cancel.clone();
        self.delete_confirm.modal.open_focusing(&cancel, window, cx);
        cx.notify();
    }

    fn delete_now(&mut self, thread_ids: Vec<i64>, cx: &mut Context<Self>) {
        self.change_these_threads(thread_ids, cx, |actions, thread_ids, cx| {
            actions.delete_threads(thread_ids, cx)
        });
    }

    fn close_delete_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_confirm.pending = None;
        self.delete_confirm.modal.close(window, cx);
        cx.notify();
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.delete_confirm.pending.take();
        self.close_delete_confirm(window, cx);
        if let Some(pending) = pending {
            self.delete_now(pending.thread_ids, cx);
        }
    }

    pub(crate) fn render_delete_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = &self.delete_confirm;
        if !confirm.modal.is_open() {
            return None;
        }
        let pending = confirm.pending.as_ref()?;
        Some(
            Modal::new("delete-confirm-dialog", confirm.modal.focus_handle())
                .title(title(pending.permanent))
                .description(description(pending.permanent, pending.thread_ids.len()))
                .footer(
                    div()
                        .debug_selector(|| "delete-confirm-cancel".into())
                        .child(
                            Button::new("delete-confirm-cancel", "Cancel")
                                .track_focus(&confirm.cancel)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_delete_confirm(window, cx)
                                })),
                        ),
                )
                .footer(
                    div()
                        .debug_selector(|| "delete-confirm-delete".into())
                        .child(
                            Button::new("delete-confirm-delete", "Delete for Good")
                                .danger()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_delete(window, cx)
                                })),
                        ),
                )
                .on_dismiss(
                    cx.listener(|this, _, window, cx| this.close_delete_confirm(window, cx)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{description, title};

    #[test]
    fn the_dialog_names_how_many_go_for_good() {
        assert_eq!(title(1), "Delete this conversation for good?");
        assert_eq!(title(3), "Delete 3 conversations for good?");
        assert_eq!(
            description(3, 3),
            "They are deleted from the server, not moved to Trash. This can't be undone."
        );
        assert_eq!(
            description(1, 3),
            "It is deleted from the server, not moved to Trash. This can't be undone. \
             The other 2 conversations move to Trash."
        );
        assert!(description(2, 3).ends_with("The other 1 conversation moves to Trash."));
    }
}
