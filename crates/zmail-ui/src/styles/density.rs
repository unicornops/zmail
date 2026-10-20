//! Density tokens: row heights and paddings for the comfortable and
//! compact layouts (UI plan §2, "Message rows").
//!
//! Comfortable matches the Phase 0 layout constants exactly, so views can
//! move to [`DensityTokens`] without changing a pixel.
//!
//! The constants are for the default text size. [`DensityTokens::scaled`]
//! grows everything that holds text (lines, rows, the avatar's initials)
//! with the text size, keeping paddings and gaps as they are.

use zmail_core::preferences::Density;

use super::{
    AVATAR_SIZE, BADGE_SIZE, LIST_ITEM_HEIGHT, MAILBOX_ITEM_HEIGHT, ROW_BORDER_WIDTH, ROW_LINE_GAP,
    ROW_PADDING_Y, ROW_SENDER_LINE_HEIGHT, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TYPE_BODY,
};

/// Row geometry for one density, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DensityTokens {
    /// Which density these tokens are for.
    pub density: Density,
    /// Total height of a message-list row, including its divider.
    pub message_row_height: f32,
    /// Lines of text in a message-list row (3 comfortable, 1 compact).
    pub message_row_lines: u8,
    /// Line height of each line in a message-list row.
    pub message_row_line_height: f32,
    /// Vertical gap between the lines of a message-list row.
    pub message_row_line_gap: f32,
    /// Top and bottom padding of a message-list row.
    pub message_row_padding_y: f32,
    /// Leading and trailing padding of a message-list row.
    pub message_row_padding_x: f32,
    /// Whether message rows show a sender avatar.
    pub show_avatar: bool,
    /// Diameter of the avatar when shown.
    pub avatar_size: f32,
    /// Height of a sidebar row (mailbox, favourite or account).
    pub sidebar_row_height: f32,
    /// Leading and trailing padding of a sidebar row.
    pub sidebar_row_padding_x: f32,
    /// Gap between the icons, text and badges inside any row.
    pub row_gap: f32,
    /// The text size these tokens are for, as a factor of the default.
    pub text_scale: f32,
}

impl DensityTokens {
    /// Three-line rows with avatars: the Phase 0 layout.
    pub const COMFORTABLE: Self = Self {
        density: Density::Comfortable,
        message_row_height: LIST_ITEM_HEIGHT,
        message_row_lines: 3,
        // The tallest of the three Phase 0 lines; the row height is still
        // derived from the per-line constants.
        message_row_line_height: ROW_SENDER_LINE_HEIGHT,
        message_row_line_gap: ROW_LINE_GAP,
        message_row_padding_y: ROW_PADDING_Y,
        message_row_padding_x: SPACE_4,
        show_avatar: true,
        avatar_size: AVATAR_SIZE,
        sidebar_row_height: MAILBOX_ITEM_HEIGHT,
        sidebar_row_padding_x: SPACE_3,
        row_gap: SPACE_3,
        text_scale: 1.0,
    };

    /// One-line rows without avatars.
    pub const COMPACT: Self = Self {
        density: Density::Compact,
        message_row_height: COMPACT_ROW_PADDING_Y * 2.0
            + COMPACT_ROW_LINE_HEIGHT
            + ROW_BORDER_WIDTH,
        message_row_lines: 1,
        message_row_line_height: COMPACT_ROW_LINE_HEIGHT,
        message_row_line_gap: 0.0,
        message_row_padding_y: COMPACT_ROW_PADDING_Y,
        message_row_padding_x: SPACE_3,
        show_avatar: false,
        avatar_size: 0.0,
        sidebar_row_height: 32.0,
        sidebar_row_padding_x: SPACE_2,
        row_gap: SPACE_2,
        text_scale: 1.0,
    };

    /// The tokens for `density`.
    pub const fn for_density(density: Density) -> Self {
        match density {
            Density::Comfortable => Self::COMFORTABLE,
            Density::Compact => Self::COMPACT,
        }
    }

    /// These tokens at a text size `scale` times the default: the text
    /// lines and the avatar grow with the text, and the rows grow by as
    /// much as their lines do.
    pub fn scaled(self, scale: f32) -> Self {
        let lines = f32::from(self.message_row_lines);
        let fixed = self.message_row_padding_y * 2.0
            + self.message_row_line_gap * (lines - 1.0)
            + ROW_BORDER_WIDTH;
        let sidebar_text = TYPE_BODY.line_height_px(16.0);
        Self {
            message_row_height: fixed + (self.message_row_height - fixed) * scale,
            message_row_line_height: self.message_row_line_height * scale,
            avatar_size: self.avatar_size * scale,
            sidebar_row_height: self.sidebar_row_height + sidebar_text * (scale - 1.0),
            text_scale: self.text_scale * scale,
            ..self
        }
    }

    /// `pixels` at the default text size, at these tokens' text size: for
    /// the heights of a row's lines.
    pub fn text(&self, pixels: f32) -> f32 {
        pixels * self.text_scale
    }
}

/// Line height of the single line in a compact message row. It holds body
/// text, 16px icons and the 20px unread badge.
const COMPACT_ROW_LINE_HEIGHT: f32 = BADGE_SIZE;

/// Top and bottom padding of a compact message row.
const COMPACT_ROW_PADDING_Y: f32 = SPACE_1 + 2.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styles::{ICON_SIZE_SM, ROW_PREVIEW_LINE_HEIGHT, ROW_TITLE_LINE_HEIGHT};

    #[test]
    fn comfortable_is_the_phase0_layout() {
        let tokens = DensityTokens::for_density(Density::Comfortable);
        assert_eq!(tokens.message_row_height, LIST_ITEM_HEIGHT);
        assert_eq!(tokens.sidebar_row_height, MAILBOX_ITEM_HEIGHT);
        assert_eq!(
            tokens.message_row_height,
            tokens.message_row_padding_y * 2.0
                + ROW_TITLE_LINE_HEIGHT
                + ROW_SENDER_LINE_HEIGHT
                + ROW_PREVIEW_LINE_HEIGHT
                + tokens.message_row_line_gap * 2.0
                + ROW_BORDER_WIDTH
        );
    }

    #[test]
    fn compact_rows_fit_their_content_and_are_shorter() {
        let compact = DensityTokens::for_density(Density::Compact);
        let comfortable = DensityTokens::COMFORTABLE;
        assert!(compact.message_row_height < comfortable.message_row_height);
        assert!(compact.sidebar_row_height < comfortable.sidebar_row_height);

        // One line holds body text, icons and the unread badge.
        assert!(compact.message_row_line_height >= TYPE_BODY.line_height_px(16.0));
        assert!(compact.message_row_line_height >= ICON_SIZE_SM);
        assert!(compact.message_row_line_height >= BADGE_SIZE);
        let available =
            compact.message_row_height - compact.message_row_padding_y * 2.0 - ROW_BORDER_WIDTH;
        assert!(compact.message_row_line_height <= available);

        // Sidebar rows hold a line of body text with padding to spare.
        assert!(compact.sidebar_row_height >= TYPE_BODY.line_height_px(16.0) + SPACE_2);
    }

    #[test]
    fn scaled_rows_grow_with_their_text() {
        for tokens in [DensityTokens::COMFORTABLE, DensityTokens::COMPACT] {
            assert_eq!(tokens.scaled(1.0), tokens);
            let double = tokens.scaled(2.0);
            assert_eq!(double.text(20.0), 40.0);
            assert_eq!(
                double.message_row_line_height,
                tokens.message_row_line_height * 2.0
            );
            // The padding stays, and the text lines double.
            let text = |tokens: DensityTokens| {
                tokens.message_row_height
                    - tokens.message_row_padding_y * 2.0
                    - tokens.message_row_line_gap * f32::from(tokens.message_row_lines - 1)
                    - ROW_BORDER_WIDTH
            };
            assert_eq!(text(double), text(tokens) * 2.0);
            assert_eq!(double.message_row_padding_y, tokens.message_row_padding_y);
            assert!(
                double.sidebar_row_height >= TYPE_BODY.line_height_px(32.0) + SPACE_2,
                "a 200% sidebar row holds its text"
            );
        }
    }
}
