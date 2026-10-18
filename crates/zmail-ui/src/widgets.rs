//! Reusable widget-level building blocks for the Zmail UI shell.
//!
//! All stateless helper functions that produce GPUI elements live here so that
//! higher-level render modules can share them without duplicating code or
//! importing directly from `lib.rs`.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use gpui::{
    AnyElement, App, ClickEvent, Entity, FontWeight, InteractiveElement, IntoElement, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Svg, Window, div, prelude::*, px, rgb, svg,
};
use zmail_core::SyncPhase;

use crate::components::TextInput;
use crate::styles::{
    ACCOUNT_COLORS, ACCOUNT_DOT_SIZE, ACTION_DIVIDER_HEIGHT, ACTION_DIVIDER_WIDTH,
    EMPTY_STATE_ICON_FRAME_SIZE, ICON_SIZE_LG, ICON_SIZE_SM, STATUS_BANNER_STRIP_WIDTH,
    TOOLBAR_BUTTON_HEIGHT, UiColors,
};

// ---------------------------------------------------------------------------
// EllipsisExt
// ---------------------------------------------------------------------------

/// Single-line text that ends in an ellipsis when it does not fit.
///
/// GPUI 0.2.2's `truncate()` (`whitespace_nowrap` + `text_ellipsis`) caches
/// the first measured size of no-wrap text, which taffy takes before the
/// final width is known, so the text is clipped mid-glyph instead of
/// ending in "…". Clamping wrapping text to one line keys the cache on the
/// wrap width, so the ellipsis is computed for the real width.
pub(crate) trait EllipsisExt: Styled + Sized {
    /// Clamp to one line and end overflowing text with an ellipsis.
    fn ellipsis(self) -> Self {
        self.line_clamp(1).text_ellipsis()
    }
}

impl<T: Styled> EllipsisExt for T {}

// ---------------------------------------------------------------------------
// Hover and selection backgrounds
// ---------------------------------------------------------------------------

/// The resting background of a selectable surface: the selection colour
/// when selected, otherwise `base`.
pub(crate) fn interactive_background(selected: bool, base: u32, colors: &UiColors) -> u32 {
    if selected {
        colors.selected_background
    } else {
        base
    }
}

/// Hover feedback as a GPUI style refinement, so hovering repaints only
/// the hovered element instead of notifying the whole workspace.
pub(crate) trait HoverExt: InteractiveElement + Sized {
    /// Show the hover background unless the element is selected.
    fn hover_bg(self, selected: bool, colors: &UiColors) -> Self {
        if selected {
            self
        } else {
            let hover = colors.hover_background;
            self.hover(move |style| style.bg(rgb(hover)))
        }
    }
}

impl<T: InteractiveElement> HoverExt for T {}

// ---------------------------------------------------------------------------
// toolbar_button
// ---------------------------------------------------------------------------

/// Build a stable element id for a toolbar-style button from its label.
pub(crate) fn toolbar_button_id(label: &str) -> SharedString {
    let mut id = String::from("toolbar-button-");
    let mut last_was_dash = false;

    for ch in label.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            id.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            id.push('-');
            last_was_dash = true;
        }
    }

    while id.ends_with('-') {
        id.pop();
    }

    if id == "toolbar-button" {
        id.push_str("-action");
    }

    SharedString::from(id)
}

/// A small pill-shaped clickable button used in the toolbar and action bars.
pub(crate) fn toolbar_button(
    label: impl Into<String>,
    icon_name: Option<&'static str>,
    background: u32,
    border: u32,
    foreground: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    let label = label.into();
    let button_id = toolbar_button_id(&label);
    div()
        .id(button_id)
        .flex()
        .flex_none()
        .whitespace_nowrap()
        .items_center()
        .gap_2()
        .min_h(px(TOOLBAR_BUTTON_HEIGHT))
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(background))
        .border_1()
        .border_color(rgb(border))
        .text_color(rgb(foreground))
        .cursor_pointer()
        // Toolbar buttons sit in the title bar: a press must reach the
        // button, not start a window drag (see `components::title_bar`).
        .occlude()
        .on_click(on_click)
        .when_some(icon_name, |this, name| {
            this.child(icon(name, ICON_SIZE_SM, foreground))
        })
        .child(div().child(label))
}

// ---------------------------------------------------------------------------
// empty_state
// ---------------------------------------------------------------------------

/// A centred placeholder card shown when a list or pane has no content.
pub(crate) fn empty_state(
    title: &str,
    detail: &str,
    icon_name: &'static str,
    action: Option<AnyElement>,
    colors: &UiColors,
) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px_6()
        .text_center()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                // Full width so the title and detail wrap inside the pane.
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(EMPTY_STATE_ICON_FRAME_SIZE))
                        .h(px(EMPTY_STATE_ICON_FRAME_SIZE))
                        .rounded_full()
                        .bg(rgb(colors.button_background))
                        .child(icon(icon_name, ICON_SIZE_LG * 2.0, colors.muted)),
                )
                .child(
                    div()
                        .w_full()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .child(detail.to_owned()),
                )
                .when_some(action, |this, action| this.child(action)),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// list_row
// ---------------------------------------------------------------------------

/// A multi-line list item laid out as:
///
/// ```text
/// [title]                      [trailing]
/// [subtitle (muted, truncated)]
/// [preview (truncated)]
/// [meta (muted)]
/// ```
///
/// Empty `preview` and `meta` lines are omitted.
pub(crate) fn list_row(
    title: &str,
    trailing: &str,
    subtitle: &str,
    preview: &str,
    meta: &str,
    colors: &UiColors,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0()
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap_2()
                .w_full()
                .child(div().flex_1().min_w_0().ellipsis().child(title.to_owned()))
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .child(trailing.to_owned()),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(colors.muted))
                .ellipsis()
                .child(subtitle.to_owned()),
        )
        .when(!preview.is_empty(), |this| {
            this.child(div().text_sm().ellipsis().child(preview.to_owned()))
        })
        .when(!meta.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(rgb(colors.muted))
                    .child(meta.to_owned()),
            )
        })
}

// ---------------------------------------------------------------------------
// input_card
// ---------------------------------------------------------------------------

/// A labelled [`TextInput`]. The input draws its own field, focus ring and
/// placeholder; callers can add children (such as a validation error)
/// below it.
pub(crate) fn input_card(label: &str, input: &Entity<TextInput>, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(colors.muted))
                .child(label.to_owned()),
        )
        .child(input.clone())
}

// ---------------------------------------------------------------------------
// section_card
// ---------------------------------------------------------------------------

/// Render a rounded content card with a section title.
pub(crate) fn section_card(title: &str, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .rounded_lg()
        .bg(rgb(colors.surface))
        .border_1()
        .border_color(rgb(colors.border))
        .px_4()
        .py_4()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
}

// ---------------------------------------------------------------------------
// format_size
// ---------------------------------------------------------------------------

/// Formats a byte count with one fractional digit and a binary unit.
pub(crate) fn format_size(size_bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size_bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size_bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// icon
// ---------------------------------------------------------------------------

/// Render a Lucide icon SVG at the given pixel size and ARGB colour.
///
/// `name` must match a filename in `assets/icons/` without the `.svg`
/// extension — e.g. `"mail"`, `"inbox"`, `"refresh-cw"`.
pub(crate) fn icon(name: &str, size: f32, color: u32) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .flex_none()
        .size(px(size))
        .text_color(rgb(color))
}

// ---------------------------------------------------------------------------
// label_chip
// ---------------------------------------------------------------------------

/// Render a compact metadata chip with optional leading icon.
pub(crate) fn label_chip(
    label: impl Into<String>,
    icon_name: Option<&'static str>,
    bg_color: u32,
    text_color: u32,
) -> gpui::Div {
    let label = label.into();
    div()
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_full()
        .bg(rgb(bg_color))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(text_color))
        .when_some(icon_name, |this, name| {
            this.child(icon(name, ICON_SIZE_SM, text_color))
        })
        .child(label)
}

// ---------------------------------------------------------------------------
// avatar
// ---------------------------------------------------------------------------

/// Render a circular avatar from the first alphanumeric character in a name.
pub(crate) fn avatar(name: &str, size: f32, bg_color: u32, colors: &UiColors) -> gpui::Div {
    let initial = name
        .chars()
        .find_map(|ch| {
            ch.is_ascii_alphanumeric()
                .then_some(ch.to_ascii_uppercase())
        })
        .unwrap_or('?');

    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(size))
        .h(px(size))
        .rounded_full()
        .bg(rgb(bg_color))
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(contrasting_text_color(bg_color, colors)))
        .child(initial.to_string())
}

// ---------------------------------------------------------------------------
// action_divider
// ---------------------------------------------------------------------------

/// Render a compact vertical divider used inside action bars.
pub(crate) fn action_divider(colors: &UiColors) -> gpui::Div {
    div()
        .w(px(ACTION_DIVIDER_WIDTH))
        .h(px(ACTION_DIVIDER_HEIGHT))
        .bg(rgb(colors.border))
}

// ---------------------------------------------------------------------------
// status_banner
// ---------------------------------------------------------------------------

/// Render a compact status or error banner with a leading colour strip.
pub(crate) fn status_banner(
    message: &str,
    icon_name: &'static str,
    accent_color: u32,
    background_color: u32,
    colors: &UiColors,
) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .bg(rgb(background_color))
        .border_t_1()
        .border_color(rgb(colors.border))
        .child(div().w(px(STATUS_BANNER_STRIP_WIDTH)).bg(rgb(accent_color)))
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .child(icon(icon_name, ICON_SIZE_SM, accent_color))
                .child(div().flex_1().text_sm().child(message.to_owned())),
        )
}

// ---------------------------------------------------------------------------
// message_avatar_background
// ---------------------------------------------------------------------------

/// Choose a stable accent colour for avatars derived from sender/account text.
pub(crate) fn message_avatar_background(seed: &str, colors: &UiColors) -> u32 {
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    let palette = [colors.accent, colors.info, colors.success, colors.warning];
    let index = (hasher.finish() as usize) % palette.len();
    palette[index]
}

// ---------------------------------------------------------------------------
// account_color
// ---------------------------------------------------------------------------

/// An account's colour: derived from its id with no setting, so the first
/// accounts all differ and an account keeps its colour across launches.
pub(crate) fn account_color(account_id: i64) -> u32 {
    let index = account_id.rem_euclid(ACCOUNT_COLORS.len() as i64) as usize;
    // Ids start at 1; the first account gets the first colour.
    ACCOUNT_COLORS[(index + ACCOUNT_COLORS.len() - 1) % ACCOUNT_COLORS.len()]
}

/// A small dot in an account's colour.
pub(crate) fn account_dot(account_id: i64) -> gpui::Div {
    div()
        .flex_none()
        .size(px(ACCOUNT_DOT_SIZE))
        .rounded_full()
        .bg(rgb(account_color(account_id)))
}

// ---------------------------------------------------------------------------
// sync_badge
// ---------------------------------------------------------------------------

/// Map a [`SyncPhase`] to a display colour and short label for the sidebar
/// account badge.
pub(crate) fn sync_badge(phase: SyncPhase, colors: &UiColors) -> (&'static str, u32, &'static str) {
    match phase {
        SyncPhase::Idle => ("check-circle", colors.success, SyncPhase::Idle.label()),
        SyncPhase::Connecting => ("loader", colors.info, SyncPhase::Connecting.label()),
        SyncPhase::Discovering => ("loader", colors.info, SyncPhase::Discovering.label()),
        SyncPhase::Syncing => ("loader", colors.info, SyncPhase::Syncing.label()),
        SyncPhase::Backoff => ("clock", colors.warning, SyncPhase::Backoff.label()),
        SyncPhase::Failed => ("alert-circle", colors.danger, SyncPhase::Failed.label()),
    }
}

// ---------------------------------------------------------------------------
// short_sha
// ---------------------------------------------------------------------------

/// Truncate a git SHA string to the first 8 characters for compact display.
pub(crate) fn short_sha(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn contrasting_text_color(color: u32, colors: &UiColors) -> u32 {
    let color_luminance = luminance(color);
    let foreground_delta = color_luminance.abs_diff(luminance(colors.foreground));
    let background_delta = color_luminance.abs_diff(luminance(colors.background));

    if foreground_delta >= background_delta {
        colors.foreground
    } else {
        colors.background
    }
}

fn luminance(color: u32) -> u32 {
    let red = (color >> 16) & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = color & 0xff;
    (red * 299) + (green * 587) + (blue * 114)
}
