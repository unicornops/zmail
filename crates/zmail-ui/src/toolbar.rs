//! Top toolbar render method for [`WorkspaceView`].
//!
//! This module defines a single `impl WorkspaceView` block containing
//! [`WorkspaceView::render_toolbar`], which produces the combined header-info
//! strip and action-button bar that spans the full width at the top of the
//! window.

use gpui::{Context, IntoElement, StatefulInteractiveElement, div, prelude::*, px, rgb};
use zmail_core::{APP_NAME, PHASE_LABEL, ThemeAppearance};

use super::{DetailMode, ListMode, WorkspaceView};
use crate::input::EditorField;
use crate::styles::{TOOLBAR_HEIGHT, UiColors};
use crate::widgets::{input_card, short_sha, toolbar_button, toolbar_button_id};

impl WorkspaceView {
    /// Render the top section of the workspace: a header info strip followed
    /// by the action-button bar (including the inline search field).
    ///
    /// The returned element is a `flex_col` wrapper containing both rows so
    /// it slots in as a single `.child()` of the root column.
    pub(crate) fn render_toolbar(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // ---- Precomputed strings ----------------------------------------

        let account_name = self
            .selected_account()
            .map(|a| a.display_name.clone())
            .unwrap_or_else(|| "No account selected".to_owned());

        let account_detail = self
            .selected_account()
            .map(|a| {
                format!(
                    "{} • {} identities • {}",
                    a.protocol, a.identity_count, a.cache_policy.name
                )
            })
            .unwrap_or_else(|| "Add an IMAP or JMAP account to begin.".to_owned());

        let header_detail = format!(
            "{} accounts • {} mailboxes • {} threads • {} messages",
            self.stats.account_count,
            self.stats.mailbox_count,
            self.stats.thread_count,
            self.stats.message_count
        );

        let upstream_detail = format!(
            "GPUI {} • Zed {} • {} Tokio workers",
            self.bootstrap.upstream.gpui_version,
            short_sha(&self.bootstrap.upstream.zed_commit),
            self.bootstrap.runtime.worker_threads
        );

        let add_account_id = toolbar_button_id("Add Account");
        let compose_id = toolbar_button_id("Compose");
        let mail_id = toolbar_button_id("Mail");
        let drafts_id = toolbar_button_id("Drafts");
        let outbox_id = toolbar_button_id("Outbox");
        let retry_outbox_id = toolbar_button_id("Retry Outbox");
        let run_search_id = toolbar_button_id("Run Search");
        let sync_selected_id = toolbar_button_id("Sync Selected");
        let sync_all_id = toolbar_button_id("Sync All");
        let refresh_id = toolbar_button_id("Refresh");
        let theme_toggle_id = toolbar_button_id("Theme");
        let theme_toggle_icon = match self.bootstrap.theme.appearance {
            ThemeAppearance::Dark => "sun",
            ThemeAppearance::Light => "moon",
        };

        // ---- Layout ------------------------------------------------------

        div()
            .flex()
            .flex_col()
            // ---- Header info strip ----------------------------------------
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(format!("{APP_NAME} {PHASE_LABEL}"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(colors.muted))
                                    .child(account_name),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(colors.muted))
                                    .child(account_detail),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap_1()
                            .child(div().text_sm().child(header_detail))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(colors.muted))
                                    .child(upstream_detail),
                            ),
                    ),
            )
            // ---- Toolbar action buttons + search field -------------------
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .min_h(px(TOOLBAR_HEIGHT))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(
                        toolbar_button(
                            "Add Account",
                            Some("plus"),
                            self.interactive_background(
                                &add_account_id,
                                false,
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.open_account_setup(window, cx);
                            }),
                        )
                        .on_hover(cx.listener({
                            let add_account_id = add_account_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(add_account_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Compose",
                            Some("pen-square"),
                            self.interactive_background(
                                &compose_id,
                                false,
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.open_compose(window, cx);
                            }),
                        )
                        .on_hover(cx.listener({
                            let compose_id = compose_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(compose_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Mail",
                            Some("mail"),
                            self.interactive_background(
                                &mail_id,
                                matches!(self.list_mode, ListMode::Mail),
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, _, cx| {
                                this.set_list_mode(ListMode::Mail, cx);
                            }),
                        )
                        .on_hover(cx.listener({
                            let mail_id = mail_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(mail_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Drafts",
                            Some("file-text"),
                            self.interactive_background(
                                &drafts_id,
                                matches!(self.list_mode, ListMode::Drafts),
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, _, cx| {
                                this.set_list_mode(ListMode::Drafts, cx);
                                this.detail_mode = DetailMode::Compose;
                            }),
                        )
                        .on_hover(cx.listener({
                            let drafts_id = drafts_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(drafts_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Outbox",
                            Some("send"),
                            self.interactive_background(
                                &outbox_id,
                                matches!(self.list_mode, ListMode::Outbox),
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, _, cx| {
                                this.set_list_mode(ListMode::Outbox, cx);
                            }),
                        )
                        .on_hover(cx.listener({
                            let outbox_id = outbox_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(outbox_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .when(
                        matches!(self.list_mode, ListMode::Outbox) && !self.outbox_jobs.is_empty(),
                        |this| {
                            this.child(
                                toolbar_button(
                                    "Retry Outbox",
                                    Some("refresh-cw"),
                                    self.interactive_background(
                                        &retry_outbox_id,
                                        false,
                                        colors.button_background,
                                        colors,
                                    ),
                                    colors.border,
                                    colors.foreground,
                                    cx.listener(|this: &mut Self, _, window, cx| {
                                        this.retry_selected_outbox(window, cx);
                                    }),
                                )
                                .on_hover(cx.listener({
                                    let retry_outbox_id = retry_outbox_id.clone();
                                    move |this, hovered, _, cx| {
                                        this.set_hovered_element(
                                            retry_outbox_id.clone(),
                                            *hovered,
                                            cx,
                                        );
                                    }
                                })),
                            )
                        },
                    )
                    .child(
                        div().flex_1().child(
                            input_card(
                                "search-query-field",
                                "Search",
                                "Search cached mail",
                                &self.search_query,
                                self.active_field == Some(EditorField::SearchQuery),
                                false,
                                colors,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.set_active_field(EditorField::SearchQuery, window, cx);
                                },
                            )),
                        ),
                    )
                    .child(
                        toolbar_button(
                            "Run Search",
                            Some("search"),
                            self.interactive_background(
                                &run_search_id,
                                false,
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, _, cx| {
                                this.run_search(cx);
                            }),
                        )
                        .on_hover(cx.listener({
                            let run_search_id = run_search_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(run_search_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Sync Selected",
                            Some("refresh-cw"),
                            self.interactive_background(
                                &sync_selected_id,
                                false,
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.sync_selected_account(window, cx);
                            }),
                        )
                        .on_hover(cx.listener({
                            let sync_selected_id = sync_selected_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(sync_selected_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Sync All",
                            Some("refresh-cw"),
                            self.interactive_background(
                                &sync_all_id,
                                false,
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.sync_all(window, cx);
                            }),
                        )
                        .on_hover(cx.listener({
                            let sync_all_id = sync_all_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(sync_all_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Refresh",
                            Some("refresh-cw"),
                            self.interactive_background(
                                &refresh_id,
                                false,
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, _, cx| {
                                this.refresh_from_store();
                                cx.notify();
                            }),
                        )
                        .on_hover(cx.listener({
                            let refresh_id = refresh_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(refresh_id.clone(), *hovered, cx);
                            }
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Theme",
                            Some(theme_toggle_icon),
                            self.interactive_background(
                                &theme_toggle_id,
                                false,
                                colors.button_background,
                                colors,
                            ),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, _, cx| {
                                this.toggle_theme(cx);
                            }),
                        )
                        .on_hover(cx.listener({
                            let theme_toggle_id = theme_toggle_id.clone();
                            move |this, hovered, _, cx| {
                                this.set_hovered_element(theme_toggle_id.clone(), *hovered, cx);
                            }
                        })),
                    ),
            )
    }
}
