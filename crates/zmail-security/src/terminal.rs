//! Filtering mail text for a terminal (ADR 0007, "Terminal safety").
//!
//! Mail text is attacker-controlled. Written to a terminal unfiltered, an
//! escape sequence in a subject or body can move the cursor and repaint
//! the screen, fake a prompt, set the window title, write the clipboard
//! (OSC 52) or plant a hyperlink (OSC 8). Every string from mail goes
//! through [`sanitize`] (or [`sanitize_line`] for one-line fields) before
//! a terminal frontend draws it.
//!
//! [`sanitize`] removes:
//!
//! - C0 controls except newline; a tab becomes spaces to the next tab stop
//!   and a carriage return a newline (`\r\n` is one newline);
//! - DEL and every C1 control (U+0080–U+009F);
//! - ESC and everything it introduces: CSI (`ESC [`), OSC (`ESC ]`), DCS
//!   (`ESC P`), SOS (`ESC X`), PM (`ESC ^`), APC (`ESC _`) and the short
//!   escapes (`ESC 7`, `ESC c`, `ESC ( B`), and the same sequences started
//!   by their 8-bit C1 introducers (U+009B CSI, U+009D OSC, U+0090 DCS,
//!   U+0098 SOS, U+009E PM, U+009F APC);
//! - the bidirectional overrides, isolates and marks and the zero-width
//!   characters that [`crate::attachment_safety::sanitize_filename`]
//!   removes from file names.
//!
//! A control string (OSC, DCS, SOS, PM, APC) is removed up to its
//! terminator, as a terminal would swallow it; one that never ends takes
//! the rest of the text with it. A CSI sequence ends at its final byte, or
//! at the first character that cannot be part of one, which is then kept
//! (if it is safe).
//!
//! The output contains no character below U+0020 other than `\n`, no DEL
//! and no C1 control, whatever the input.

use crate::attachment_safety::is_invisible_format_char;

/// Tab stops are every this many columns, as in most terminals.
pub const TAB_WIDTH: usize = 8;

const ESC: char = '\u{1B}';
const BEL: char = '\u{07}';
/// String Terminator, the 8-bit form of `ESC \`.
const ST: char = '\u{9C}';

/// Returns `text` with every control character and escape sequence
/// removed, safe to write to a terminal. Newlines are kept; see the
/// module documentation for everything else.
pub fn sanitize(text: &str) -> String {
    let mut filter = Filter::new(text.len());
    for ch in text.chars() {
        filter.push(ch);
    }
    filter.output
}

/// [`sanitize`] for a one-line field (a subject, a sender, a file name, a
/// mailbox name, an error message): every run of whitespace, newlines
/// included, becomes one space, and the ends are trimmed.
pub fn sanitize_line(text: &str) -> String {
    let sanitized = sanitize(text);
    let mut line = String::with_capacity(sanitized.len());
    for word in sanitized.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    line
}

/// Where the filter is in an escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Plain text.
    Ground,
    /// After ESC.
    Escape,
    /// After ESC and one or more intermediate bytes (`ESC ( B`).
    EscapeIntermediate,
    /// In a CSI sequence's parameter and intermediate bytes.
    Csi,
    /// In a control string (OSC, DCS, SOS, PM, APC). OSC also ends at BEL.
    ControlString { ends_at_bel: bool },
    /// After ESC inside a control string: `\` ends it, anything else
    /// abandons it and starts a new escape sequence, as terminals do.
    ControlStringEscape { ends_at_bel: bool },
}

struct Filter {
    output: String,
    state: State,
    /// Characters since the last newline, for tab stops.
    column: usize,
    /// The last character written was a newline made from `\r`, so a
    /// following `\n` belongs to the same line break.
    after_carriage_return: bool,
}

impl Filter {
    fn new(capacity: usize) -> Self {
        Self {
            output: String::with_capacity(capacity),
            state: State::Ground,
            column: 0,
            after_carriage_return: false,
        }
    }

    fn push(&mut self, ch: char) {
        // A character that ends an escape sequence without belonging to it
        // goes round again in the ground state.
        while !self.step(ch) {}
    }

    /// Feeds one character; `false` when it was not consumed and must be
    /// fed again in the new state.
    fn step(&mut self, ch: char) -> bool {
        match self.state {
            State::Ground => {
                match ch {
                    ESC => self.state = State::Escape,
                    '\u{9B}' => self.state = State::Csi,
                    '\u{9D}' => self.state = State::ControlString { ends_at_bel: true },
                    '\u{90}' | '\u{98}' | '\u{9E}' | '\u{9F}' => {
                        self.state = State::ControlString { ends_at_bel: false }
                    }
                    _ => self.text(ch),
                }
                true
            }
            State::Escape => match ch {
                '[' => {
                    self.state = State::Csi;
                    true
                }
                ']' => {
                    self.state = State::ControlString { ends_at_bel: true };
                    true
                }
                'P' | 'X' | '^' | '_' => {
                    self.state = State::ControlString { ends_at_bel: false };
                    true
                }
                '\x20'..='\x2F' => {
                    self.state = State::EscapeIntermediate;
                    true
                }
                '\x30'..='\x7E' => {
                    self.state = State::Ground;
                    true
                }
                ESC => true,
                _ => {
                    self.state = State::Ground;
                    false
                }
            },
            State::EscapeIntermediate => match ch {
                '\x20'..='\x2F' => true,
                '\x30'..='\x7E' => {
                    self.state = State::Ground;
                    true
                }
                _ => {
                    self.state = State::Ground;
                    false
                }
            },
            State::Csi => match ch {
                '\x20'..='\x3F' => true,
                '\x40'..='\x7E' => {
                    self.state = State::Ground;
                    true
                }
                _ => {
                    self.state = State::Ground;
                    false
                }
            },
            State::ControlString { ends_at_bel } => {
                match ch {
                    BEL if ends_at_bel => self.state = State::Ground,
                    ST => self.state = State::Ground,
                    ESC => self.state = State::ControlStringEscape { ends_at_bel },
                    _ => {}
                }
                true
            }
            State::ControlStringEscape { ends_at_bel } => match ch {
                '\\' => {
                    self.state = State::Ground;
                    true
                }
                // `ESC ESC`: still waiting for the backslash.
                ESC => {
                    self.state = State::ControlStringEscape { ends_at_bel };
                    true
                }
                _ => {
                    self.state = State::Escape;
                    false
                }
            },
        }
    }

    /// A character outside any escape sequence.
    fn text(&mut self, ch: char) {
        let after_carriage_return = std::mem::take(&mut self.after_carriage_return);
        match ch {
            '\n' if after_carriage_return => {}
            '\n' | '\u{2028}' | '\u{2029}' => self.newline(),
            '\r' => {
                self.newline();
                self.after_carriage_return = true;
            }
            '\t' => {
                let spaces = TAB_WIDTH - self.column % TAB_WIDTH;
                self.output.extend(std::iter::repeat_n(' ', spaces));
                self.column += spaces;
            }
            // `char::is_control` is exactly C0, DEL and C1.
            ch if ch.is_control() || is_invisible_format_char(ch) => {}
            ch => {
                self.output.push(ch);
                self.column += 1;
            }
        }
    }

    fn newline(&mut self) {
        self.output.push('\n');
        self.column = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{sanitize, sanitize_line};

    #[test]
    fn plain_text_is_kept() {
        assert_eq!(
            sanitize("Hello, world!\nSecond line"),
            "Hello, world!\nSecond line"
        );
        assert_eq!(
            sanitize("Grüße, 日本語, emoji 📬"),
            "Grüße, 日本語, emoji 📬"
        );
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn csi_sequences_are_removed() {
        assert_eq!(sanitize("a\x1b[31mred\x1b[0m b"), "ared b");
        assert_eq!(sanitize("\x1b[2J\x1b[H\x1b[10;20Hfake"), "fake");
        assert_eq!(sanitize("x\x1b[?1049hy"), "xy");
        // 8-bit CSI.
        assert_eq!(sanitize("a\u{9b}31mb"), "ab");
    }

    #[test]
    fn a_csi_sequence_ends_at_the_first_character_that_cannot_be_in_one() {
        // `é` cannot be a parameter or final byte: the sequence is
        // abandoned there and the letter kept.
        assert_eq!(sanitize("\x1b[12é done"), "é done");
        assert_eq!(sanitize("\x1b[12\nnext"), "\nnext");
    }

    #[test]
    fn osc_strings_are_removed_up_to_their_terminator() {
        assert_eq!(sanitize("\x1b]0;evil title\x07after"), "after");
        assert_eq!(sanitize("\x1b]2;evil title\x1b\\after"), "after");
        assert_eq!(sanitize("\x1b]52;c;aGVsbG8=\x07after"), "after");
        assert_eq!(
            sanitize("\x1b]8;;https://evil.example/\x1b\\click\x1b]8;;\x1b\\"),
            "click"
        );
        // 8-bit OSC, ended by the 8-bit String Terminator.
        assert_eq!(sanitize("\u{9d}0;title\u{9c}after"), "after");
    }

    #[test]
    fn dcs_sos_pm_and_apc_strings_are_removed() {
        assert_eq!(sanitize("a\x1bPq#0;2;0;0;0\x1b\\b"), "ab");
        assert_eq!(sanitize("a\x1bXsos\x1b\\b"), "ab");
        assert_eq!(sanitize("a\x1b^pm\x1b\\b"), "ab");
        assert_eq!(sanitize("a\x1b_Gf=100;AAAA\x1b\\b"), "ab");
        assert_eq!(sanitize("a\u{90}dcs\u{9c}b"), "ab");
        // BEL ends only OSC: a DCS keeps going to its String Terminator.
        assert_eq!(sanitize("a\x1bPx\x07y\x1b\\b"), "ab");
    }

    #[test]
    fn unterminated_strings_take_the_rest_of_the_text() {
        assert_eq!(sanitize("before\x1b]0;never ends\nmore"), "before");
        assert_eq!(sanitize("before\x1bP"), "before");
        assert_eq!(sanitize("before\x1b"), "before");
        assert_eq!(sanitize("before\x1b["), "before");
    }

    #[test]
    fn an_escape_inside_a_string_starts_a_new_sequence() {
        assert_eq!(sanitize("\x1b]0;t\x1b[31mvisible"), "visible");
    }

    #[test]
    fn short_escapes_are_removed() {
        assert_eq!(sanitize("a\x1bcb"), "ab");
        assert_eq!(sanitize("a\x1b7b\x1b8c"), "abc");
        assert_eq!(sanitize("a\x1b(Bb"), "ab");
        assert_eq!(sanitize("a\x1b#8b"), "ab");
        // ESC before something that cannot follow it is dropped alone.
        assert_eq!(sanitize("a\x1bé"), "aé");
    }

    #[test]
    fn controls_are_removed_and_tabs_become_spaces() {
        assert_eq!(sanitize("a\x00b\x07c\x08d\x7fe"), "abcde");
        assert_eq!(sanitize("a\u{85}b\u{9c}c"), "abc");
        assert_eq!(sanitize("a\tb"), "a       b");
        assert_eq!(sanitize("abcdefgh\tx"), "abcdefgh        x");
        assert_eq!(sanitize("12\n\tx"), "12\n        x");
    }

    #[test]
    fn carriage_returns_become_newlines() {
        assert_eq!(sanitize("one\r\ntwo\rthree"), "one\ntwo\nthree");
        assert_eq!(sanitize("a\r\n\r\nb"), "a\n\nb");
        assert_eq!(sanitize("a\u{2028}b"), "a\nb");
    }

    #[test]
    fn bidi_and_zero_width_characters_are_removed() {
        assert_eq!(sanitize("invoice\u{202E}fdp.exe"), "invoicefdp.exe");
        assert_eq!(sanitize("a\u{2066}b\u{2069}c"), "abc");
        assert_eq!(sanitize("pay\u{200B}pal"), "paypal");
        assert_eq!(sanitize("\u{FEFF}bom"), "bom");
    }

    #[test]
    fn sanitize_line_folds_whitespace() {
        assert_eq!(
            sanitize_line("  Re:\r\n  hello\tworld \n"),
            "Re: hello world"
        );
        assert_eq!(sanitize_line("\x1b]0;x\x07"), "");
    }
}
