//! The type scale.
//!
//! Sizes and line heights are in `rem`, so the whole UI scales with the
//! window's rem size (`Window::set_rem_size`, 16px by default). That is
//! the single knob for the text size (Settings → Appearance, see
//! [`TextScale`]): every window frame sets its rem size from it.
//!
//! Anything that has to grow with its text (a row's height, a control's
//! height, a dialog's width) is sized with [`rems_from_px`], which takes
//! the size in pixels at the default text size. Spacing, borders and
//! icons stay in pixels.
//!
//! Apply a style with [`TypeStyleExt::type_style`]:
//!
//! ```text
//! div().type_style(TYPE_BODY_STRONG).child("Inbox")
//! ```
//!
//! The family is not part of a [`TypeStyle`]: each window root sets
//! [`PlatformStyle::ui_font`](super::PlatformStyle) once and every element
//! (popovers and menus included) inherits it. That is the system UI font on
//! macOS and Windows and the bundled Inter on Linux (see `crate::fonts`).

use gpui::{App, FontWeight, Global, Pixels, Rems, Styled, px, rems};

/// The rem size at the default text size (GPUI's default).
pub const BASE_REM_PX: f32 = 16.0;

/// A size designed in pixels at the default text size, in rems, so that it
/// grows with the text size.
pub fn rems_from_px(pixels: f32) -> Rems {
    rems(pixels / BASE_REM_PX)
}

/// The text size as a factor of the default (1.0 to 2.0). The view that
/// owns the main window installs it from the saved preferences, and
/// `components::window_frame` sets every window's rem size from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextScale(pub f32);

impl Global for TextScale {}

impl TextScale {
    /// The scale for a text size in percent.
    pub fn from_percent(percent: u16) -> Self {
        Self(f32::from(percent) / 100.0)
    }

    /// The active scale; 1.0 until one is installed.
    pub fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or(Self(1.0))
    }

    /// The rem size for this scale.
    pub fn rem_size(self) -> Pixels {
        px(BASE_REM_PX * self.0)
    }

    /// `pixels` at the default text size, at this scale.
    pub fn scale(self, pixels: f32) -> f32 {
        pixels * self.0
    }
}

/// One step of the type scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeStyle {
    /// Font size in rem.
    pub size_rem: f32,
    /// Line height in rem.
    pub line_height_rem: f32,
    /// Font weight.
    pub weight: FontWeight,
}

impl TypeStyle {
    /// Font size in pixels at a given rem size.
    pub fn size_px(&self, rem_px: f32) -> f32 {
        self.size_rem * rem_px
    }

    /// Line height in pixels at a given rem size.
    pub fn line_height_px(&self, rem_px: f32) -> f32 {
        self.line_height_rem * rem_px
    }
}

/// Timestamps, counts, hints and other small print: 12/16px.
pub const TYPE_CAPTION: TypeStyle = TypeStyle {
    size_rem: 0.75,
    line_height_rem: 1.0,
    weight: FontWeight::NORMAL,
};

/// Default UI and list text: 14/20px.
pub const TYPE_BODY: TypeStyle = TypeStyle {
    size_rem: 0.875,
    line_height_rem: 1.25,
    weight: FontWeight::NORMAL,
};

/// Emphasised body text, such as unread senders and labels: 14/20px
/// semibold.
pub const TYPE_BODY_STRONG: TypeStyle = TypeStyle {
    size_rem: 0.875,
    line_height_rem: 1.25,
    weight: FontWeight::SEMIBOLD,
};

/// Pane and section titles: 17/24px semibold.
pub const TYPE_TITLE: TypeStyle = TypeStyle {
    size_rem: 1.0625,
    line_height_rem: 1.5,
    weight: FontWeight::SEMIBOLD,
};

/// The reader's subject line and empty-state headings: 22/28px semibold.
pub const TYPE_HEADLINE: TypeStyle = TypeStyle {
    size_rem: 1.375,
    line_height_rem: 1.75,
    weight: FontWeight::SEMIBOLD,
};

/// The whole scale, smallest first.
pub const TYPE_SCALE: [TypeStyle; 5] = [
    TYPE_CAPTION,
    TYPE_BODY,
    TYPE_BODY_STRONG,
    TYPE_TITLE,
    TYPE_HEADLINE,
];

/// Applies a [`TypeStyle`] to any styled element.
pub trait TypeStyleExt: Styled + Sized {
    /// Set the font size, line height and weight from `style`.
    fn type_style(self, style: TypeStyle) -> Self {
        self.text_size(rems(style.size_rem))
            .line_height(rems(style.line_height_rem))
            .font_weight(style.weight)
    }
}

impl<T: Styled> TypeStyleExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_grows_and_line_heights_hold_their_text() {
        for pair in TYPE_SCALE.windows(2) {
            assert!(pair[0].size_rem <= pair[1].size_rem);
        }
        for style in TYPE_SCALE {
            // 1.2x is the tightest line height that does not clip glyphs.
            assert!(style.line_height_rem >= style.size_rem * 1.2, "{style:?}");
            // Line heights land on the 4px grid at the default rem size.
            assert_eq!(style.line_height_px(16.0) % 4.0, 0.0, "{style:?}");
        }
    }

    #[test]
    fn sizes_scale_with_the_rem_size() {
        assert_eq!(TYPE_BODY.size_px(16.0), 14.0);
        assert_eq!(TYPE_BODY.size_px(20.0), 17.5);
        assert_eq!(TYPE_CAPTION.line_height_px(16.0), 16.0);
    }

    #[test]
    fn text_scales_set_the_rem_size() {
        assert_eq!(TextScale::from_percent(100).rem_size(), px(16.0));
        assert_eq!(TextScale::from_percent(150).rem_size(), px(24.0));
        assert_eq!(TextScale::from_percent(200).rem_size(), px(32.0));
        assert_eq!(rems_from_px(24.0), rems(1.5));
    }
}
