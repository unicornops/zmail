//! Zmail's terminal UI (ADR 0007), started with `zmail-app tui`.
//!
//! A second frontend on the same encrypted store as the GUI, in the style
//! of Pine and Alpine: a title bar, the screen, a status line and a
//! command bar listing the keys that work, on every screen (Main Menu,
//! Folder List, Message Index, Message Text). Phase 1 reads mail; triage,
//! compose and accounts come later.
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
//! - **Copy and paste as in Zellij.** Selection stays inside a pane and
//!   copies on release (OSC 52, `pbcopy` on a local Mac, or a
//!   `copy_command`); Shift-drag leaves the mouse to the terminal; pastes
//!   only go into text fields.

mod app;
mod clipboard;
mod data;
mod links;
mod message;
mod notifier;
mod pane;
mod terminal;
mod text;
mod view;

#[cfg(test)]
mod tests;

use std::{
    io::{self, Write},
    sync::Arc,
};

use anyhow::{Context, Result};
use crossterm::event;
use ratatui::{Terminal, backend::CrosstermBackend};
use zmail_core::time::HourCycle;
use zmail_store::LocalStore;
use zmail_sync::SyncService;

pub use app::{App, AppOptions, Effect, Environment};
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
    /// Zmail's version, for the title bar.
    pub version: String,
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
    });

    let guard = terminal::TerminalGuard::enter(app.preferences().mouse)
        .context("couldn't set up the terminal")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("couldn't set up the terminal")?;
    let result = event_loop(&mut app, &mut terminal);
    drop(terminal);
    drop(guard);
    result
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    loop {
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
        }
    }
    Ok(())
}
