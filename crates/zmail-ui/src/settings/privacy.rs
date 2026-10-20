//! Settings → Privacy: what Zmail does to protect you, and how many
//! sender warnings to show.
//!
//! Remote content is always blocked (loading it is out of scope for now),
//! so that row states it rather than offering a switch that does nothing.
//! Sender warnings have two levels (`UiPreferences::sender_warnings`):
//! Standard shows every warning; Fewer warnings leaves out the
//! informational one for a Reply-To on another part of the sender's own
//! domain. Warnings about lookalike senders and unrelated Reply-To domains
//! always show.

use gpui::{
    AppContext as _, Context, Entity, IntoElement, Render, Subscription, Window, div, prelude::*,
    px,
};
use zmail_core::preferences::SenderWarnings;

use super::{
    SettingsEvent, SettingsHost, apply_to_workspace, group_heading, section_heading, setting_row,
};
use crate::components::{Badge, BadgeTone, ComponentTheme, Select, SelectEvent};

/// The sender-warning levels, in picker order.
const SENDER_WARNINGS: [(SenderWarnings, &str); 2] = [
    (SenderWarnings::Standard, "Standard"),
    (SenderWarnings::Fewer, "Fewer Warnings"),
];

/// The Privacy section.
pub(crate) struct PrivacySettings {
    sender_warnings: Entity<Select>,
    _subscription: Subscription,
}

impl PrivacySettings {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let sender_warnings = cx.new(|cx| {
            Select::new(SENDER_WARNINGS.map(|(_, label)| label), None, cx)
                .label("Sender warnings")
                .width(px(200.0))
        });
        let subscription = cx.subscribe(&sender_warnings, |_, _, event: &SelectEvent, cx| {
            let SelectEvent::Changed(index) = *event;
            let level = SENDER_WARNINGS[index].0;
            match SettingsHost::get(cx)
                .store
                .update_ui_preferences(|preferences| preferences.sender_warnings = level)
            {
                Ok(_) => apply_to_workspace(cx),
                Err(error) => cx.emit(SettingsEvent::Error(format!(
                    "Could not save the sender warnings: {error:#}"
                ))),
            }
        });
        let mut this = Self {
            sender_warnings,
            _subscription: subscription,
        };
        this.reload(cx);
        this
    }

    /// Show the saved level.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let level = SettingsHost::get(cx)
            .store
            .ui_preferences()
            .unwrap_or_default()
            .sender_warnings;
        self.sender_warnings.update(cx, |select, cx| {
            select.set_selected(
                SENDER_WARNINGS
                    .iter()
                    .position(|(candidate, _)| *candidate == level),
                cx,
            )
        });
    }

    /// The sender-warnings picker, for tests to drive.
    #[cfg(test)]
    pub(crate) fn sender_warnings(&self) -> &Entity<Select> {
        &self.sender_warnings
    }
}

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
                    "A banner warns when a sender's domain imitates a well-known one, or when replies would go to a different domain. Fewer Warnings leaves out the note for replies that go to another part of the sender's own domain; the other warnings always show."
                        .into(),
                ),
                self.sender_warnings.clone(),
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
