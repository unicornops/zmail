//! Zmail's terminal UI (ADR 0007), started with `zmail-app tui`.
//!
//! A second frontend on the same encrypted store as the GUI, in the style
//! of Pine and Alpine: a title bar, the screen, a status line and a
//! command bar listing the keys that work, on every screen (Main Menu,
//! Folder List, Message Index, Message Text, Compose). It reads, triages
//! (delete, archive, save, flag, read, with undo) and writes mail (compose,
//! reply, forward, drafts, attachments, send with undo-send, the Outbox)
//! and sets accounts up (Setup → Accounts: add, change, delete, through
//! `zmail-setup`, as the GUI does).
//!
//! - **Local-first.** Screens read the store, which is read again every 5
//!   seconds; sync writes to it in the background.
//! - **One syncing process.** With the store's sync lock it runs
//!   [`SyncService`] itself, as the GUI does; without it (a Zmail window is
//!   open) it shows what that window syncs.
//! - **Terminal safety.** Every string from mail goes through
//!   [`zmail_security::terminal::sanitize`] before it is drawn. The only
//!   escape sequences written are ratatui's drawing and OSC 52 for a copy
//!   the user made.
//! - **The GUI's mail actions.** Triage goes through `zmail-actions`, as
//!   the GUI's does: store writes with their server write-back queued.
//! - **Copy and paste as in Zellij.** Selection stays inside a pane and
//!   copies on release (OSC 52, `pbcopy` on a local Mac, or a
//!   `copy_command`); Shift-drag leaves the mouse to the terminal; pastes
//!   only go into text fields.

mod accounts;
mod app;
mod attachments;
mod clipboard;
mod compose;
mod data;
mod editor;
mod identities;
mod keymap;
mod links;
mod message;
mod notifier;
mod outbox;
mod palette;
mod pane;
mod paths;
mod terminal;
mod text;
mod triage;
mod view;
mod warnings;

#[cfg(test)]
mod tests;

use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result};
use crossterm::event;
use ratatui::{Terminal, backend::CrosstermBackend};
use zmail_core::time::HourCycle;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

pub use accounts::Step as AddStep;
pub use app::{App, AppOptions, Effect, Environment};
pub use keymap::{KEYMAP_FILE, UserKeymap};
pub use notifier::TerminalNotifier;

use crate::{
    app::Notice,
    clipboard::{CopyTarget, copy_target, osc52, run_copy_command},
};

/// What [`run`] needs from the app: the unlocked store, a sync service
/// built with [`SyncService::with_sync_lock`] (and Proton configured, if
/// built in), and the runtime it runs on.
pub struct Launch {
    /// The unlocked store.
    pub store: Arc<LocalStore>,
    /// Sync, which runs only while this process holds the sync lock.
    /// `None` reads the store without syncing (the fixture).
    pub sync: Option<SyncService>,
    /// The background runtime.
    pub runtime: tokio::runtime::Handle,
    /// The keychain, where Setup → Accounts keeps new accounts' secrets.
    /// `None` (the fixture) can't add accounts.
    pub secrets: Option<Arc<dyn zmail_security::SecretStore>>,
    /// Zmail's version, for the title bar.
    pub version: String,
    /// The user's keymap file ([`KEYMAP_FILE`] beside the store), read
    /// at start; `None` reads none.
    pub keymap: Option<std::path::PathBuf>,
}

/// Runs the terminal UI until the user quits.
pub fn run(launch: Launch) -> Result<()> {
    let (notifier, arrivals) = TerminalNotifier::new();
    if let Some(sync) = &launch.sync {
        sync.set_notifier(Arc::new(notifier));
        // As the GUI starts sync: these do nothing while another Zmail
        // holds the sync lock, and start if it quits first.
        let _ = sync.bootstrap_existing_accounts();
        let _ = sync.start_mail_watcher();
        sync.start_background_indexing();
    }

    let mut app = App::new(AppOptions {
        store: launch.store,
        sync: launch.sync,
        runtime: Some(launch.runtime),
        env: Environment::detect(),
        clock: Arc::new(|| chrono::Local::now().fixed_offset()),
        hour_cycle: HourCycle::from_env(),
        version: launch.version,
        arrivals: Some(arrivals),
        secrets: launch.secrets,
        network: None,
    });
    if let Some(path) = &launch.keymap {
        app.set_keymap(UserKeymap::load(path));
    }

    let stop = StopSignals::watch().context("couldn't watch for SIGTERM and SIGHUP")?;
    let guard = terminal::TerminalGuard::enter(app.preferences().mouse)
        .context("couldn't set up the terminal")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("couldn't set up the terminal")?;
    let result = event_loop(&mut app, &mut terminal, &stop);
    drop(terminal);
    drop(guard);
    result
}

/// Watches for the process being asked to stop (SIGTERM, or SIGHUP when
/// the terminal goes away): the TUI then quits as a confirmed Quit does,
/// saving the draft being written, and gives the terminal back. A second
/// signal while it does ends the process at once, as the signal would
/// have. The signals' own handling is back once this is dropped.
struct StopSignals {
    stop: Arc<AtomicBool>,
    #[cfg(unix)]
    ids: Vec<signal_hook::SigId>,
}

impl StopSignals {
    #[cfg(unix)]
    fn watch() -> io::Result<Self> {
        use signal_hook::{consts::signal, flag};
        let stop = Arc::new(AtomicBool::new(false));
        let mut ids = Vec::new();
        for signal in [signal::SIGTERM, signal::SIGHUP] {
            ids.push(flag::register_conditional_shutdown(
                signal,
                128 + signal,
                stop.clone(),
            )?);
            ids.push(flag::register(signal, stop.clone())?);
        }
        Ok(Self { stop, ids })
    }

    /// Windows has no SIGTERM or SIGHUP.
    #[cfg(not(unix))]
    fn watch() -> io::Result<Self> {
        Ok(Self {
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    fn asked(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
impl Drop for StopSignals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

fn event_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    stop: &StopSignals,
) -> Result<()> {
    loop {
        // Before drawing: after SIGHUP there may be no terminal to draw on.
        if stop.asked() {
            app.quit_now();
            return Ok(());
        }
        terminal.draw(|frame| view::draw(app, frame))?;
        run_effects(app, terminal)?;
        if app.should_quit() {
            return Ok(());
        }
        if event::poll(app.tick_interval())? {
            app.handle_event(event::read()?);
            // Take everything already waiting before drawing again, so a
            // fast drag or a burst of keys doesn't queue up frames.
            while event::poll(std::time::Duration::ZERO)? {
                app.handle_event(event::read()?);
            }
        }
        app.tick();
    }
}

fn run_effects(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    for effect in app.take_effects() {
        match effect {
            Effect::Copy(text) => {
                let env = app.env;
                match copy_target(app.preferences(), env.macos, env.over_ssh) {
                    CopyTarget::Osc52(clipboard) => {
                        let backend = terminal.backend_mut();
                        backend.write_all(osc52(&text, clipboard).as_bytes())?;
                        backend.flush()?;
                    }
                    CopyTarget::Command(command) => {
                        let notices = app.notice_sender();
                        std::thread::spawn(move || {
                            if let Err(error) = run_copy_command(&command, &text) {
                                let _ = notices.send(Notice::Status(format!(
                                    "Copy failed: {}",
                                    zmail_security::terminal::sanitize_line(&format!("{error:#}"))
                                )));
                            }
                        });
                    }
                }
            }
            Effect::Bell => {
                let backend = terminal.backend_mut();
                backend.write_all(b"\x07")?;
                backend.flush()?;
            }
            Effect::MouseCapture(on) => terminal::set_mouse_capture(on)?,
            Effect::Open(url) => {
                if let Err(error) = links::open(&url) {
                    let _ = app
                        .notice_sender()
                        .send(Notice::Status(format!("{error:#}")));
                }
            }
            Effect::Redraw => terminal.clear()?,
            Effect::Edit { command, text } => {
                // The editor gets the terminal to itself, then the TUI
                // takes it back and draws everything again.
                terminal::restore();
                let result = editor::edit(&std::env::temp_dir(), &text, &command);
                terminal::resume(app.preferences().mouse)?;
                terminal.clear()?;
                app.external_edit_done(result);
            }
        }
    }
    Ok(())
}
