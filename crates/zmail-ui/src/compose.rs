//! Compose view, draft state, and compose action buttons.
//!
//! [`ComposeState`] holds the in-memory form backing a draft being edited.
//! [`render_compose_detail`] produces the scroll content for the compose pane.
//! [`compose_action_buttons`] produces the Send / Save Draft buttons shown at
//! the bottom of the detail pane when composing.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, StatefulInteractiveElement, div, prelude::*, px,
    rgb,
};
use zmail_core::{DraftAttachment, DraftDetail, DraftState, IdentitySummary};

use super::WorkspaceView;
use crate::input::{EditorField, UiBuffer};
use crate::styles::{ICON_BUTTON_SIZE, ICON_SIZE_SM, UiColors};
use crate::widgets::{
    EllipsisExt, empty_state, format_size, icon, input_card, label_chip, section_card,
    status_banner, toolbar_button,
};

// ---------------------------------------------------------------------------
// ComposeState
// ---------------------------------------------------------------------------

/// In-memory form state for a draft being actively composed.
///
/// All fields mirror the underlying [`DraftDetail`] from the store, but are
/// kept as [`UiBuffer`]s so the user's cursor position and edits-in-flight are
/// preserved across refreshes.
#[derive(Debug, Clone)]
pub(crate) struct ComposeState {
    pub(crate) draft_id: i64,
    pub(crate) account_id: i64,
    pub(crate) identity_id: i64,
    pub(crate) identity_email: String,
    pub(crate) to: UiBuffer,
    pub(crate) cc: UiBuffer,
    pub(crate) bcc: UiBuffer,
    pub(crate) subject: UiBuffer,
    pub(crate) body: UiBuffer,
    pub(crate) attachment_path: UiBuffer,
    pub(crate) attachments: Vec<DraftAttachment>,
    pub(crate) state: DraftState,
    pub(crate) last_error_message: Option<String>,
}

impl ComposeState {
    /// Construct a [`ComposeState`] from a store-fetched [`DraftDetail`].
    pub(crate) fn from_draft(draft: DraftDetail) -> Self {
        Self {
            draft_id: draft.id,
            account_id: draft.account_id,
            identity_id: draft.identity_id,
            identity_email: draft.identity_email,
            to: UiBuffer::single_line(draft.to.join(", ")),
            cc: UiBuffer::single_line(draft.cc.join(", ")),
            bcc: UiBuffer::single_line(draft.bcc.join(", ")),
            subject: UiBuffer::single_line(draft.subject),
            body: UiBuffer::multiline(draft.body),
            attachment_path: UiBuffer::single_line(String::new()),
            attachments: draft.attachments,
            state: draft.state,
            last_error_message: draft.last_error_message,
        }
    }
}

// ---------------------------------------------------------------------------
// render_compose_detail
// ---------------------------------------------------------------------------

/// Render the compose form: identity selector, address fields, subject, body,
/// and attachment path input.  Returns an [`AnyElement`] ready to be placed
/// inside a scrollable container.
pub(crate) fn render_compose_detail(
    compose: Option<&ComposeState>,
    identities: &[IdentitySummary],
    active_field: Option<EditorField>,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let Some(compose) = compose else {
        return empty_state(
            "No draft open",
            "Press Compose or select a draft to edit it here.",
            "pen-square",
            None,
            colors,
        );
    };

    let title = if compose.subject.text.trim().is_empty() {
        "New message".to_owned()
    } else {
        compose.subject.text.trim().to_owned()
    };
    // "Editing" is the normal state of an open draft; only call out the
    // states that need attention.
    let state_chip = (compose.state != DraftState::Editing).then(|| {
        label_chip(
            compose.state.label(),
            Some("file-text"),
            colors.button_background,
            colors.info,
        )
    });

    div()
        .flex()
        .flex_col()
        .gap_4()
        .w_full()
        .min_w_0()
        .child(
            div()
                .flex()
                .justify_between()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .child(title),
                )
                .children(state_chip),
        )
        .when_some(compose.last_error_message.as_ref(), |this, error| {
            this.child(status_banner(
                &format!("Last send error: {error}"),
                "alert-circle",
                colors.danger,
                colors.warning_background,
                colors,
            ))
        })
        .child(render_identity_section(compose, identities, colors, cx))
        .child(render_recipients_section(compose, active_field, colors, cx))
        .child(render_body_section(compose, active_field, colors, cx))
        .child(render_attachments_section(
            compose,
            active_field,
            colors,
            cx,
        ))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// compose_action_buttons
// ---------------------------------------------------------------------------

/// Build the action-bar buttons shown at the bottom of the detail pane while
/// composing: save and send stay visible however long the draft is.
///
/// Returns an empty [`Vec`] when no draft is open.
pub(crate) fn compose_action_buttons(
    draft_open: bool,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> Vec<AnyElement> {
    if !draft_open {
        return Vec::new();
    }
    vec![
        toolbar_button(
            "Send",
            Some("send"),
            colors.accent,
            colors.accent,
            colors.background,
            cx.listener(|this: &mut WorkspaceView, _, window, cx| {
                this.send_current_draft(window, cx);
            }),
        )
        .into_any_element(),
        toolbar_button(
            "Save Draft",
            Some("file-text"),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.save_current_draft(cx);
            }),
        )
        .into_any_element(),
    ]
}

/// The sender identity. With one identity it is a plain "From" line; with
/// several, each is a selectable row and the chosen one is marked. Either
/// way each identity appears once.
fn render_identity_section(
    compose: &ComposeState,
    identities: &[IdentitySummary],
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    if identities.len() <= 1 {
        let (name, email) = identities
            .first()
            .map(|identity| {
                (
                    identity.display_name.clone(),
                    identity.email_address.clone(),
                )
            })
            .unwrap_or_else(|| (String::new(), compose.identity_email.clone()));
        return section_card("From", colors).child(identity_row(&name, &email, false, colors));
    }

    section_card("From", colors).child(div().flex().flex_col().gap_2().children(
        identities.iter().map(|identity| {
            let identity_id = identity.id;
            let selected = identity.id == compose.identity_id;

            div()
                .id(("compose-identity", identity.id as u64))
                .cursor_pointer()
                .rounded_md()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_compose_identity(identity_id, cx);
                }))
                .child(identity_row(
                    &identity.display_name,
                    &identity.email_address,
                    selected,
                    colors,
                ))
        }),
    ))
}

/// One identity: name and address, highlighted and ticked when selected.
fn identity_row(name: &str, email: &str, selected: bool, colors: &UiColors) -> gpui::Div {
    div()
        .rounded_md()
        .bg(rgb(if selected {
            colors.selected_background
        } else {
            colors.background
        }))
        .border_1()
        .border_color(rgb(if selected {
            colors.accent
        } else {
            colors.border
        }))
        .px_3()
        .py_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(icon(
                    "user",
                    ICON_SIZE_SM,
                    if selected {
                        colors.accent
                    } else {
                        colors.muted
                    },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .when(!name.is_empty(), |this| {
                            this.child(div().ellipsis().child(name.to_owned()))
                        })
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .ellipsis()
                                .child(email.to_owned()),
                        ),
                )
                .when(selected, |this| {
                    this.child(icon("check-circle", ICON_SIZE_SM, colors.success))
                }),
        )
}

fn render_recipients_section(
    compose: &ComposeState,
    active_field: Option<EditorField>,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    section_card("Recipients", colors)
        .child(
            input_card(
                "compose-to-field",
                "To",
                "alice@example.com, bob@example.com",
                &compose.to,
                active_field == Some(EditorField::ComposeTo),
                false,
                colors,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeTo, window, cx);
            })),
        )
        .child(
            input_card(
                "compose-cc-field",
                "Cc",
                "Optional carbon copy recipients",
                &compose.cc,
                active_field == Some(EditorField::ComposeCc),
                false,
                colors,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeCc, window, cx);
            })),
        )
        .child(
            input_card(
                "compose-bcc-field",
                "Bcc",
                "Optional blind copy recipients",
                &compose.bcc,
                active_field == Some(EditorField::ComposeBcc),
                false,
                colors,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeBcc, window, cx);
            })),
        )
        .child(
            input_card(
                "compose-subject-field",
                "Subject",
                "Message subject",
                &compose.subject,
                active_field == Some(EditorField::ComposeSubject),
                false,
                colors,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeSubject, window, cx);
            })),
        )
}

fn render_body_section(
    compose: &ComposeState,
    active_field: Option<EditorField>,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    section_card("Message", colors).child(
        input_card(
            "compose-body-field",
            "Body",
            "Write your message...",
            &compose.body,
            active_field == Some(EditorField::ComposeBody),
            true,
            colors,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.set_active_field(EditorField::ComposeBody, window, cx);
        })),
    )
}

fn render_attachments_section(
    compose: &ComposeState,
    active_field: Option<EditorField>,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    section_card("Attachments", colors)
        .child(
            input_card(
                "compose-attachment-field",
                "Attachment Path",
                "/path/to/attachment.pdf",
                &compose.attachment_path,
                active_field == Some(EditorField::ComposeAttachmentPath),
                false,
                colors,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_active_field(EditorField::ComposeAttachmentPath, window, cx);
            })),
        )
        .child(div().flex().flex_wrap().gap_2().child(toolbar_button(
            "Add Attachment",
            Some("paperclip"),
            colors.button_background,
            colors.border,
            colors.foreground,
            cx.listener(|this: &mut WorkspaceView, _, _, cx| {
                this.add_current_attachment(cx);
            }),
        )))
        .child(if compose.attachments.is_empty() {
            div()
                .rounded_md()
                .bg(rgb(colors.background))
                .border_1()
                .border_color(rgb(colors.border))
                .px_3()
                .py_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(icon("paperclip", ICON_SIZE_SM, colors.muted))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .child("No attachments added yet."),
                        ),
                )
        } else {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .children(compose.attachments.iter().map(|attachment| {
                    render_attachment_card(attachment, colors, cx).into_any_element()
                }))
        })
}

fn render_attachment_card(
    attachment: &DraftAttachment,
    colors: &UiColors,
    cx: &mut Context<WorkspaceView>,
) -> gpui::Div {
    let attachment_id = attachment.id;

    div()
        .rounded_md()
        .bg(rgb(colors.background))
        .border_1()
        .border_color(rgb(colors.border))
        .px_3()
        .py_3()
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(icon("file-text", ICON_SIZE_SM, colors.muted))
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
                                        .child(attachment.filename.clone()),
                                )
                                .child(div().text_sm().text_color(rgb(colors.muted)).child(
                                    format!(
                                        "{} • {}",
                                        attachment.content_type,
                                        format_size(attachment.size_bytes)
                                    ),
                                )),
                        ),
                )
                .child(
                    div()
                        .id(("compose-attachment-remove", attachment.id as u64))
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
                        .on_click(cx.listener(move |this: &mut WorkspaceView, _, _, cx| {
                            this.remove_current_attachment(attachment_id, cx);
                        }))
                        .child(icon("x", ICON_SIZE_SM, colors.muted)),
                ),
        )
}
