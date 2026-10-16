//! Top toolbar render method for [`WorkspaceView`].
//!
//! This module defines a single `impl WorkspaceView` block containing
//! [`WorkspaceView::render_toolbar`], which produces the action-button bar
//! that spans the full width at the top of the window.

use gpui::{Context, IntoElement, StatefulInteractiveElement, div, prelude::*, px, rgb};
use zmail_core::ThemeAppearance;

use super::{DetailMode, ListMode, WorkspaceView};
use crate::input::EditorField;
use crate::styles::{TOOLBAR_HEIGHT, UiColors};
use crate::widgets::{search_field, toolbar_button, toolbar_button_id};

impl WorkspaceView {
    /// Render the top section of the workspace: the action-button bar,
    /// including the inline search field.
    ///
    /// Internal diagnostics that used to sit above the buttons live in
    /// [`crate::diagnostics`] until the Settings → About page exists.
    pub(crate) fn render_toolbar(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let add_account_id = toolbar_button_id("Add Account");
        let compose_id = toolbar_button_id("Compose");
        let mail_id = toolbar_button_id("Mail");
        let drafts_id = toolbar_button_id("Drafts");
        let outbox_id = toolbar_button_id("Outbox");
        let retry_outbox_id = toolbar_button_id("Retry Outbox");
        let run_search_id = toolbar_button_id("Search");
        let sync_selected_id = toolbar_button_id("Sync Selected");
        let sync_all_id = toolbar_button_id("Sync All");
        let theme_toggle_id = toolbar_button_id("Theme");
        let theme_toggle_icon = match self.bootstrap.theme.appearance {
            ThemeAppearance::Dark => "sun",
            ThemeAppearance::Light => "moon",
        };

        // ---- Layout ------------------------------------------------------

        div()
            .flex()
            .flex_col()
            // ---- Toolbar action buttons + search field -------------------
            .child(
                div()
                    .flex()
                    // Wrap onto a second row instead of running off the
                    // right edge in narrow windows (the toolbar redesign is
                    // Phase 2 of the UI plan).
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
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
                        search_field(
                            "search-query-field",
                            "Search mail",
                            &self.search_query,
                            self.active_field == Some(EditorField::SearchQuery),
                            colors,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_active_field(EditorField::SearchQuery, window, cx);
                        })),
                    )
                    .child(
                        toolbar_button(
                            "Search",
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
