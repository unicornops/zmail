//! A window showing every component, for review and screenshots.
//!
//! Run it with `cargo run -p zmail-ui --example gallery`. Set
//! `ZMAIL_GALLERY_APPEARANCE=light` (or `dark`) and
//! `ZMAIL_GALLERY_PLATFORM=macos|windows|linux` to pick the starting
//! theme and platform style; both can also be switched in the window.

use gpui::{
    AnyElement, App, AssetSource, Bounds, Context, DismissEvent, Entity, FocusHandle, Focusable,
    FontWeight, IntoElement, KeyBinding, MouseButton, Pixels, Point, Render, ScrollHandle,
    SharedString, Subscription, UniformListScrollHandle, Window, WindowBounds, WindowOptions,
    actions, div, prelude::*, px, rgb, size, uniform_list,
};
use zmail_core::ThemeAppearance;

use crate::styles::{
    DensityTokens, Platform, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TYPE_BODY, TYPE_CAPTION,
    TYPE_TITLE, TypeStyleExt,
};

use super::actions::{FocusNext, FocusPrevious};
use super::{
    Avatar, Badge, BadgeTone, Button, ButtonSize, ButtonVariant, ComponentTheme, IconButton, Kbd,
    Menu, MenuEntry, Modal, ModalState, PaneSide, Scrollbar, Select, SelectEvent, SkeletonRow,
    SplitHandle, Toast, ToastHost, ToastKind, overlay_at, popover_below,
};

/// Open the component gallery as the only window of a new application.
pub fn run_gallery(assets: impl AssetSource) {
    gpui_platform::application()
        .with_assets(assets)
        .run(|cx: &mut App| {
            crate::fonts::register_ui_font(cx);
            super::actions::init(cx);
            cx.bind_keys([
                KeyBinding::new("tab", FocusNext, Some(GALLERY_CONTEXT)),
                KeyBinding::new("shift-tab", FocusPrevious, Some(GALLERY_CONTEXT)),
                KeyBinding::new("secondary-z", UndoLatest, Some(GALLERY_CONTEXT)),
            ]);
            let appearance = match std::env::var("ZMAIL_GALLERY_APPEARANCE").as_deref() {
                Ok("light") => ThemeAppearance::Light,
                _ => ThemeAppearance::Dark,
            };
            let platform = match std::env::var("ZMAIL_GALLERY_PLATFORM").as_deref() {
                Ok("macos") => Platform::MacOs,
                Ok("windows") => Platform::Windows,
                Ok("linux") => Platform::Linux,
                _ => Platform::current(),
            };
            ComponentTheme::set(
                ComponentTheme::fallback(appearance).with_platform(platform),
                cx,
            );
            let bounds = Bounds::centered(None, size(px(1180.0), px(860.0)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Zmail component gallery".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            if let Err(error) = cx.open_window(options, |window, cx| {
                cx.new(|cx| Gallery::new(appearance, platform, window, cx))
            }) {
                panic!("failed to open the gallery window: {error:#}");
            }
            cx.activate(true);
        });
}

const GALLERY_CONTEXT: &str = "Gallery";

actions!(
    zmail_gallery,
    [
        /// Undo the newest toast's action.
        UndoLatest
    ]
);

const PLATFORMS: [(Platform, &str); 3] = [
    (Platform::MacOs, "macOS"),
    (Platform::Windows, "Windows"),
    (Platform::Linux, "Linux"),
];

struct Gallery {
    focus_handle: FocusHandle,
    appearance: ThemeAppearance,
    platform_select: Entity<Select>,
    density_select: Entity<Select>,
    toasts: Entity<ToastHost>,
    menu: Option<(Entity<Menu>, Option<Point<Pixels>>, Subscription)>,
    dialog: ModalState,
    sheet: ModalState,
    dialog_cancel: FocusHandle,
    folder_select: Entity<Select>,
    disabled_select: Entity<Select>,
    split_width: f32,
    detail_width: f32,
    scroll: ScrollHandle,
    list_scroll: UniformListScrollHandle,
    animate: bool,
    selected_toggle: bool,
    archived: usize,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Gallery {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Gallery {
    fn new(
        appearance: ThemeAppearance,
        platform: Platform,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let platform_index = PLATFORMS.iter().position(|(p, _)| *p == platform);
        let platform_select = cx.new(|cx| {
            Select::new(PLATFORMS.map(|(_, name)| name), platform_index, cx)
                .label("Platform style")
                .width(px(160.0))
        });
        let density_select = cx.new(|cx| {
            Select::new(["Comfortable", "Compact"], Some(0), cx)
                .label("Density")
                .width(px(160.0))
        });
        let folder_select = cx.new(|cx| {
            Select::new(["Inbox", "Archive", "Receipts", "Travel"], None, cx)
                .label("Move to folder")
                .placeholder("Move to…")
                .width(px(160.0))
        });
        let disabled_select = cx.new(|cx| {
            let mut select = Select::new(["Unavailable"], Some(0), cx)
                .label("Disabled picker")
                .width(px(140.0));
            select.set_disabled(true, cx);
            select
        });
        let subscriptions = vec![
            cx.subscribe(&platform_select, |gallery, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = *event;
                if let Some((platform, _)) = PLATFORMS.get(index) {
                    let theme = ComponentTheme::get(cx).with_platform(*platform);
                    ComponentTheme::set(theme, cx);
                    gallery.toast(format!("Platform style: {}", PLATFORMS[index].1), cx);
                }
            }),
            cx.subscribe(&density_select, |_, _, _: &SelectEvent, cx| cx.notify()),
        ];
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            focus_handle,
            appearance,
            platform_select,
            density_select,
            toasts: cx.new(|_| ToastHost::new()),
            menu: None,
            dialog: ModalState::new(cx),
            sheet: ModalState::new(cx),
            dialog_cancel: cx.focus_handle(),
            folder_select,
            disabled_select,
            split_width: 260.0,
            detail_width: 240.0,
            scroll: ScrollHandle::new(),
            list_scroll: UniformListScrollHandle::new(),
            // Screenshots want still frames.
            animate: std::env::var_os("ZMAIL_GALLERY_STATIC").is_none(),
            selected_toggle: false,
            archived: 0,
            _subscriptions: subscriptions,
        }
    }

    fn toast(&mut self, message: String, cx: &mut Context<Self>) {
        self.toasts
            .update(cx, |toasts, cx| toasts.push(Toast::new(message), cx));
    }

    fn toggle_appearance(&mut self, cx: &mut Context<Self>) {
        self.appearance = match self.appearance {
            ThemeAppearance::Light => ThemeAppearance::Dark,
            ThemeAppearance::Dark => ThemeAppearance::Light,
        };
        let platform = ComponentTheme::get(cx).platform.platform;
        ComponentTheme::set(
            ComponentTheme::fallback(self.appearance).with_platform(platform),
            cx,
        );
        cx.notify();
    }

    fn open_menu(
        &mut self,
        position: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gallery = cx.weak_entity();
        let menu = Menu::build(window, cx, |menu| {
            let archive = gallery.clone();
            menu.header("Conversation")
                .entry(
                    MenuEntry::new("Archive")
                        .icon("archive")
                        .shortcut("e")
                        .on_select(move |_, cx| {
                            archive.update(cx, |gallery, cx| gallery.archive(cx)).ok();
                        }),
                )
                .entry(
                    MenuEntry::new("Mark as unread")
                        .icon("mail")
                        .shortcut("shift-u"),
                )
                .entry(MenuEntry::new("Flag").icon("flag").shortcut("s"))
                .item("Print…", |_, _| {})
                .separator()
                .entry(MenuEntry::new("Move to Spam").disabled(true))
                .entry(
                    MenuEntry::new("Delete")
                        .icon("trash-2")
                        .shortcut("secondary-backspace"),
                )
        });
        let subscription =
            cx.subscribe_in(&menu, window, |gallery, _, _: &DismissEvent, window, cx| {
                gallery.menu = None;
                window.focus(&gallery.focus_handle, cx);
                cx.notify();
            });
        window.focus(&menu.focus_handle(cx), cx);
        self.menu = Some((menu, position, subscription));
        cx.notify();
    }

    fn archive(&mut self, cx: &mut Context<Self>) {
        self.archived += 3;
        let gallery = cx.weak_entity();
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(
                Toast::new("Archived 3 conversations")
                    .kind(ToastKind::Success)
                    .on_undo(move |_, cx| {
                        gallery
                            .update(cx, |gallery, cx| {
                                gallery.archived = gallery.archived.saturating_sub(3);
                                cx.notify();
                            })
                            .ok();
                    }),
                cx,
            )
        });
        cx.notify();
    }

    fn section(title: &str, theme: &ComponentTheme) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_3))
            .p(px(SPACE_4))
            .rounded(px(8.0))
            .bg(rgb(theme.colors.surface))
            .border_1()
            .border_color(rgb(theme.colors.border))
            .child(
                div()
                    .type_style(TYPE_CAPTION)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme.colors.muted))
                    .child(title.to_uppercase()),
            )
    }

    fn row() -> gpui::Div {
        div().flex().flex_wrap().items_center().gap(px(SPACE_3))
    }

    fn buttons(&self, theme: &ComponentTheme, cx: &mut Context<Self>) -> gpui::Div {
        let variants = [
            ("Primary", ButtonVariant::Primary),
            ("Subtle", ButtonVariant::Subtle),
            ("Ghost", ButtonVariant::Ghost),
            ("Danger", ButtonVariant::Danger),
        ];
        let sizes = [
            ("small", ButtonSize::Small),
            ("medium", ButtonSize::Medium),
            ("large", ButtonSize::Large),
        ];
        let mut section = Self::section("Buttons", theme);
        for (size_name, size) in sizes {
            section =
                section.child(Self::row().children(variants.iter().map(|(name, variant)| {
                    Button::new(
                        SharedString::from(format!("button-{name}-{size_name}")),
                        *name,
                    )
                    .variant(*variant)
                    .size(size)
                    .into_any_element()
                })));
        }
        section.child(
            Self::row()
                .child(
                    Button::new("button-icon", "Compose")
                        .primary()
                        .icon("pen-square")
                        .shortcut("secondary-n"),
                )
                .child(Button::new("button-disabled", "Disabled").disabled(true))
                .child(
                    Button::new("button-toggle", "Toggle")
                        .icon("star")
                        .tooltip("A toggle button")
                        .selected(self.selected_toggle)
                        .on_click(cx.listener(|gallery, _, _, cx| {
                            gallery.selected_toggle = !gallery.selected_toggle;
                            cx.notify();
                        })),
                )
                .child(
                    IconButton::new("icon-star", "star", "Star")
                        .selected(self.selected_toggle)
                        .on_click(cx.listener(|gallery, _, _, cx| {
                            gallery.selected_toggle = !gallery.selected_toggle;
                            cx.notify();
                        })),
                )
                .child(IconButton::new("icon-reply", "reply", "Reply").shortcut("r"))
                .child(IconButton::new("icon-archive", "archive", "Archive").shortcut("e"))
                .child(
                    IconButton::new("icon-delete", "trash-2", "Delete")
                        .variant(ButtonVariant::Subtle)
                        .shortcut("secondary-backspace"),
                )
                .child(IconButton::new("icon-disabled", "flag", "Flag").disabled(true))
                .child(IconButton::new("icon-small", "x", "Close").size(ButtonSize::Small)),
        )
    }

    fn indicators(&self, theme: &ComponentTheme) -> gpui::Div {
        let names = [
            ("Ada Lovelace", "ada@example.com"),
            ("Doe, Jane", "jane@example.com"),
            ("张伟", "wei@example.cn"),
            ("Дмитрий Иванов", "dmitri@example.ru"),
            ("🦄 Unicorn Ops", "hello@unicornops.ie"),
            ("", "billing+eu@example.com"),
            ("محمد علي", "m.ali@example.com"),
        ];
        Self::section("Badges, avatars and keys", theme)
            .child(
                Self::row()
                    .child(Badge::count(3))
                    .child(Badge::count(250))
                    .child(Badge::count(12).tone(BadgeTone::Neutral))
                    .child(Badge::label("Failed").tone(BadgeTone::Danger))
                    .child(Badge::label("Sent").tone(BadgeTone::Success))
                    .child(Badge::label("Retrying").tone(BadgeTone::Warning))
                    .child(Badge::dot()),
            )
            .child(
                Self::row()
                    .children(names.iter().map(|(name, address)| {
                        Avatar::new(*name).address(*address).into_any_element()
                    }))
                    .child(Avatar::new("Large Avatar").size(48.0)),
            )
            .child(
                Self::row().children(
                    [
                        "secondary-k",
                        "secondary-shift-z",
                        "secondary-enter",
                        "escape",
                        "shift-tab",
                        "g i",
                        "alt-up",
                    ]
                    .into_iter()
                    .map(|keys| Kbd::new(keys).into_any_element()),
                ),
            )
    }

    fn overlays(&self, theme: &ComponentTheme, cx: &mut Context<Self>) -> gpui::Div {
        let menu_below = self
            .menu
            .as_ref()
            .filter(|(_, position, _)| position.is_none())
            .map(|(menu, _, _)| menu.clone());
        Self::section("Menus, selects, dialogs and toasts", theme)
            .child(
                Self::row()
                    .child(
                        div()
                            .relative()
                            .child(
                                Button::new("open-menu", "Actions")
                                    .icon("chevron-down")
                                    .on_click(cx.listener(|gallery, _, window, cx| {
                                        gallery.open_menu(None, window, cx);
                                    })),
                            )
                            .when_some(menu_below, |this, menu| this.child(popover_below(menu))),
                    )
                    .child(self.platform_select.clone())
                    .child(self.density_select.clone())
                    .child(self.folder_select.clone())
                    .child(self.disabled_select.clone())
                    .child(
                        Button::new("reset-density", "Reset density")
                            .ghost()
                            .on_click(cx.listener(|gallery, _, _, cx| {
                                gallery
                                    .density_select
                                    .update(cx, |select, cx| select.set_selected(Some(0), cx));
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("open-dialog", "Delete…")
                            .danger()
                            .on_click(cx.listener(|gallery, _, window, cx| {
                                let cancel = gallery.dialog_cancel.clone();
                                gallery.dialog.open_focusing(&cancel, window, cx);
                                cx.notify();
                            })),
                    )
                    .child(Button::new("open-sheet", "Sheet").on_click(cx.listener(
                        |gallery, _, window, cx| {
                            gallery.sheet.open(window, cx);
                            cx.notify();
                        },
                    )))
                    .child(
                        Button::new("push-toast", "Archive with undo")
                            .icon("archive")
                            .on_click(cx.listener(|gallery, _, _, cx| gallery.archive(cx))),
                    )
                    .child(
                        Button::new("push-error", "Fail to send")
                            .ghost()
                            .on_click(cx.listener(|gallery, _, _, cx| {
                                gallery.toasts.update(cx, |toasts, cx| {
                                    toasts.push(
                                        Toast::new("Could not reach smtp.example.com")
                                            .kind(ToastKind::Error)
                                            .duration(std::time::Duration::from_secs(12))
                                            .action("Retry", |_, _| {}),
                                        cx,
                                    )
                                });
                            })),
                    )
                    .child(
                        div()
                            .type_style(TYPE_BODY)
                            .text_color(rgb(theme.colors.muted))
                            .child(format!("{} archived (secondary-z undoes)", self.archived)),
                    ),
            )
            .child(
                div()
                    .id("context-area")
                    .p(px(SPACE_4))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(theme.colors.control_border))
                    .type_style(TYPE_BODY)
                    .text_color(rgb(theme.colors.muted))
                    .child("Right-click here for a context menu")
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|gallery, event: &gpui::MouseDownEvent, window, cx| {
                            gallery.open_menu(Some(event.position), window, cx);
                        }),
                    ),
            )
    }

    fn layout(&self, theme: &ComponentTheme, cx: &mut Context<Self>) -> gpui::Div {
        let density = match self.density_select.read(cx).selected_index() {
            Some(1) => DensityTokens::COMPACT,
            _ => DensityTokens::COMFORTABLE,
        };
        let colors = theme.colors;
        let animate = self.animate;
        Self::section("Split handles, scrollbar and skeleton rows", theme).child(
            div()
                .flex()
                .h(px(260.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(colors.border))
                .overflow_hidden()
                .bg(rgb(colors.background))
                .child(
                    div()
                        .flex_none()
                        .w(px(self.split_width))
                        .h_full()
                        .p(px(SPACE_3))
                        .type_style(TYPE_BODY)
                        .child(format!(
                            "Pane: {}px. Drag a divider, or focus it and use the arrow keys.",
                            self.split_width
                        )),
                )
                .child(
                    SplitHandle::new("gallery-split", self.split_width)
                        .range(180.0, 520.0)
                        .default_width(260.0)
                        .on_resize(cx.listener(|gallery, width: &f32, _, cx| {
                            gallery.split_width = *width;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(
                            uniform_list("skeleton-list", 200, move |range, _, _| {
                                range
                                    .map(|index| {
                                        SkeletonRow::new(("skeleton", index))
                                            .density(density)
                                            .animated(animate)
                                            .into_any_element()
                                    })
                                    .collect()
                            })
                            .size_full()
                            .track_scroll(&self.list_scroll),
                        )
                        .child(Scrollbar::for_uniform_list(
                            "skeleton-scrollbar",
                            &self.list_scroll,
                        )),
                )
                .child(
                    SplitHandle::new("gallery-detail-split", self.detail_width)
                        .side(PaneSide::Trailing)
                        .range(160.0, 400.0)
                        .default_width(240.0)
                        .on_resize(cx.listener(|gallery, width: &f32, _, cx| {
                            gallery.detail_width = *width;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(self.detail_width))
                        .h_full()
                        .p(px(SPACE_3))
                        .type_style(TYPE_BODY)
                        .child(format!("Trailing pane: {}px", self.detail_width)),
                ),
        )
    }

    fn modals(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut modals = Vec::new();
        if self.dialog.is_open() {
            modals.push(
                Modal::new("gallery-dialog", self.dialog.focus_handle())
                    .title("Delete 3 conversations?")
                    .description("They move to Trash, where they are deleted after 30 days.")
                    .child("Tab and Shift+Tab stay inside this dialog; Escape closes it.")
                    .footer(
                        Button::new("dialog-cancel", "Cancel")
                            .track_focus(&self.dialog_cancel)
                            .on_click(cx.listener(|gallery, _, window, cx| {
                                gallery.dialog.close(window, cx);
                                cx.notify();
                            })),
                    )
                    .footer(
                        Button::new("dialog-delete", "Delete")
                            .danger()
                            .on_click(cx.listener(|gallery, _, window, cx| {
                                gallery.dialog.close(window, cx);
                                gallery.toast("Deleted 3 conversations".into(), cx);
                                cx.notify();
                            })),
                    )
                    .on_dismiss(cx.listener(|gallery, _, window, cx| {
                        gallery.dialog.close(window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if self.sheet.is_open() {
            modals.push(
                Modal::new("gallery-sheet", self.sheet.focus_handle())
                    .sheet()
                    .persistent()
                    .title("Add account")
                    .description("A sheet hangs from the top of the window, as on macOS.")
                    .width(px(520.0))
                    .child(
                        Button::new("sheet-oauth", "Sign in with your provider")
                            .icon("key")
                            .full_width(),
                    )
                    .footer(
                        Button::new("sheet-done", "Done")
                            .primary()
                            .on_click(cx.listener(|gallery, _, window, cx| {
                                gallery.sheet.close(window, cx);
                                cx.notify();
                            })),
                    )
                    .on_dismiss(cx.listener(|gallery, _, window, cx| {
                        gallery.sheet.close(window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        modals
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let context_menu = self
            .menu
            .as_ref()
            .and_then(|(menu, position, _)| position.map(|position| (menu.clone(), position)));
        let is_light = matches!(self.appearance, ThemeAppearance::Light);

        div()
            .id("gallery")
            .track_focus(&self.focus_handle)
            .key_context(GALLERY_CONTEXT)
            // Tab between controls (the workspace keymap does this in the
            // app). A modal's own Tab binding is deeper, so it wins.
            .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
            .on_action(|_: &FocusPrevious, window, cx| window.focus_prev(cx))
            .on_action(cx.listener(|gallery, _: &UndoLatest, window, cx| {
                gallery
                    .toasts
                    .update(cx, |toasts, cx| toasts.undo_latest(window, cx));
            }))
            .relative()
            .size_full()
            .font_family(theme.platform.ui_font.family())
            .bg(rgb(colors.background))
            .text_color(rgb(colors.foreground))
            .type_style(TYPE_BODY)
            .child(
                div()
                    .id("gallery-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_4))
                            .p(px(SPACE_6))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(div().type_style(TYPE_TITLE).child("Zmail components"))
                                    .child(
                                        div().flex().gap(px(SPACE_2)).child(
                                            Button::new(
                                                "toggle-theme",
                                                if is_light { "Dark" } else { "Light" },
                                            )
                                            .icon(if is_light { "moon" } else { "sun" })
                                            .on_click(
                                                cx.listener(|gallery, _, _, cx| {
                                                    gallery.toggle_appearance(cx);
                                                }),
                                            ),
                                        ),
                                    ),
                            )
                            .child(self.buttons(&theme, cx))
                            .child(self.indicators(&theme))
                            .child(self.overlays(&theme, cx))
                            .child(self.layout(&theme, cx)),
                    ),
            )
            .child(Scrollbar::vertical("gallery-scrollbar", &self.scroll))
            .when_some(context_menu, |this, (menu, position)| {
                this.child(overlay_at(position, menu))
            })
            .children(self.modals(cx))
            .child(self.toasts.clone())
    }
}
