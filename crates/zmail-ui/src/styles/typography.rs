//! The type scale.
//!
//! Sizes and line heights are in `rem`, so the whole UI scales with the
//! window's rem size (`Window::set_rem_size`, 16px by default). That is
//! the single knob for honouring the operating system's text-size setting.
//!
//! Apply a style with [`TypeStyleExt::type_style`]:
//!
//! ```text
//! div().type_style(TYPE_BODY_STRONG).child("Inbox")
//! ```

use gpui::{FontWeight, Styled, rems};

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
}
