//! Taking the terminal over and giving it back: raw mode, the alternate
//! screen, bracketed paste and mouse capture, restored when the TUI ends,
//! returns an error or panics.

use std::io::{self, Write};

use crossterm::{
    cursor, event, execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

/// Holds the terminal in the TUI's modes; restores it when dropped.
pub struct TerminalGuard;

impl TerminalGuard {
    /// Enters raw mode and the alternate screen, turns on bracketed paste
    /// and, when `mouse` is set, mouse capture. Installs a panic hook that
    /// restores the terminal before the panic is reported.
    pub fn enter(mouse: bool) -> io::Result<Self> {
        install_panic_hook();
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        // A terminal without bracketed paste (the legacy Windows console)
        // types a paste out as keys; nothing more can be done there.
        let _ = execute!(io::stdout(), event::EnableBracketedPaste);
        set_mouse_capture(mouse)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Turns mouse capture on or off.
pub fn set_mouse_capture(on: bool) -> io::Result<()> {
    if on {
        execute!(io::stdout(), event::EnableMouseCapture)
    } else {
        execute!(io::stdout(), event::DisableMouseCapture)
    }
}

/// Puts the terminal back as it was: every mode off, the main screen,
/// the cursor shown. Safe to call more than once.
pub fn restore() {
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        event::DisableMouseCapture,
        event::DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = terminal::disable_raw_mode();
    let _ = stdout.flush();
}

/// Restores the terminal before any panic is reported, so the message is
/// readable and the shell usable, then runs the hook that was there (the
/// app's crash log).
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}
