//! `TitleBar`: the window's title bar, with the app's own content (the
//! toolbar) inside it, Zed-style.
//!
//! Ported from Zed's `platform_title_bar` crate (Zed v1.24.2,
//! <https://github.com/zed-industries/zed>, Copyright Zed Industries, Inc.,
//! GPL-3.0-or-later; used here under GPL-3.0-only).
//!
//! What it draws depends on the platform
//! ([`PlatformStyle`](crate::styles::PlatformStyle)) and on what
//! the window system gives the window ([`WindowChrome`]):
//!
//! - **macOS:** the system title bar is transparent and the traffic lights
//!   sit over the bar's leading edge, so the content starts after them.
//!   Dragging the bar moves the window; a double-click does whatever the
//!   system's title-bar setting says (zoom or minimise).
//! - **Windows:** Minimise, Maximise/Restore and Close are drawn with
//!   Segoe Fluent Icons glyphs and tagged with `WindowControlArea`, so the
//!   OS hit-tests them as the real caption buttons (Snap Layouts on hover
//!   over Maximise). The rest of the bar is the caption drag area. The
//!   close button follows the window's rounded corner, square when
//!   maximised.
//! - **Linux:** with client-side decorations (`Decorations::Client`) the
//!   bar is the header bar: window controls on the side the desktop's
//!   button layout puts them, only those `window_controls()` allows;
//!   dragging moves the window, a double-click maximises and a right-click
//!   opens the window menu. With server-side decorations the window manager
//!   draws the title bar, so this is just the toolbar row.
//!
//! The trailing slot holds the app-menu button (Windows and Linux), which
//! sits before the window controls.
//!
//! Every interactive element placed in the bar must call `.occlude()`:
//! otherwise Windows hit-tests a click on it as the caption drag area (the
//! element never sees it), and on macOS and Linux a press-and-move on it
//! starts a window move.

use gpui::{
    AnyElement, App, Decorations, ElementId, IntoElement, MAX_BUTTONS_PER_SIDE, MouseButton,
    ParentElement, RenderOnce, Rgba, SharedString, Tiling, Window, WindowButton,
    WindowButtonLayout, WindowControlArea, WindowControls, div, prelude::*, px, rgb,
};

use crate::a11y::{A11yFlags, AccessibleExt};
use crate::styles::{ICON_SIZE_SM, Platform};
use crate::widgets::icon;

use super::theme::ComponentTheme;

/// Space the macOS traffic lights take at the bar's leading edge: their
/// inset ([`TRAFFIC_LIGHT_POSITION`]) plus three buttons and the gaps
/// between them, with room to spare for macOS 26's larger buttons.
pub(crate) const TRAFFIC_LIGHT_PADDING: f32 = 84.0;

/// Where the macOS traffic lights sit, from the window's top-left corner.
/// Vertically centred in the 52px unified title bar.
pub(crate) const TRAFFIC_LIGHT_POSITION: (f32, f32) = (16.0, 19.0);

/// Width of a Windows caption button (Windows 11 uses 46px).
const WINDOWS_CAPTION_BUTTON_WIDTH: f32 = 46.0;

/// Diameter of a Linux header-bar window control (libadwaita uses 24px).
const LINUX_CONTROL_SIZE: f32 = 24.0;

/// The Windows 11 caption icon font. Windows 10 does not ship it, and
/// uses the older MDL2 font, which has the same glyphs at the same code
/// points.
const WINDOWS_ICON_FONTS: [&str; 2] = ["Segoe Fluent Icons", "Segoe MDL2 Assets"];

/// The window state the title bar is drawn for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WindowChrome {
    /// Who draws the decorations, and which edges are tiled.
    pub decorations: Decorations,
    /// Which window controls the window system supports.
    pub controls: WindowControls,
    /// The desktop's window-button layout (Linux).
    pub button_layout: Option<WindowButtonLayout>,
    /// Whether the window is maximised.
    pub maximized: bool,
    /// Whether the window is full screen.
    pub fullscreen: bool,
    /// Whether the window can be resized (so maximised).
    pub resizable: bool,
    /// Whether the window can be minimised.
    pub minimizable: bool,
}

impl WindowChrome {
    /// The state of `window`.
    pub(crate) fn of(window: &Window, cx: &App) -> Self {
        Self {
            decorations: window.window_decorations(),
            controls: window.window_controls(),
            button_layout: cx.button_layout(),
            maximized: window.is_maximized(),
            fullscreen: window.is_fullscreen(),
            resizable: window.is_resizable(),
            minimizable: window.is_minimizable(),
        }
    }

    /// Whether the app draws the window's decorations (Linux CSD).
    pub(crate) fn client_decorated(&self) -> bool {
        matches!(self.decorations, Decorations::Client { .. })
    }

    /// The tiled edges (none with server-side decorations).
    pub(crate) fn tiling(&self) -> Tiling {
        match self.decorations {
            Decorations::Client { tiling } => tiling,
            Decorations::Server => Tiling::default(),
        }
    }
}

/// Whether this bar acts as the window's title bar on `platform` (moves
/// the window, double-click maximises), rather than being a toolbar under
/// a title bar the window manager draws.
fn is_window_title_bar(platform: Platform, chrome: &WindowChrome) -> bool {
    match platform {
        Platform::MacOs | Platform::Windows => true,
        Platform::Linux => chrome.client_decorated(),
    }
}

/// The Linux window controls for each side of the bar: the desktop's
/// layout (GNOME's `gtk-decoration-layout`, or minimise, maximise, close on
/// the right when it has none), without the buttons the window system
/// does not support. Empty unless the app draws the decorations.
pub(crate) fn linux_controls(chrome: &WindowChrome) -> (Vec<WindowButton>, Vec<WindowButton>) {
    if !chrome.client_decorated() || chrome.fullscreen {
        return (Vec::new(), Vec::new());
    }
    let layout = chrome.button_layout.unwrap_or(WindowButtonLayout {
        left: [None; MAX_BUTTONS_PER_SIDE],
        right: [
            Some(WindowButton::Minimize),
            Some(WindowButton::Maximize),
            Some(WindowButton::Close),
        ],
    });
    let supported = |side: [Option<WindowButton>; MAX_BUTTONS_PER_SIDE]| {
        side.into_iter()
            .flatten()
            .filter(|button| match button {
                WindowButton::Minimize => chrome.controls.minimize,
                WindowButton::Maximize => chrome.controls.maximize,
                WindowButton::Close => true,
            })
            .collect::<Vec<_>>()
    };
    (supported(layout.left), supported(layout.right))
}

/// The window's title bar. See the module documentation.
#[derive(IntoElement)]
pub(crate) struct TitleBar {
    id: ElementId,
    children: Vec<AnyElement>,
    trailing: Option<AnyElement>,
    chrome: Option<WindowChrome>,
}

impl TitleBar {
    /// An empty title bar.
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            children: Vec::new(),
            trailing: None,
            chrome: None,
        }
    }

    /// Put `element` (the app-menu button) at the trailing end, before the
    /// window controls.
    pub(crate) fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    /// Draw for `chrome` instead of the window's real state (tests: GPUI's
    /// test window always reports server-side decorations).
    #[cfg(test)]
    pub(crate) fn chrome(mut self, chrome: WindowChrome) -> Self {
        self.chrome = Some(chrome);
        self
    }
}

impl ParentElement for TitleBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let style = theme.platform;
        let platform = style.platform;
        let chrome = self.chrome.unwrap_or_else(|| WindowChrome::of(window, cx));
        let colors = theme.colors;
        let title_bar = is_window_title_bar(platform, &chrome);
        let tiling = chrome.tiling();
        let radius = px(style.window_radius_for(chrome.maximized));
        let (left_controls, right_controls) = match platform {
            Platform::Linux => linux_controls(&chrome),
            _ => (Vec::new(), Vec::new()),
        };

        // Press-and-move starts a window move; a plain click does not, so
        // double-clicks still reach the bar. The state lives with the
        // element across frames.
        let should_move = window.use_keyed_state(
            ElementId::from(SharedString::from("title-bar-should-move")),
            cx,
            |_, _| false,
        );

        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .min_h(px(style.title_bar_height))
            .bg(rgb(colors.background))
            .border_b_1()
            .border_color(rgb(colors.border))
            // The bar's top corners follow the window frame's rounding.
            .when(chrome.client_decorated(), |this| {
                this.when(!(tiling.top || tiling.left), |this| this.rounded_tl(radius))
                    .when(!(tiling.top || tiling.right), |this| {
                        this.rounded_tr(radius)
                    })
            })
            .when(platform == Platform::Windows, |this| {
                this.window_control_area(WindowControlArea::Drag)
            })
            .when(title_bar && platform != Platform::Windows, |this| {
                let down = should_move.clone();
                let up = should_move.clone();
                let out = should_move.clone();
                let moved = should_move.clone();
                this.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    down.update(cx, |should_move, _| *should_move = true);
                })
                .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                    up.update(cx, |should_move, _| *should_move = false);
                })
                .on_mouse_down_out(move |_, _, cx| {
                    out.update(cx, |should_move, _| *should_move = false);
                })
                .on_mouse_move(move |_, window, cx| {
                    if *moved.read(cx) {
                        moved.update(cx, |should_move, _| *should_move = false);
                        window.start_window_move();
                    }
                })
                .on_click(move |event, window, _| {
                    if event.click_count() != 2 {
                        return;
                    }
                    if platform == Platform::MacOs {
                        window.titlebar_double_click();
                    } else if chrome.controls.maximize && chrome.resizable {
                        window.zoom_window();
                    }
                })
            })
            .when(
                platform == Platform::Linux && title_bar && chrome.controls.window_menu,
                |this| {
                    this.on_mouse_down(MouseButton::Right, |event, window, _| {
                        window.show_window_menu(event.position)
                    })
                },
            )
            // Leading edge: room for the traffic lights, or the left-hand
            // Linux controls.
            .map(|this| {
                if platform == Platform::MacOs && !chrome.fullscreen {
                    this.pl(px(TRAFFIC_LIGHT_PADDING))
                } else if !left_controls.is_empty() {
                    this.child(linux_window_controls(
                        "left-window-controls",
                        &left_controls,
                        &chrome,
                        &theme,
                    ))
                } else {
                    this
                }
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .children(self.children),
            )
            .children(
                self.trailing
                    .map(|trailing| div().flex_none().pr_2().child(trailing)),
            )
            .when(!right_controls.is_empty(), |this| {
                this.child(linux_window_controls(
                    "right-window-controls",
                    &right_controls,
                    &chrome,
                    &theme,
                ))
            })
            .when(
                platform == Platform::Windows && !chrome.fullscreen,
                |this| this.child(windows_caption_buttons(&chrome, radius, &theme, cx)),
            )
    }
}

/// The Linux header-bar controls: round icon buttons, like libadwaita's.
fn linux_window_controls(
    id: &'static str,
    buttons: &[WindowButton],
    chrome: &WindowChrome,
    theme: &ComponentTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let fill = theme.hover_tint(colors.background);
    let hover = theme.active_tint(colors.background);
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_3()
        .px_3()
        .children(buttons.iter().map(|button| {
            let (control_id, icon_name, label) = match button {
                WindowButton::Minimize => ("window-minimize", "minus", "Minimize"),
                WindowButton::Maximize if chrome.maximized => ("window-restore", "copy", "Restore"),
                WindowButton::Maximize => ("window-maximize", "square", "Maximize"),
                WindowButton::Close => ("window-close", "x", "Close"),
            };
            let button = *button;
            let enabled = match button {
                WindowButton::Minimize => chrome.minimizable,
                WindowButton::Maximize => chrome.resizable,
                WindowButton::Close => true,
            };
            div()
                .id(control_id)
                .debug_selector(move || control_id.to_owned())
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(LINUX_CONTROL_SIZE))
                .rounded_full()
                .bg(rgb(fill))
                .occlude()
                .accessible(gpui::Role::Button, label)
                .a11y_flags(A11yFlags::disabled(!enabled))
                .when(!enabled, |this| this.opacity(0.45))
                .when(enabled, |this| {
                    this.cursor_pointer()
                        .hover(move |style| style.bg(rgb(hover)))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            match button {
                                WindowButton::Minimize => window.minimize_window(),
                                WindowButton::Maximize => window.zoom_window(),
                                WindowButton::Close => window.remove_window(),
                            }
                        })
                })
                .child(icon(icon_name, ICON_SIZE_SM, colors.foreground))
        }))
}

/// The Windows caption icon font this system has.
fn windows_icon_font(cx: &App) -> &'static str {
    let installed = cx.text_system().all_font_names();
    WINDOWS_ICON_FONTS
        .into_iter()
        .find(|font| installed.iter().any(|name| name == font))
        .unwrap_or(WINDOWS_ICON_FONTS[0])
}

/// Minimise, Maximise/Restore and Close, as Windows 11 draws them.
fn windows_caption_buttons(
    chrome: &WindowChrome,
    window_radius: gpui::Pixels,
    theme: &ComponentTheme,
    cx: &App,
) -> impl IntoElement {
    let colors = theme.colors;
    let font = windows_icon_font(cx);
    let buttons = [
        (
            "minimize",
            "Minimize",
            "\u{e921}",
            WindowControlArea::Min,
            chrome.minimizable,
        ),
        if chrome.maximized {
            (
                "restore",
                "Restore",
                "\u{e923}",
                WindowControlArea::Max,
                chrome.resizable,
            )
        } else {
            (
                "maximize",
                "Maximize",
                "\u{e922}",
                WindowControlArea::Max,
                chrome.resizable,
            )
        },
        ("close", "Close", "\u{e8bb}", WindowControlArea::Close, true),
    ];
    // Windows' own close-button red, with white glyphs.
    let close_red: Rgba = rgb(0xC42B1C);
    div()
        .id("windows-caption-buttons")
        .flex()
        .flex_none()
        .self_stretch()
        .font_family(font)
        .children(
            buttons
                .into_iter()
                .map(move |(id, label, glyph, area, enabled)| {
                    let close = area == WindowControlArea::Close;
                    let (hover_bg, active_bg) = if close {
                        (close_red, close_red.opacity(0.9))
                    } else {
                        (
                            rgb(theme.hover_tint(colors.background)),
                            rgb(theme.active_tint(colors.background)),
                        )
                    };
                    div()
                        .id(id)
                        .debug_selector(move || format!("caption-{id}"))
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(WINDOWS_CAPTION_BUTTON_WIDTH))
                        .h_full()
                        .text_size(px(10.0))
                        .text_color(rgb(colors.foreground))
                        .occlude()
                        // The close button sits in the window's top-right corner.
                        .when(close, |this| this.rounded_tr(window_radius))
                        .when(!enabled, |this| this.opacity(0.45))
                        .when(enabled, |this| {
                            this.hover(move |style| {
                                let style = style.bg(hover_bg);
                                if close {
                                    style.text_color(gpui::white())
                                } else {
                                    style
                                }
                            })
                            .active(move |style| style.bg(active_bg))
                        })
                        .window_control_area(area)
                        // Windows handles the pointer through the control area;
                        // assistive technology presses the button through here.
                        .accessible(gpui::Role::Button, label)
                        .a11y_flags(A11yFlags::disabled(!enabled))
                        .when(enabled, |this| {
                            this.on_a11y_action(
                                gpui::AccessibleAction::Click,
                                move |_, window, _| match area {
                                    WindowControlArea::Min => window.minimize_window(),
                                    WindowControlArea::Max => window.zoom_window(),
                                    _ => window.remove_window(),
                                },
                            )
                        })
                        .child(glyph)
                }),
        )
}

#[cfg(test)]
mod tests {
    use gpui::{Context, IntoElement, Render, TestAppContext, VisualTestContext, Window, div};
    use zmail_core::ThemeAppearance;

    use super::*;
    use crate::components::test_support::init;

    const CONTROLS: WindowControls = WindowControls {
        fullscreen: true,
        maximize: true,
        minimize: true,
        window_menu: true,
    };

    fn chrome(decorations: Decorations) -> WindowChrome {
        WindowChrome {
            decorations,
            controls: CONTROLS,
            button_layout: None,
            maximized: false,
            fullscreen: false,
            resizable: true,
            minimizable: true,
        }
    }

    fn client() -> Decorations {
        Decorations::Client {
            tiling: Tiling::default(),
        }
    }

    #[test]
    fn linux_draws_controls_only_for_client_side_decorations() {
        let (left, right) = linux_controls(&chrome(Decorations::Server));
        assert!(left.is_empty() && right.is_empty());

        let (left, right) = linux_controls(&chrome(client()));
        assert!(left.is_empty());
        assert_eq!(
            right,
            [
                WindowButton::Minimize,
                WindowButton::Maximize,
                WindowButton::Close
            ]
        );
    }

    #[test]
    fn linux_controls_follow_the_desktop_layout_and_supported_controls() {
        let mut state = chrome(client());
        state.button_layout = Some(WindowButtonLayout {
            left: [Some(WindowButton::Close), None, None],
            right: [
                Some(WindowButton::Minimize),
                Some(WindowButton::Maximize),
                None,
            ],
        });
        state.controls.minimize = false;
        let (left, right) = linux_controls(&state);
        assert_eq!(left, [WindowButton::Close]);
        assert_eq!(right, [WindowButton::Maximize]);

        state.fullscreen = true;
        let (left, right) = linux_controls(&state);
        assert!(
            left.is_empty() && right.is_empty(),
            "full screen hides them"
        );
    }

    #[test]
    fn the_bar_is_the_title_bar_unless_the_window_manager_draws_one() {
        assert!(is_window_title_bar(
            Platform::MacOs,
            &chrome(Decorations::Server)
        ));
        assert!(is_window_title_bar(
            Platform::Windows,
            &chrome(Decorations::Server)
        ));
        assert!(!is_window_title_bar(
            Platform::Linux,
            &chrome(Decorations::Server)
        ));
        assert!(is_window_title_bar(Platform::Linux, &chrome(client())));
    }

    /// A window that renders a title bar for a given platform and state.
    struct Host {
        chrome: WindowChrome,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                TitleBar::new("title-bar").chrome(self.chrome).child(
                    div()
                        .debug_selector(|| "title-bar-content".into())
                        .w(px(100.0))
                        .h(px(20.0)),
                ),
            )
        }
    }

    fn render(
        platform: Platform,
        state: WindowChrome,
        cx: &mut TestAppContext,
    ) -> &mut VisualTestContext {
        init(cx);
        cx.update(|cx| {
            ComponentTheme::set(
                ComponentTheme::fallback(ThemeAppearance::Dark).with_platform(platform),
                cx,
            )
        });
        let (_, cx) = cx.add_window_view(|_, _| Host { chrome: state });
        cx.run_until_parked();
        cx
    }

    #[gpui::test]
    fn linux_client_side_decorations_draw_the_controls(cx: &mut TestAppContext) {
        let cx = render(Platform::Linux, chrome(client()), cx);
        for control in ["window-minimize", "window-maximize", "window-close"] {
            assert!(cx.debug_bounds(control).is_some(), "{control} is drawn");
        }
        let close = cx.debug_bounds("window-close").expect("close");
        let content = cx.debug_bounds("title-bar-content").expect("content");
        assert!(close.left() > content.right(), "controls trail the content");
    }

    #[gpui::test]
    fn linux_maximised_shows_restore(cx: &mut TestAppContext) {
        let mut state = chrome(client());
        state.maximized = true;
        let cx = render(Platform::Linux, state, cx);
        assert!(cx.debug_bounds("window-restore").is_some());
        assert!(cx.debug_bounds("window-maximize").is_none());
    }

    #[gpui::test]
    fn linux_server_side_decorations_draw_no_controls(cx: &mut TestAppContext) {
        let cx = render(Platform::Linux, chrome(Decorations::Server), cx);
        assert!(cx.debug_bounds("window-close").is_none());
        assert!(cx.debug_bounds("title-bar-content").is_some());
    }

    #[gpui::test]
    fn windows_draws_caption_buttons(cx: &mut TestAppContext) {
        let cx = render(Platform::Windows, chrome(Decorations::Server), cx);
        for control in ["caption-minimize", "caption-maximize", "caption-close"] {
            let bounds = cx
                .debug_bounds(control)
                .unwrap_or_else(|| panic!("{control}"));
            assert_eq!(bounds.size.width, px(WINDOWS_CAPTION_BUTTON_WIDTH));
        }
        assert!(cx.debug_bounds("window-close").is_none());
    }

    #[gpui::test]
    fn macos_leaves_room_for_the_traffic_lights(cx: &mut TestAppContext) {
        let cx = render(Platform::MacOs, chrome(Decorations::Server), cx);
        let content = cx.debug_bounds("title-bar-content").expect("content");
        assert_eq!(content.left(), px(TRAFFIC_LIGHT_PADDING));
        assert!(cx.debug_bounds("caption-close").is_none());
        assert!(cx.debug_bounds("window-close").is_none());
    }
}
