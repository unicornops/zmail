//! "Looks safe" on a sender-safety warning in Message Text: hides the
//! warning for that message, as the desktop app's button does
//! (`LocalStore::dismiss_sender_warning`, kept with the message). The
//! warning itself is part of the conversation (`message.rs`).

use crate::{
    app::{App, Screen},
    pane::PaneId,
};

/// The key "Looks safe" is on.
pub const LOOKS_SAFE_KEY: &str = "X";

impl App {
    /// The key shown on a warning's "Looks safe" line.
    pub(crate) fn looks_safe_key(&self) -> String {
        LOOKS_SAFE_KEY.to_owned()
    }

    /// Whether the open conversation shows a sender-safety warning.
    pub(crate) fn shows_sender_warning(&self) -> bool {
        self.screen == Screen::Message
            && self
                .message
                .as_ref()
                .is_some_and(|state| !state.conversation.warnings.is_empty())
    }

    /// The message whose warning "Looks safe" hides: the first warning
    /// on screen, else the last one above it.
    fn warning_in_view(&self) -> Option<i64> {
        let state = self.message.as_ref()?;
        let pane = self.pane(PaneId::Message);
        let (top, bottom) = pane.map_or((0, usize::MAX), |pane| {
            (pane.scroll, pane.scroll + pane.height().saturating_sub(1))
        });
        let warnings = &state.drawn_warnings;
        warnings
            .iter()
            .find(|(_, line)| (top..=bottom).contains(line))
            .or_else(|| warnings.iter().rev().find(|(_, line)| *line < top))
            .or_else(|| warnings.first())
            .map(|(id, _)| *id)
    }

    /// `X`: hides the sender-safety warning in view.
    pub(crate) fn looks_safe(&mut self) {
        let Some(message_id) = self.warning_in_view() else {
            self.status = Some("No sender warning here".to_owned());
            return;
        };
        match self.store.dismiss_sender_warning(message_id) {
            Ok(_) => {
                if let Some((thread_id, account_id)) = self
                    .message
                    .as_ref()
                    .map(|state| (state.conversation.thread_id, state.conversation.account_id))
                {
                    self.reload_message(thread_id, account_id);
                }
                self.preview = None;
                self.status = Some("Warning hidden for this message".to_owned());
            }
            Err(error) => self.report(error),
        }
    }
}
