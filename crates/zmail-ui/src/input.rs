//! Text input buffer, editor field identifiers, and key-handling primitives
//! for the Zmail UI shell.
//!
//! All keyboard-driven text editing for the workspace flows through
//! [`UiBuffer`].  [`EditorField`] identifies which logical field currently
//! has focus, and [`InputOutcome`] lets callers react to key events without
//! having to inspect the buffer themselves.

use gpui::KeyDownEvent;

// ---------------------------------------------------------------------------
// EditorField — identifies the focused input slot
// ---------------------------------------------------------------------------

/// Identifies which logical text field within [`WorkspaceView`] currently
/// receives keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorField {
    SearchQuery,
    ComposeTo,
    ComposeCc,
    ComposeBcc,
    ComposeSubject,
    ComposeBody,
    ComposeAttachmentPath,
    AccountDisplayName,
    AccountEndpoint,
    AccountUsername,
    AccountPassword,
    AccountOauthClientId,
    AccountOauthClientSecret,
    AccountOauthAuthorizationEndpoint,
    AccountOauthTokenEndpoint,
    AccountOauthScopes,
    AccountIdentityName,
    AccountIdentityEmail,
    AccountSignature,
    AccountAliasEmails,
    AccountSmtpHost,
    AccountSmtpPort,
    AccountSmtpSecurity,
    AccountSmtpUsername,
}

// ---------------------------------------------------------------------------
// UiBuffer — simple cursor-based text buffer
// ---------------------------------------------------------------------------

/// A minimal single- or multi-line text buffer with a byte cursor.
///
/// The buffer does not use GPUI's own editor widget; it is rendered as a plain
/// text label with a cursor character inserted at the current position.  This
/// keeps the dependency surface small and ensures consistent behaviour across
/// all input fields.
#[derive(Debug, Clone)]
pub(crate) struct UiBuffer {
    pub(crate) text: String,
    pub(crate) cursor: usize,
    pub(crate) multiline: bool,
    pub(crate) secret: bool,
}

impl UiBuffer {
    /// Create a single-line buffer pre-filled with `value`.
    pub(crate) fn single_line(value: impl Into<String>) -> Self {
        let text = value.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            multiline: false,
            secret: false,
        }
    }

    /// Create a multi-line buffer pre-filled with `value`.
    pub(crate) fn multiline(value: impl Into<String>) -> Self {
        let text = value.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            multiline: true,
            secret: false,
        }
    }

    /// Create a single-line password buffer pre-filled with `value`.
    /// The contents are rendered as asterisks.
    pub(crate) fn secret(value: impl Into<String>) -> Self {
        let text = value.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            multiline: false,
            secret: true,
        }
    }

    /// Replace the buffer content and move the cursor to the end.
    pub(crate) fn set(&mut self, value: impl Into<String>) {
        self.text = value.into();
        self.cursor = self.text.len();
    }

    /// Return the text that should be displayed (asterisks for secret fields).
    pub(crate) fn visible_text(&self) -> String {
        if self.secret {
            "*".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    /// Return the display string: placeholder when empty-and-inactive,
    /// visible text with a cursor character inserted when active.
    pub(crate) fn display_text(&self, active: bool) -> String {
        let mut visible = self.visible_text();
        if !active {
            if visible.is_empty() {
                return if self.multiline {
                    "Click to edit...".to_owned()
                } else {
                    "(empty)".to_owned()
                };
            }
            return visible;
        }

        let cursor = self.cursor.min(if self.secret {
            visible.len()
        } else {
            self.text.len()
        });
        visible.insert(cursor, '|');
        visible
    }

    /// Insert `new_text` at the current cursor position and advance the cursor.
    pub(crate) fn insert_text(&mut self, new_text: &str) {
        self.text.insert_str(self.cursor, new_text);
        self.cursor += new_text.len();
    }

    /// Process a GPUI key-down event, optionally applying a clipboard paste
    /// value.  Returns an [`InputOutcome`] describing what happened.
    pub(crate) fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        pasted: Option<&str>,
    ) -> InputOutcome {
        if event.is_held {
            return InputOutcome::Ignored;
        }

        // Cmd/Ctrl-V — paste
        if event.keystroke.modifiers.platform && event.keystroke.key.eq_ignore_ascii_case("v") {
            if let Some(pasted) = pasted {
                let pasted = if self.multiline {
                    pasted.to_owned()
                } else {
                    pasted.replace('\n', " ")
                };
                self.insert_text(&pasted);
                return InputOutcome::Changed;
            }
            return InputOutcome::Ignored;
        }

        match event.keystroke.key.as_str() {
            "backspace" => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            "delete" => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            "left" => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            "right" => {
                if self.cursor < self.text.len() {
                    self.cursor += 1;
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
            "home" => {
                self.cursor = 0;
                InputOutcome::Changed
            }
            "end" => {
                self.cursor = self.text.len();
                InputOutcome::Changed
            }
            "enter" => {
                if self.multiline {
                    self.insert_text("\n");
                    InputOutcome::Changed
                } else {
                    InputOutcome::Submitted
                }
            }
            _ => {
                if event.keystroke.modifiers.control || event.keystroke.modifiers.platform {
                    return InputOutcome::Ignored;
                }
                if let Some(text) = event
                    .keystroke
                    .key_char
                    .as_deref()
                    .filter(|text| !text.is_empty())
                {
                    self.insert_text(text);
                    InputOutcome::Changed
                } else {
                    InputOutcome::Ignored
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// InputOutcome — result of handling a key event
// ---------------------------------------------------------------------------

/// The result of processing a key event through [`UiBuffer::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputOutcome {
    /// The buffer content or cursor changed.
    Changed,
    /// The user pressed Enter on a single-line field (submit action).
    Submitted,
    /// The key event was not handled by this buffer.
    Ignored,
}
