//! The shell's toolbar: command buttons and the search field.
//!
//! Every button dispatches its action from the window root, exactly like
//! its key binding. Clicking the toolbar leaves keyboard focus where it
//! was (only the search field takes focus), so Sync or Theme do not pull
//! focus out of the message list.

use gpui::{Context, IntoElement, MouseButton, div, prelude::*, px, rgb};
use zmail_core::ThemeAppearance;

use super::{ListMode, WorkspaceView};
use crate::actions::{
    AddAccount, NewMessage, RetryOutbox, RunSearch, ShowDrafts, ShowMail, ShowOutbox, SyncAccount,
    SyncAll, ToggleTheme,
};
use crate::pane::dispatch_from;
use crate::styles::{MenuPlacement, PlatformStyle, SEARCH_FIELD_WIDTH, TOOLBAR_HEIGHT, UiColors};
use crate::widgets::{HoverExt, interactive_background, toolbar_button};

impl WorkspaceView {
    /// Render the top section of the workspace: the action-button bar,
    /// including the inline search field.
    ///
    /// Internal diagnostics that used to sit above the buttons live in
    /// [`crate::diagnostics`] until the Settings → About page exists.
    pub(crate) fn render_toolbar(
        &self,
        colors: &UiColors,
        _cx: &mut Context<Self>,
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
                    // Buttons here do not take keyboard focus; the search
                    // field focuses itself before this runs.
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(
                        toolbar_button(
                            "Add Account",
                            Some("plus"),
                            interactive_background(false, colors.button_background, colors),
                            colors.border,
                            colors.foreground,
                            dispatch_from(&self.focus_handle, AddAccount),
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
                            dispatch_from(&self.focus_handle, NewMessage),
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
                            dispatch_from(&self.focus_handle, ShowMail),
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
                            dispatch_from(&self.focus_handle, ShowDrafts),
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
                            dispatch_from(&self.focus_handle, ShowOutbox),
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
                                    dispatch_from(&self.focus_handle, RetryOutbox),
                                )
                                .hover_bg(false, colors),
                            )
                        },
                    )
                    // Fixed width so typing never shifts the buttons beside
                    // it; long queries scroll inside the field.
                    .child(
                        div()
                            .flex_none()
                            .w(px(SEARCH_FIELD_WIDTH))
                            .child(self.search_query.clone()),
                    )
                    .child(
                        toolbar_button(
                            "Search",
                            Some("search"),
                            interactive_background(false, colors.button_background, colors),
                            colors.border,
                            colors.foreground,
                            dispatch_from(&self.focus_handle, RunSearch),
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
                            dispatch_from(&self.focus_handle, SyncAccount),
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
                            dispatch_from(&self.focus_handle, SyncAll),
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
                            dispatch_from(&self.focus_handle, ToggleTheme),
                        )
                        .hover_bg(false, colors),
                    )
                    // The command menu (macOS has the menu bar instead).
                    .when(
                        PlatformStyle::current().menu != MenuPlacement::MenuBar,
                        |this| this.child(div().ml_auto().child(self.app_menu.clone())),
                    ),
            )
    }
}
