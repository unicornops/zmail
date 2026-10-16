//! `Button` and `IconButton`.
//!
//! Both are keyboard operable: they are Tab stops unless disabled, Enter or
//! Space activates them (GPUI turns those into a click on the focused
//! element), and keyboard focus draws the shared 2px focus ring. Disabled
//! buttons are dimmed, skipped by Tab and ignore clicks.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, FocusHandle, FontWeight, IntoElement, RenderOnce, Role,
    SharedString, Window, div, prelude::*, px, rgb,
};

use crate::styles::{ICON_SIZE_MD, ICON_SIZE_SM, TYPE_BODY, TYPE_CAPTION, TypeStyleExt};
use crate::widgets::icon;

use super::theme::{ComponentTheme, FocusRingExt};
use super::tooltip::TooltipExt;

/// A click handler shared by the button components.
pub(crate) type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// The visual weight of a button.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonVariant {
    /// The main action of a view: accent fill.
    Primary,
    /// A secondary action: neutral fill.
    #[default]
    Subtle,
    /// A low-emphasis action: no fill until hovered.
    Ghost,
    /// A destructive action: danger fill.
    Danger,
}

/// The height and padding of a button.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonSize {
    /// 24px tall: dense rows and inline actions.
    Small,
    /// 32px tall: toolbars and forms.
    #[default]
    Medium,
    /// 40px tall: prominent calls to action.
    Large,
}

impl ButtonSize {
    /// The button height in pixels. Medium follows the platform's control
    /// height (`PlatformStyle::control_height`).
    pub(crate) fn height(self, theme: &ComponentTheme) -> f32 {
        match self {
            Self::Small => 24.0,
            Self::Medium => theme.platform.control_height,
            Self::Large => 40.0,
        }
    }

    const fn padding_x(self) -> f32 {
        match self {
            Self::Small => 6.0,
            Self::Medium => 10.0,
            Self::Large => 14.0,
        }
    }
}

/// Resolved colours for one button state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ButtonColors {
    /// Resting fill (also the colour under the focus ring).
    pub fill: u32,
    /// Label and icon colour.
    pub text: u32,
    /// Fill while hovered.
    pub hover: u32,
    /// Fill while pressed.
    pub active: u32,
    /// Boundary colour, for variants that draw one.
    pub border: Option<u32>,
    /// Whether the resting fill is left transparent (ghost buttons show
    /// whatever is behind them until hovered).
    pub transparent: bool,
}

impl ButtonVariant {
    /// The colours this variant uses with `theme`. `selected` marks a
    /// toggled-on button.
    pub(crate) fn colors(self, theme: &ComponentTheme, selected: bool) -> ButtonColors {
        let colors = &theme.colors;
        let fill = if selected {
            colors.selected_background
        } else {
            match self {
                Self::Primary => colors.accent,
                Self::Subtle => colors.button_background,
                // Ghost buttons sit on whatever is behind them; tint from
                // the app background.
                Self::Ghost => colors.background,
                Self::Danger => colors.danger,
            }
        };
        ButtonColors {
            fill,
            text: theme.text_on(fill),
            hover: theme.hover_tint(fill),
            active: theme.active_tint(fill),
            // Neutral fills are barely distinct from the pane, so they
            // carry a control border that reaches 3:1 (WCAG 1.4.11).
            border: (self == Self::Subtle && !selected).then_some(colors.control_border),
            transparent: self == Self::Ghost && !selected,
        }
    }
}

/// A text button with an optional leading icon.
#[derive(IntoElement)]
pub(crate) struct Button {
    id: ElementId,
    label: SharedString,
    icon: Option<&'static str>,
    variant: ButtonVariant,
    size: ButtonSize,
    disabled: bool,
    selected: bool,
    full_width: bool,
    tooltip: Option<SharedString>,
    shortcut: Option<SharedString>,
    focus_handle: Option<FocusHandle>,
    tab_index: isize,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// A subtle, medium button labelled `label`.
    pub(crate) fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            disabled: false,
            selected: false,
            full_width: false,
            tooltip: None,
            shortcut: None,
            focus_handle: None,
            tab_index: 0,
            on_click: None,
        }
    }

    /// Show the Lucide icon `name` before the label.
    pub(crate) fn icon(mut self, name: &'static str) -> Self {
        self.icon = Some(name);
        self
    }

    /// Set the variant.
    pub(crate) fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Use the [`ButtonVariant::Primary`] style.
    pub(crate) fn primary(self) -> Self {
        self.variant(ButtonVariant::Primary)
    }

    /// Use the [`ButtonVariant::Ghost`] style.
    pub(crate) fn ghost(self) -> Self {
        self.variant(ButtonVariant::Ghost)
    }

    /// Use the [`ButtonVariant::Danger`] style.
    pub(crate) fn danger(self) -> Self {
        self.variant(ButtonVariant::Danger)
    }

    /// Set the size.
    pub(crate) fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Disable the button: dimmed, not focusable, and clicks are ignored.
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Show the button as toggled on.
    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Stretch to the width of the parent.
    pub(crate) fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Show a tooltip after a hover delay.
    pub(crate) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// The keyboard shortcut for the same command (GPUI binding syntax).
    /// It appears in the tooltip and is announced to screen readers.
    pub(crate) fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Track focus with a handle the caller owns, so it can focus the
    /// button programmatically.
    pub(crate) fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }

    /// Run `handler` when the button is clicked or activated by keyboard.
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = self.variant.colors(&theme, self.selected);
        let label = self.label.clone();
        let tooltip = self.tooltip.clone().or_else(|| {
            // A shortcut needs somewhere to be shown.
            self.shortcut.as_ref().map(|_| self.label.clone())
        });
        button_base(
            self.id,
            &theme,
            colors,
            ButtonBase {
                disabled: self.disabled,
                selected: self.selected,
                focus_handle: self.focus_handle,
                tab_index: self.tab_index,
                on_click: self.on_click,
                tooltip,
                shortcut: self.shortcut,
                label: label.clone(),
            },
        )
        .h(px(self.size.height(&theme)))
        .px(px(self.size.padding_x()))
        .gap(px(6.0))
        .when(self.full_width, |this| this.w_full())
        .when(self.size == ButtonSize::Small, |this| {
            this.type_style(TYPE_CAPTION)
        })
        .when(self.size != ButtonSize::Small, |this| {
            this.type_style(TYPE_BODY)
        })
        .font_weight(FontWeight::MEDIUM)
        .when_some(self.icon, |this, name| {
            this.child(icon(name, ICON_SIZE_SM, colors.text))
        })
        .child(label)
    }
}

/// A square button that shows only an icon. The accessible label is
/// required: it is the tooltip and the screen-reader name.
#[derive(IntoElement)]
pub(crate) struct IconButton {
    id: ElementId,
    icon: &'static str,
    label: SharedString,
    variant: ButtonVariant,
    size: ButtonSize,
    disabled: bool,
    selected: bool,
    shortcut: Option<SharedString>,
    focus_handle: Option<FocusHandle>,
    tab_index: isize,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    /// A ghost icon button showing `icon`, named `label`.
    pub(crate) fn new(
        id: impl Into<ElementId>,
        icon: &'static str,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
            variant: ButtonVariant::Ghost,
            size: ButtonSize::default(),
            disabled: false,
            selected: false,
            shortcut: None,
            focus_handle: None,
            tab_index: 0,
            on_click: None,
        }
    }

    /// Set the variant (default [`ButtonVariant::Ghost`]).
    pub(crate) fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Set the size; the button is as wide as it is tall.
    pub(crate) fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Disable the button.
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Show the button as toggled on.
    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// The keyboard shortcut for the same command, shown in the tooltip.
    pub(crate) fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Run `handler` when the button is clicked or activated by keyboard.
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = self.variant.colors(&theme, self.selected);
        let size = self.size.height(&theme);
        let icon_size = if self.size == ButtonSize::Large {
            ICON_SIZE_MD
        } else {
            ICON_SIZE_SM
        };
        button_base(
            self.id,
            &theme,
            colors,
            ButtonBase {
                disabled: self.disabled,
                selected: self.selected,
                focus_handle: self.focus_handle,
                tab_index: self.tab_index,
                on_click: self.on_click,
                tooltip: Some(self.label.clone()),
                shortcut: self.shortcut,
                label: self.label,
            },
        )
        .size(px(size))
        .child(icon(self.icon, icon_size, colors.text))
    }
}

/// Behaviour shared by [`Button`] and [`IconButton`].
struct ButtonBase {
    disabled: bool,
    selected: bool,
    focus_handle: Option<FocusHandle>,
    tab_index: isize,
    on_click: Option<ClickHandler>,
    tooltip: Option<SharedString>,
    shortcut: Option<SharedString>,
    label: SharedString,
}

fn button_base(
    id: ElementId,
    theme: &ComponentTheme,
    colors: ButtonColors,
    base: ButtonBase,
) -> gpui::Stateful<gpui::Div> {
    let ButtonBase {
        disabled,
        selected,
        focus_handle,
        tab_index,
        on_click,
        tooltip,
        shortcut,
        label,
    } = base;
    let (hover, active) = (colors.hover, colors.active);

    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .whitespace_nowrap()
        .rounded(px(theme.platform.control_radius))
        .when(!colors.transparent, |this| this.bg(rgb(colors.fill)))
        .text_color(rgb(colors.text))
        .when_some(colors.border, |this, border| {
            this.border_1().border_color(rgb(border))
        })
        .focus_ring(theme)
        .role(Role::Button)
        .aria_label(label)
        .when(selected, |this| this.aria_toggled(gpui::Toggled::True))
        .when_some(shortcut.clone(), |this, shortcut| {
            this.aria_keyshortcuts(shortcut)
        })
        // A tracked handle carries its own tab settings (GPUI ignores the
        // element's `tab_index` for it), so set them on the handle.
        .when_some(focus_handle, |this, handle| {
            this.track_focus(&handle.tab_index(tab_index).tab_stop(!disabled))
        })
        .map(|this| {
            if disabled {
                this.opacity(0.45).cursor_default()
            } else {
                this.tab_index(tab_index)
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(hover)))
                    .active(move |style| style.bg(rgb(active)))
                    .when_some(on_click, |this, handler| {
                        this.on_click(move |event, window, cx| handler(event, window, cx))
                    })
            }
        })
        .when_some(tooltip, |this, tooltip| {
            this.with_tooltip(tooltip, shortcut)
        })
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{Context, FocusHandle, Render, TestAppContext, VisualTestContext};
    use zmail_core::ThemeAppearance;

    use super::*;
    use crate::components::test_support::press;
    use crate::styles::contrast::{MIN_TEXT_CONTRAST, contrast_ratio};

    #[test]
    fn primary_and_danger_labels_are_readable() {
        for appearance in [ThemeAppearance::Light, ThemeAppearance::Dark] {
            let theme = ComponentTheme::fallback(appearance);
            for variant in [
                ButtonVariant::Primary,
                ButtonVariant::Subtle,
                ButtonVariant::Ghost,
                ButtonVariant::Danger,
            ] {
                let colors = variant.colors(&theme, false);
                let ratio = contrast_ratio(colors.text, colors.fill);
                assert!(
                    ratio >= MIN_TEXT_CONTRAST,
                    "{appearance:?} {variant:?}: {ratio:.2}:1 is too low"
                );
            }
        }
    }

    struct Buttons {
        enabled: FocusHandle,
        clicks: Rc<Cell<usize>>,
        disabled_clicks: Rc<Cell<usize>>,
    }

    impl Render for Buttons {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.clicks.clone();
            let disabled_clicks = self.disabled_clicks.clone();
            div()
                .child(
                    Button::new("enabled", "Save")
                        .primary()
                        .track_focus(&self.enabled)
                        .on_click(move |_, _, _| clicks.set(clicks.get() + 1)),
                )
                .child(
                    Button::new("disabled", "Delete")
                        .danger()
                        .disabled(true)
                        .on_click(move |_, _, _| disabled_clicks.set(disabled_clicks.get() + 1)),
                )
        }
    }

    #[gpui::test]
    fn enter_and_space_activate_a_focused_button(cx: &mut TestAppContext) {
        let clicks = Rc::new(Cell::new(0));
        let disabled_clicks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view(|_, cx| Buttons {
            enabled: cx.focus_handle(),
            clicks: clicks.clone(),
            disabled_clicks: disabled_clicks.clone(),
        });
        focus(&view, cx);
        press(cx, "enter space");
        assert_eq!(clicks.get(), 2);
        assert_eq!(disabled_clicks.get(), 0);
    }

    fn focus(view: &gpui::Entity<Buttons>, cx: &mut VisualTestContext) {
        let handle = view.read_with(cx, |view, _| view.enabled.clone());
        cx.update(|window, cx| window.focus(&handle, cx));
        cx.run_until_parked();
    }
}
