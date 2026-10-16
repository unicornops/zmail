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
use crate::widgets::{HoverExt, interactive_background, search_field, toolbar_button};

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
                            interactive_background(false, colors.button_background, colors),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.open_account_setup(window, cx);
                            }),
                        )
                        .hover_bg(false, colors),
                    )
                    .child(
                        toolbar_button(
                            "Compose",
                            Some("pen-square"),
                            interactive_background(false, colors.button_background, colors),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.open_compose(window, cx);
                            }),
                        )
                        .hover_bg(false, colors),
                    )
                    .child(
                        toolbar_button(
                            "Mail",
                            Some("mail"),
                            interactive_background(
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
                        .hover_bg(matches!(self.list_mode, ListMode::Mail), colors),
                    )
                    .child(
                        toolbar_button(
                            "Drafts",
                            Some("file-text"),
                            interactive_background(
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
                        .hover_bg(matches!(self.list_mode, ListMode::Drafts), colors),
                    )
                    .child(
                        toolbar_button(
                            "Outbox",
                            Some("send"),
                            interactive_background(
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
                        .hover_bg(matches!(self.list_mode, ListMode::Outbox), colors),
                    )
                    .when(
                        matches!(self.list_mode, ListMode::Outbox) && !self.outbox_jobs.is_empty(),
                        |this| {
                            this.child(
                                toolbar_button(
                                    "Retry Outbox",
                                    Some("refresh-cw"),
                                    interactive_background(false, colors.button_background, colors),
                                    colors.border,
                                    colors.foreground,
                                    cx.listener(|this: &mut Self, _, window, cx| {
                                        this.retry_selected_outbox(window, cx);
                                    }),
                                )
                                .hover_bg(false, colors),
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
                            interactive_background(false, colors.button_background, colors),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, _, cx| {
                                this.run_search(cx);
                            }),
                        )
                        .hover_bg(false, colors),
                    )
                    .child(
                        toolbar_button(
                            "Sync Selected",
                            Some("refresh-cw"),
                            interactive_background(false, colors.button_background, colors),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.sync_selected_account(window, cx);
                            }),
                        )
                        .hover_bg(false, colors),
                    )
                    .child(
                        toolbar_button(
                            "Sync All",
                            Some("refresh-cw"),
                            interactive_background(false, colors.button_background, colors),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.sync_all(window, cx);
                            }),
                        )
                        .hover_bg(false, colors),
                    )
                    .child(
                        toolbar_button(
                            "Theme",
                            Some(theme_toggle_icon),
                            interactive_background(false, colors.button_background, colors),
                            colors.border,
                            colors.foreground,
                            cx.listener(|this: &mut Self, _, window, cx| {
                                this.toggle_theme(cx);
                                this.persist_theme_choice(window.appearance());
                            }),
                        )
                        .hover_bg(false, colors),
                    ),
            )
    }
}
