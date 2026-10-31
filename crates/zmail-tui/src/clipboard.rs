//! Where a copy goes (ADR 0007 section 3), as Zellij does it.
//!
//! - **OSC 52** by default: the terminal sets the clipboard, locally and
//!   over SSH, in terminals that support it (iTerm2, kitty, WezTerm,
//!   Alacritty, foot, Windows Terminal, tmux with `set-clipboard on`).
//! - **`pbcopy`** on a Mac when not over SSH, because Terminal.app has no
//!   OSC 52.
//! - **A `copy_command`** when the user set one (`wl-copy`, `xclip
//!   -selection clipboard`): the text goes to its standard input. It runs
//!   without a shell.
//!
//! What is copied is always text the TUI drew, so it has been through
//! the terminal filter; the OSC 52 payload is that text in base64, which
//! cannot carry an escape sequence.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use base64::Engine;
use zmail_core::preferences::{CopyClipboard, TerminalPreferences};

/// The OSC 52 sequence that puts `text` on `clipboard`.
pub fn osc52(text: &str, clipboard: CopyClipboard) -> String {
    let selection = match clipboard {
        CopyClipboard::System => 'c',
        CopyClipboard::Primary => 'p',
    };
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;{selection};{payload}\x07")
}

/// How a copy reaches the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyTarget {
    /// Write OSC 52 to the terminal.
    Osc52(CopyClipboard),
    /// Pipe the text to this program and its arguments.
    Command(Vec<String>),
}

/// Where copies go with these settings, in this environment.
pub fn copy_target(preferences: &TerminalPreferences, macos: bool, over_ssh: bool) -> CopyTarget {
    if let Some(command) = preferences
        .copy_command
        .as_deref()
        .map(split_command)
        .filter(|words| !words.is_empty())
    {
        return CopyTarget::Command(command);
    }
    if macos && !over_ssh {
        return CopyTarget::Command(vec!["pbcopy".to_owned()]);
    }
    CopyTarget::Osc52(preferences.copy_clipboard)
}

/// Whether this session runs over SSH, as Zellij decides it.
pub fn over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// Splits a command line into words: whitespace separates them, and
/// single or double quotes keep spaces inside one. No shell runs it, so
/// nothing else is special.
pub fn split_command(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(open), ch) if ch == open => quote = None,
            (Some(_), ch) => word.push(ch),
            (None, '"' | '\'') => {
                quote = Some(ch);
                in_word = true;
            }
            (None, ch) if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, ch) => {
                word.push(ch);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Pipes `text` to `command` and waits for it.
pub fn run_copy_command(command: &[String], text: &str) -> Result<()> {
    let Some((program, arguments)) = command.split_first() else {
        bail!("the copy command is empty");
    };
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("couldn't run {program}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .with_context(|| format!("couldn't write to {program}"))?;
    }
    let status = child
        .wait()
        .with_context(|| format!("{program} didn't finish"))?;
    if !status.success() {
        bail!("{program} failed ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use zmail_core::preferences::{CopyClipboard, TerminalPreferences};

    use super::{CopyTarget, copy_target, osc52, split_command};

    #[test]
    fn osc52_carries_the_text_in_base64() {
        assert_eq!(
            osc52("hello", CopyClipboard::System),
            "\x1b]52;c;aGVsbG8=\x07"
        );
        assert_eq!(
            osc52("hello", CopyClipboard::Primary),
            "\x1b]52;p;aGVsbG8=\x07"
        );
        // Whatever the text, the payload is only base64, so it cannot end
        // the sequence or start another.
        let text = "日本 \u{7}\x1b]0;x\x07 end";
        let sequence = osc52(text, CopyClipboard::System);
        let payload = sequence
            .strip_prefix("\x1b]52;c;")
            .and_then(|rest| rest.strip_suffix('\x07'))
            .expect("one OSC 52 sequence");
        assert!(
            payload
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        );
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("valid base64");
        assert_eq!(decoded, text.as_bytes());
    }

    #[test]
    fn copies_go_to_osc52_unless_on_a_local_mac_or_a_command_is_set() {
        let defaults = TerminalPreferences::default();
        assert_eq!(
            copy_target(&defaults, false, false),
            CopyTarget::Osc52(CopyClipboard::System)
        );
        assert_eq!(
            copy_target(&defaults, true, false),
            CopyTarget::Command(vec!["pbcopy".to_owned()])
        );
        assert_eq!(
            copy_target(&defaults, true, true),
            CopyTarget::Osc52(CopyClipboard::System)
        );
        let primary = TerminalPreferences {
            copy_clipboard: CopyClipboard::Primary,
            ..TerminalPreferences::default()
        };
        assert_eq!(
            copy_target(&primary, false, true),
            CopyTarget::Osc52(CopyClipboard::Primary)
        );
        let command = TerminalPreferences {
            copy_command: Some("xclip -selection clipboard".to_owned()),
            ..TerminalPreferences::default()
        };
        for (macos, ssh) in [(false, false), (true, false), (true, true)] {
            assert_eq!(
                copy_target(&command, macos, ssh),
                CopyTarget::Command(vec![
                    "xclip".to_owned(),
                    "-selection".to_owned(),
                    "clipboard".to_owned()
                ])
            );
        }
        let blank = TerminalPreferences {
            copy_command: Some("   ".to_owned()),
            ..TerminalPreferences::default()
        };
        assert_eq!(
            copy_target(&blank, false, false),
            CopyTarget::Osc52(CopyClipboard::System)
        );
    }

    #[test]
    fn commands_split_on_spaces_outside_quotes() {
        assert_eq!(split_command("wl-copy"), vec!["wl-copy"]);
        assert_eq!(
            split_command("  xclip  -selection clipboard "),
            vec!["xclip", "-selection", "clipboard"]
        );
        assert_eq!(
            split_command(r#"/opt/my tools/copy "a b" 'c d' e"f"#),
            vec!["/opt/my", "tools/copy", "a b", "c d", "ef"]
        );
        assert_eq!(split_command(r#""" x"#), vec!["", "x"]);
        assert!(split_command("").is_empty());
    }
}
