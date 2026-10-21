//! View Plain Text / View Source: a dialog with a message's body as it is
//! stored locally.
//!
//! A plain-text message shows its text. An HTML message shows its
//! sanitized HTML: scripts and styles removed, remote addresses rewritten
//! to `blocked://`, which is what the store keeps. The raw RFC 5322 source
//! is not stored, so it cannot be shown.

use gpui::{
    AnyElement, ClipboardItem, Context, IntoElement, SharedString, Window, div, prelude::*, px, rgb,
};
use zmail_core::participants::sender_identity;

use super::ReaderView;
use crate::a11y::readable;
use crate::components::{Button, Modal};
use crate::styles::UiColors;

/// Width of the source dialog: wider than a confirmation, for text.
const SOURCE_DIALOG_WIDTH: f32 = 680.;

/// Tallest the source text gets before it scrolls.
const SOURCE_MAX_HEIGHT: f32 = 420.;

/// What the source dialog shows.
pub(super) struct SourceView {
    title: SharedString,
    description: SharedString,
    text: SharedString,
}

impl ReaderView {
    /// Opens the source dialog on the message at `index`.
    pub(super) fn open_source(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self
            .state
            .thread
            .as_ref()
            .and_then(|thread| thread.messages.get(index))
        else {
            return;
        };
        let (name, _) = sender_identity(&message.sender);
        let source = match (&message.body_text, &message.sanitized_html) {
            (Some(text), _) => SourceView {
                title: "Plain Text".into(),
                description: format!("The message from {name}, as plain text.").into(),
                text: text.clone().into(),
            },
            (None, Some(html)) => SourceView {
                title: "Source".into(),
                description: format!(
                    "The message from {name} as sanitized HTML: scripts removed and remote \
                     addresses blocked."
                )
                .into(),
                text: html.clone().into(),
            },
            (None, None) => SourceView {
                title: "Plain Text".into(),
                description: format!("Only a preview of the message from {name} is stored so far.")
                    .into(),
                text: message.rendered_body.clone().into(),
            },
        };
        self.source = Some(source);
        self.source_modal.open(window, cx);
        cx.notify();
    }

    fn close_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.source = None;
        self.source_modal.close(window, cx);
        cx.notify();
    }

    /// The source dialog, while it is open.
    pub(super) fn render_source(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let source = self
            .source
            .as_ref()
            .filter(|_| self.source_modal.is_open())?;
        let text = source.text.clone();
        Some(
            Modal::new("reader-source", self.source_modal.focus_handle())
                .title(source.title.clone())
                .description(source.description.clone())
                .width(px(SOURCE_DIALOG_WIDTH))
                .child(
                    div()
                        .id("reader-source-text")
                        .debug_selector(|| "reader-source-text".into())
                        .max_h(px(SOURCE_MAX_HEIGHT))
                        .overflow_y_scroll()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(colors.surface))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .text_sm()
                        .child(readable("reader-source-body", source.text.clone())),
                )
                .footer(
                    Button::new("reader-source-copy", "Copy")
                        .icon("copy")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                        }),
                )
                .footer(
                    Button::new("reader-source-close", "Close")
                        .primary()
                        .on_click(cx.listener(|reader, _, window, cx| {
                            reader.close_source(window, cx);
                        })),
                )
                .on_dismiss(cx.listener(|reader, _, window, cx| reader.close_source(window, cx)))
                .into_any_element(),
        )
    }
}
