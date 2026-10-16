//! Sidebar render method for [`WorkspaceView`].
//!
//! This module defines a single `impl WorkspaceView` block containing
//! [`WorkspaceView::render_sidebar`], which produces the left-hand sidebar
//! showing the accounts list and the virtualised mailbox list for the
//! selected account.

use std::ops::Range;

use gpui::{
    Context, FontWeight, IntoElement, SharedString, StatefulInteractiveElement, div, prelude::*,
    px, rgb, uniform_list,
};

use super::WorkspaceView;
use crate::styles::{
    AVATAR_SIZE, ICON_SIZE_SM, MAILBOX_ITEM_HEIGHT, SIDEBAR_MIN_WIDTH, SIDEBAR_WIDTH, UiColors,
};
use crate::widgets::{EllipsisExt, avatar, badge, icon, message_avatar_background, sync_badge};

impl WorkspaceView {
    /// Render the left sidebar pane: accounts header, account items, mailboxes
    /// header, and the virtualised mailbox list for the selected account.
    pub(crate) fn render_sidebar(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // UiColors is Copy — safe to capture by value in the processor closure.
        let colors = *colors;

        div()
            .w(px(SIDEBAR_WIDTH))
            // The sidebar gives up width before the reader does when the
            // window is too narrow for all three panes.
            .flex_shrink()
            .min_w(px(SIDEBAR_MIN_WIDTH))
            .flex()
            .flex_col()
            .bg(rgb(colors.surface))
            .border_r_1()
            .border_color(rgb(colors.border))
            // ---- Accounts header -----------------------------------------
            .child(
                div()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Accounts")),
            )
            // ---- Account items -------------------------------------------
            .child(
                div()
                    .flex()
                    .flex_col()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .children(self.accounts.iter().map(|account| {
                        let account_id = account.id;
                        let is_selected = Some(account_id) == self.selected_account_id;
                        let element_id = account_row_id(account_id);
                        let background_color = rgb(self.interactive_background(
                            &element_id,
                            is_selected,
                            colors.surface,
                            &colors,
                        ));
                        let (badge_icon, badge_color, badge_text) =
                            sync_badge(account.sync_status.phase, &colors);
                        let avatar_color =
                            message_avatar_background(&account.display_name, &colors);

                        div()
                            .id(element_id.clone())
                            .px_3()
                            .py_3()
                            .cursor_pointer()
                            .border_b_1()
                            .border_color(rgb(colors.border))
                            .bg(background_color)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_account(account_id, cx);
                            }))
                            .on_hover(cx.listener({
                                let element_id = element_id.clone();
                                move |this, hovered, _, cx| {
                                    this.set_hovered_element(element_id.clone(), *hovered, cx);
                                }
                            }))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .items_start()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_1()
                                            .min_w_0()
                                            .items_start()
                                            .gap_3()
                                            .child(avatar(
                                                &account.display_name,
                                                AVATAR_SIZE,
                                                avatar_color,
                                                &colors,
                                            ))
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .ellipsis()
                                                            .child(account.display_name.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(rgb(colors.muted))
                                                            .ellipsis()
                                                            .child(account.username.clone()),
                                                    )
                                                    .children(
                                                        (account.sync_status.failure_count > 0)
                                                            .then(|| {
                                                                div()
                                                                    .text_sm()
                                                                    .text_color(rgb(colors.danger))
                                                                    .ellipsis()
                                                                    .child(format!(
                                                                        "Sync error: {}",
                                                                        account
                                                                            .sync_status
                                                                            .last_error_message
                                                                            .as_deref()
                                                                            .unwrap_or("unknown"),
                                                                    ))
                                                            }),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_none()
                                            .items_center()
                                            .gap_1()
                                            .px_2()
                                            .py_1()
                                            .rounded_full()
                                            .bg(rgb(colors.button_background))
                                            .child(icon(badge_icon, ICON_SIZE_SM, badge_color))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(badge_color))
                                                    .child(badge_text),
                                            ),
                                    ),
                            )
                    })),
            )
            // ---- Mailboxes header ----------------------------------------
            .child(
                div()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Mailboxes")),
            )
            // ---- Virtualised mailbox list --------------------------------
            .child(
                div().flex_1().overflow_hidden().child(
                    uniform_list(
                        "mailboxes",
                        self.mailboxes.len(),
                        cx.processor(move |this, range: Range<usize>, _window, cx| {
                            let mut items = Vec::new();
                            for index in range {
                                let Some(mailbox) = this.mailboxes.get(index).cloned() else {
                                    continue;
                                };
                                let is_selected = Some(mailbox.id) == this.selected_mailbox_id;
                                let element_id = mailbox_row_id(mailbox.id);
                                let background_color = rgb(this.interactive_background(
                                    &element_id,
                                    is_selected,
                                    colors.surface,
                                    &colors,
                                ));
                                let icon_name = mailbox_icon_name(&mailbox.name);

                                items.push(
                                    div()
                                        .id(element_id.clone())
                                        .w_full()
                                        .h(px(MAILBOX_ITEM_HEIGHT))
                                        .px_3()
                                        .items_center()
                                        .cursor_pointer()
                                        .border_b_1()
                                        .border_color(rgb(colors.border))
                                        .bg(background_color)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select_mailbox(mailbox.id, cx);
                                        }))
                                        .on_hover(cx.listener({
                                            let element_id = element_id.clone();
                                            move |this, hovered, _, cx| {
                                                this.set_hovered_element(
                                                    element_id.clone(),
                                                    *hovered,
                                                    cx,
                                                );
                                            }
                                        }))
                                        .child(
                                            div()
                                                .flex()
                                                .justify_between()
                                                .items_center()
                                                .w_full()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .child(icon(
                                                            icon_name,
                                                            ICON_SIZE_SM,
                                                            colors.muted,
                                                        ))
                                                        .child(
                                                            div()
                                                                .ellipsis()
                                                                .child(mailbox.name.clone()),
                                                        ),
                                                )
                                                .when(mailbox.unread_threads > 0, |this| {
                                                    this.child(badge(
                                                        mailbox.unread_threads,
                                                        colors.accent,
                                                        colors.background,
                                                    ))
                                                }),
                                        ),
                                );
                            }
                            items
                        }),
                    )
                    .track_scroll(self.mailbox_scroll.clone())
                    .h_full(),
                ),
            )
    }
}

fn account_row_id(account_id: i64) -> SharedString {
    SharedString::from(format!("account-row-{account_id}"))
}

fn mailbox_row_id(mailbox_id: i64) -> SharedString {
    SharedString::from(format!("mailbox-row-{mailbox_id}"))
}

fn mailbox_icon_name(name: &str) -> &'static str {
    let normalized = name.trim().to_ascii_lowercase();

    if normalized == "inbox" {
        "inbox"
    } else if normalized == "sent" || normalized == "sent items" {
        "send"
    } else if normalized == "drafts" {
        "file-text"
    } else if normalized == "archive" {
        "archive"
    } else if normalized == "trash"
        || normalized == "junk"
        || normalized == "spam"
        || normalized == "junk email"
    {
        "trash-2"
    } else {
        "folder"
    }
}
