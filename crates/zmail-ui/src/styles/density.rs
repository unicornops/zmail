//! Density tokens: row heights and paddings for the comfortable and
//! compact layouts (UI plan §2, "Message rows").
//!
//! Comfortable matches the Phase 0 layout constants exactly, so views can
//! move to [`DensityTokens`] without changing a pixel.

use zmail_core::preferences::Density;

use super::{
    AVATAR_SIZE, BADGE_SIZE, LIST_ITEM_HEIGHT, MAILBOX_ITEM_HEIGHT, ROW_BORDER_WIDTH, ROW_LINE_GAP,
    ROW_PADDING_Y, ROW_SENDER_LINE_HEIGHT, SPACE_1, SPACE_2, SPACE_3, SPACE_4,
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
    };

    /// The tokens for `density`.
    pub const fn for_density(density: Density) -> Self {
        match density {
            Density::Comfortable => Self::COMFORTABLE,
            Density::Compact => Self::COMPACT,
        }
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
    use crate::styles::{ICON_SIZE_SM, ROW_PREVIEW_LINE_HEIGHT, ROW_TITLE_LINE_HEIGHT, TYPE_BODY};

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
}
