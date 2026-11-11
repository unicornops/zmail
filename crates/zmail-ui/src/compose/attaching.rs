//! Adding attachments off the UI thread.
//!
//! Each file the user attaches (and each received attachment a forward
//! takes along) is copied into the draft, encrypted as the draft's own
//! (ADR 0005), on a background thread, so a large file never freezes the
//! window. Until it is done it shows as a card saying "Attaching…" with
//! its size, and Send waits for it. A file that cannot be attached stays
//! as a card that says which file and why, until dismissed.
//!
//! The copy goes on when the editor closes: the attachment is then in the
//! draft when it is opened again.

use std::{io, path::PathBuf};

use gpui::{Context, FontWeight, Role, SharedString, Stateful, div, prelude::*, px, rgb};
use zmail_core::ReceivedAttachment;

use super::{ATTACHMENT_CARD_WIDTH, ComposeView};
use crate::a11y::AccessibleExt;
use crate::components::HitTargetExt;
use crate::styles::{ICON_BUTTON_SIZE, ICON_SIZE_SM, UiColors, rems_from_px};
use crate::widgets::{EllipsisExt, format_size, icon};

/// Above this many bytes of attachments, compose warns that the message
/// may be too large to send. Many providers stop at about 25 MB; others
/// differ, so it is a warning, not a limit.
pub(crate) const LARGE_ATTACHMENTS_BYTES: u64 = 25 * 1024 * 1024;

/// Where an attachment being added comes from.
enum Source {
    /// A file on disk.
    File(PathBuf),
    /// A received attachment in the store (a forward's).
    Received(i64),
}

/// An attachment being copied into the draft, or one that could not be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingAttachment {
    id: usize,
    pub(crate) filename: String,
    /// The size, once known.
    pub(crate) size_bytes: Option<u64>,
    /// Why it could not be attached; `None` while it is being copied.
    pub(crate) failure: Option<String>,
}

impl ComposeView {
    /// Whether attachments are still being copied in: Send waits for
    /// them.
    pub(crate) fn attaching(&self) -> bool {
        self.pending_attachments
            .iter()
            .any(|pending| pending.failure.is_none())
    }

    /// The attachments being added and those that could not be.
    #[cfg(test)]
    pub(crate) fn pending_attachments(&self) -> &[PendingAttachment] {
        &self.pending_attachments
    }

    /// The size of the draft's attachments, those still being copied
    /// included.
    fn attachments_size(&self) -> u64 {
        let attached = self.compose.as_ref().map_or(0, |compose| {
            compose
                .attachments
                .iter()
                .map(|attachment| attachment.size_bytes)
                .sum()
        });
        let copying: u64 = self
            .pending_attachments
            .iter()
            .filter(|pending| pending.failure.is_none())
            .filter_map(|pending| pending.size_bytes)
            .sum();
        attached + copying
    }

    /// The warning that the attachments may be too large to send, past
    /// [`LARGE_ATTACHMENTS_BYTES`] in all.
    pub(crate) fn large_attachments_notice(&self) -> Option<String> {
        let size = self.attachments_size();
        (size > LARGE_ATTACHMENTS_BYTES).then(|| {
            format!(
                "The attachments come to {}. Many providers refuse messages over 25 MB, so \
                 this one may not send.",
                format_size(size)
            )
        })
    }

    /// Copies each file into the draft in the background.
    pub(crate) fn attach_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let filename = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            self.start_attachment(filename, None, Source::File(path), cx);
        }
    }

    /// Copies a forward's attachments into the draft in the background.
    pub(crate) fn attach_received(
        &mut self,
        attachments: Vec<ReceivedAttachment>,
        cx: &mut Context<Self>,
    ) {
        for attachment in attachments {
            self.start_attachment(
                attachment.filename,
                Some(attachment.size_bytes),
                Source::Received(attachment.id),
                cx,
            );
        }
    }

    fn start_attachment(
        &mut self,
        filename: String,
        size_bytes: Option<u64>,
        source: Source,
        cx: &mut Context<Self>,
    ) {
        let Some(draft_id) = self.draft_id() else {
            return;
        };
        let id = self.next_pending_attachment;
        self.next_pending_attachment += 1;
        self.pending_attachments.push(PendingAttachment {
            id,
            filename,
            size_bytes,
            failure: None,
        });
        cx.notify();

        let store = self.store.clone();
        let mail_actions = self.mail_actions.clone();
        cx.spawn(async move |this, cx| {
            let added = match source {
                Source::File(path) => {
                    // The size first, for the card while the copy runs,
                    // and a quick answer for a folder or a missing file.
                    let inspect = path.clone();
                    let size = cx
                        .background_spawn(async move { file_size(&inspect) })
                        .await;
                    match size {
                        Ok(size) => {
                            let _ = this.update(cx, |this, cx| this.set_size(id, size, cx));
                            cx.background_spawn(async move {
                                store
                                    .add_draft_attachment(draft_id, &path)
                                    .map(drop)
                                    .map_err(|error| failure_reason(&error))
                            })
                            .await
                        }
                        Err(reason) => Err(reason),
                    }
                }
                Source::Received(attachment_id) => {
                    cx.background_spawn(async move {
                        store
                            .add_forwarded_attachment(draft_id, attachment_id)
                            .map(drop)
                            .map_err(|_| "it isn't stored any more".to_owned())
                    })
                    .await
                }
            };
            if added.is_ok() {
                // The shell lists the draft's attachments again.
                mail_actions.update(cx, |actions, cx| {
                    actions.draft_attachments_changed(draft_id, cx)
                });
            }
            let _ = this.update(cx, |this, cx| this.finish_attachment(id, added, cx));
        })
        .detach();
    }

    fn set_size(&mut self, id: usize, size_bytes: u64, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_attachments.iter_mut().find(|p| p.id == id) {
            pending.size_bytes = Some(size_bytes);
            cx.notify();
        }
    }

    /// An attachment finished copying: its card becomes the draft's, or
    /// says why it could not be attached.
    fn finish_attachment(&mut self, id: usize, added: Result<(), String>, cx: &mut Context<Self>) {
        let Some(index) = self.pending_attachments.iter().position(|p| p.id == id) else {
            // It belonged to a draft that is no longer open here.
            return;
        };
        match added {
            Ok(()) => {
                self.pending_attachments.remove(index);
                self.refresh_from_store(cx);
            }
            Err(reason) => self.pending_attachments[index].failure = Some(reason),
        }
        // A banner asking to wait for the attachments goes once they are
        // done.
        if !self.attaching()
            && self.send_problem.as_deref() == Some(super::send_checks::WAIT_FOR_ATTACHMENTS)
        {
            self.send_problem = None;
        }
        cx.notify();
    }

    /// Dismisses the card of an attachment that could not be attached.
    fn dismiss_failed(&mut self, id: usize, cx: &mut Context<Self>) {
        self.pending_attachments
            .retain(|pending| pending.id != id || pending.failure.is_none());
        cx.notify();
    }

    /// A card for an attachment being copied in, or one that could not
    /// be.
    pub(crate) fn render_pending_card(
        &self,
        pending: &PendingAttachment,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let id = pending.id;
        let size = pending.size_bytes.map(format_size);
        let (icon_name, icon_color, detail, detail_color) = match &pending.failure {
            None => (
                "loader",
                colors.muted,
                match &size {
                    Some(size) => format!("Attaching… • {size}"),
                    None => "Attaching…".to_owned(),
                },
                colors.muted,
            ),
            Some(reason) => (
                "alert-circle",
                colors.danger,
                format!("Couldn't attach: {reason}"),
                colors.danger,
            ),
        };
        let failed = pending.failure.is_some();
        div()
            .id(("compose-attachment-pending", id))
            .debug_selector(move || format!("compose-attachment-pending-{id}"))
            .accessible(
                if failed { Role::Alert } else { Role::Status },
                SharedString::from(format!("{}: {detail}", pending.filename)),
            )
            .w(rems_from_px(ATTACHMENT_CARD_WIDTH))
            .max_w_full()
            .rounded_md()
            .bg(rgb(if failed {
                colors.danger_background
            } else {
                colors.background
            }))
            .border_1()
            .border_color(rgb(if failed { colors.danger } else { colors.border }))
            .px_3()
            .py_3()
            .flex()
            .items_start()
            .gap_3()
            .child(icon(icon_name, ICON_SIZE_SM, icon_color))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .ellipsis()
                            .child(pending.filename.clone()),
                    )
                    .child(div().text_sm().text_color(rgb(detail_color)).child(detail)),
            )
            .when(failed, |this| {
                this.child(
                    div()
                        .id(("compose-attachment-dismiss", id))
                        .hit_target(move || format!("compose-attachment-dismiss-{id}"))
                        .accessible(
                            Role::Button,
                            SharedString::from(format!("Dismiss {}", pending.filename)),
                        )
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .w(px(ICON_BUTTON_SIZE))
                        .h(px(ICON_BUTTON_SIZE))
                        .rounded_full()
                        .bg(rgb(colors.button_background))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.dismiss_failed(id, cx)))
                        .child(icon("x", ICON_SIZE_SM, colors.muted)),
                )
            })
    }
}

/// The size of the file at `path`, or why it cannot be attached.
fn file_size(path: &std::path::Path) -> Result<u64, String> {
    let metadata = std::fs::metadata(path).map_err(|error| io_reason(&error))?;
    if metadata.is_dir() {
        return Err("it's a folder; attach the files in it instead".to_owned());
    }
    Ok(metadata.len())
}

/// Why a file could not be attached, in a few plain words: from the
/// underlying I/O error when there is one.
fn failure_reason(error: &anyhow::Error) -> String {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<io::Error>())
        .map_or_else(|| format!("{error:#}"), io_reason)
}

fn io_reason(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "the file isn't there any more".to_owned(),
        io::ErrorKind::PermissionDenied => "Zmail isn't allowed to read it".to_owned(),
        io::ErrorKind::IsADirectory => "it's a folder; attach the files in it instead".to_owned(),
        io::ErrorKind::StorageFull => "there isn't enough disk space to copy it".to_owned(),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_say_why_in_plain_words() {
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(io_reason(&missing), "the file isn't there any more");
        let wrapped = anyhow::Error::new(io::Error::from(io::ErrorKind::PermissionDenied))
            .context("failed to open /tmp/secret.pdf");
        assert_eq!(failure_reason(&wrapped), "Zmail isn't allowed to read it");
        let dir = std::env::temp_dir();
        assert_eq!(
            file_size(&dir),
            Err("it's a folder; attach the files in it instead".to_owned())
        );
    }
}
