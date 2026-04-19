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
    AnyElement, App, ClickEvent, FontWeight, IntoElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Svg, Window, div, prelude::*, px, rgb, svg,
};
use zmail_core::SyncPhase;

use crate::input::UiBuffer;
use crate::styles::{
    ACTION_DIVIDER_HEIGHT, ACTION_DIVIDER_WIDTH, BADGE_SIZE, EMPTY_STATE_ICON_FRAME_SIZE,
    ICON_SIZE_LG, ICON_SIZE_SM, INPUT_HEIGHT_MULTILINE, INPUT_HEIGHT_SINGLE,
    STATUS_BANNER_STRIP_WIDTH, TOOLBAR_BUTTON_HEIGHT, UiColors,
};

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
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_owned()),
                )
                .child(
                    div()
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
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .w_full()
                .child(div().truncate().child(title.to_owned()))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .child(trailing.to_owned()),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(colors.muted))
                .truncate()
                .child(subtitle.to_owned()),
        )
        .child(div().text_sm().truncate().child(preview.to_owned()))
        .child(
            div()
                .text_xs()
                .text_color(rgb(colors.muted))
                .child(meta.to_owned()),
        )
}

// ---------------------------------------------------------------------------
// input_card
// ---------------------------------------------------------------------------

/// A labelled text-input card backed by a [`UiBuffer`].
///
/// The card border highlights in the accent colour when `active` is `true`.
/// `multiline` controls the minimum height of the content area.
pub(crate) fn input_card(
    id: &'static str,
    label: &str,
    placeholder: &str,
    buffer: &UiBuffer,
    active: bool,
    multiline: bool,
    colors: &UiColors,
) -> Stateful<gpui::Div> {
    let card_border = if active { colors.accent } else { colors.border };
    let card_background = if active {
        colors.background
    } else {
        colors.surface
    };
    let min_height = if multiline {
        px(INPUT_HEIGHT_MULTILINE)
    } else {
        px(INPUT_HEIGHT_SINGLE)
    };
    let is_placeholder = !active && buffer.text.is_empty();
    let display_text = if is_placeholder {
        placeholder.to_owned()
    } else {
        buffer.display_text(active)
    };
    let text_color = if is_placeholder {
        colors.muted
    } else {
        colors.foreground
    };

    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(colors.muted))
                .child(label.to_owned()),
        )
        .child(
            div()
                .min_h(min_height)
                .w_full()
                .px_3()
                .py_3()
                .rounded_md()
                .bg(rgb(card_background))
                .border_1()
                .border_color(rgb(card_border))
                .child(div().text_color(rgb(text_color)).child(display_text)),
        )
}

// ---------------------------------------------------------------------------
// section_card
// ---------------------------------------------------------------------------

/// Render a rounded content card with a title and muted supporting text.
pub(crate) fn section_card(title: &str, detail: &str, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .rounded_lg()
        .bg(rgb(colors.surface))
        .border_1()
        .border_color(rgb(colors.border))
        .px_4()
        .py_4()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .child(detail.to_owned()),
                ),
        )
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
        .size(px(size))
        .text_color(rgb(color))
}

// ---------------------------------------------------------------------------
// badge
// ---------------------------------------------------------------------------

/// Render a compact unread-count pill.
pub(crate) fn badge(count: usize, bg_color: u32, text_color: u32) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(BADGE_SIZE))
        .h(px(BADGE_SIZE))
        .px_2()
        .rounded_full()
        .bg(rgb(bg_color))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(text_color))
        .child(count.to_string())
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
