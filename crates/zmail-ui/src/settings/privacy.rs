//! Settings → Privacy: what Zmail does to protect you, as it is today.
//!
//! Remote content is always blocked (loading it is out of scope for now),
//! and sender-safety warnings always show; neither has a setting yet, so
//! the section states them rather than offering a switch that does
//! nothing.

use gpui::{Context, IntoElement, Render, Window, div, prelude::*};

use super::{group_heading, section_heading, setting_row};
use crate::components::{Badge, BadgeTone, ComponentTheme};

/// The Privacy section.
pub(crate) struct PrivacySettings;

impl Render for PrivacySettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        div()
            .flex()
            .flex_col()
            .child(section_heading("Privacy"))
            .child(setting_row(
                "Remote content",
                Some(
                    "Images and other files a message loads from the web can tell the sender when and where you read it. Zmail never loads them, and says so above a conversation that has any."
                        .into(),
                ),
                Badge::label("Always blocked").tone(BadgeTone::Neutral),
                &colors,
            ))
            .child(setting_row(
                "Sender warnings",
                Some(
                    "A banner warns when a sender's domain imitates a well-known one, or when replies would go to a different domain than the sender's. \u{201c}Looks safe\u{201d} hides it for that message."
                        .into(),
                ),
                Badge::label("On").tone(BadgeTone::Neutral),
                &colors,
            ))
            .child(group_heading("Links and attachments", &colors))
            .child(setting_row(
                "Before opening",
                Some(
                    "A link opens only after a dialog shows where it really goes, and warns when its text names a different site. Zmail also asks before opening a program, a script or a web page from an attachment."
                        .into(),
                ),
                Badge::label("Always").tone(BadgeTone::Neutral),
                &colors,
            ))
    }
}
