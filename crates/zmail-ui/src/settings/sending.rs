//! Settings → Sending: how long a sent message waits in the Outbox so it
//! can be undone (`UiPreferences::undo_send_delay_secs`).
//!
//! The compose view reads the delay from the store each time it sends, so
//! a change applies to the next message without anything else to update.

use gpui::{Context, Entity, IntoElement, Render, Subscription, Window, div, prelude::*, px};
use zmail_core::preferences::MAX_UNDO_SEND_DELAY_SECS;

use super::{SettingsEvent, SettingsHost, section_heading, setting_row};
use crate::components::{ComponentTheme, Select, SelectEvent};

/// The delays offered, in seconds; 0 turns Undo Send off.
pub(crate) const UNDO_SEND_DELAYS: [u32; 5] = [0, 5, 10, 20, 30];

/// The Sending section.
pub(crate) struct SendingSettings {
    undo_send: Entity<Select>,
    /// The delays the picker lists: [`UNDO_SEND_DELAYS`], plus the saved
    /// one when it is not among them (set by an older version, say).
    delays: Vec<u32>,
    _subscription: Option<Subscription>,
}

impl SendingSettings {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let undo_send = cx.new(|cx| Select::new(Vec::<String>::new(), None, cx).label("Undo Send"));
        let mut this = Self {
            undo_send,
            delays: Vec::new(),
            _subscription: None,
        };
        this.reload(cx);
        this
    }

    /// Show the saved delay.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let saved = SettingsHost::get(cx)
            .store
            .ui_preferences()
            .unwrap_or_default()
            .undo_send_delay_secs
            .min(MAX_UNDO_SEND_DELAY_SECS);
        let mut delays = UNDO_SEND_DELAYS.to_vec();
        if !delays.contains(&saved) {
            delays.push(saved);
            delays.sort_unstable();
        }
        if delays == self.delays {
            self.undo_send.update(cx, |select, cx| {
                select.set_selected(delays.iter().position(|delay| *delay == saved), cx)
            });
            return;
        }
        let selected = delays.iter().position(|delay| *delay == saved);
        let labels: Vec<String> = delays.iter().map(|delay| delay_label(*delay)).collect();
        self.undo_send = cx.new(|cx| {
            Select::new(labels, selected, cx)
                .label("Undo Send")
                .width(px(180.0))
        });
        self._subscription = Some(cx.subscribe(
            &self.undo_send,
            |this, _, event: &SelectEvent, cx| {
                let SelectEvent::Changed(index) = *event;
                if let Some(delay) = this.delays.get(index).copied() {
                    this.save(delay, cx);
                }
            },
        ));
        self.delays = delays;
        cx.notify();
    }

    fn save(&mut self, delay: u32, cx: &mut Context<Self>) {
        if let Err(error) = SettingsHost::get(cx)
            .store
            .update_ui_preferences(|preferences| preferences.undo_send_delay_secs = delay)
        {
            cx.emit(SettingsEvent::Error(format!(
                "Could not save the Undo Send delay: {error:#}"
            )));
        }
    }

    /// The picker, for tests to drive.
    #[cfg(test)]
    pub(crate) fn select(&self) -> &Entity<Select> {
        &self.undo_send
    }
}

/// How a delay reads in the picker.
pub(crate) fn delay_label(seconds: u32) -> String {
    match seconds {
        0 => "Off".to_owned(),
        1 => "1 second".to_owned(),
        seconds => format!("{seconds} seconds"),
    }
}

impl Render for SendingSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        div()
            .flex()
            .flex_col()
            .child(section_heading("Sending"))
            .child(setting_row(
                "Undo Send",
                Some(
                    "After you press Send, the message waits this long in the Outbox, with an Undo button, before it goes."
                        .into(),
                ),
                self.undo_send.clone(),
                &colors,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::delay_label;

    #[test]
    fn delays_read_as_words() {
        assert_eq!(delay_label(0), "Off");
        assert_eq!(delay_label(1), "1 second");
        assert_eq!(delay_label(10), "10 seconds");
    }
}
