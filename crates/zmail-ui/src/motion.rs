//! Reduced motion (UI plan Phase 8).
//!
//! Every animation in the UI goes through GPUI's `with_animation`, which
//! draws one still frame and schedules no more while [`App::reduce_motion`]
//! is set. These are all the animations there are:
//!
//! - the pulse of the message list's loading placeholders
//!   (`SkeletonRow`), which stays at full opacity;
//! - the spinning loader of `IconButton::spinning` and of the onboarding
//!   steps that wait on the network, which stands still;
//! - animated images (GPUI's `img`), which show their first frame.
//!
//! Nothing slides. Menus, popovers, the sidebar overlay, modals and toasts
//! appear and go at once, the overlay scrollbar shows and hides without a
//! fade, and the text cursor does not blink. New motion must go through
//! `with_animation` (or check `cx.reduce_motion()` itself) and should be a
//! fade rather than a slide.
//!
//! Settings → Appearance → Motion ([`MotionMode`]) forces reduced or full
//! motion, or follows the system:
//!
//! - macOS: Accessibility → Display → Reduce motion
//!   (`defaults read com.apple.universalaccess reduceMotion`);
//! - Linux and the BSDs: GNOME's
//!   `org.gnome.desktop.interface enable-animations` (through `gsettings`);
//! - Windows: not read yet. `SPI_GETCLIENTAREAANIMATION` needs a binding
//!   and unsafe code the workspace does not allow, so it counts as unknown.
//!
//! The system setting is read by a short-lived subprocess on a background
//! thread, like `zmail_sync`'s power monitor, so no platform binding is
//! needed. A setting that cannot be read (no such tool, a timeout, output
//! that does not parse) is unknown, and unknown means motion is allowed.
//!
//! [`App::reduce_motion`]: gpui::App::reduce_motion

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gpui::{App, Global};
use zmail_core::preferences::MotionMode;

/// The longest a system setting read may take before it counts as unknown.
const SYSTEM_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// What the system last said about reduced motion: `Some(true)` asks for
/// less motion, `None` means it could not be read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SystemMotion(pub(crate) Option<bool>);

impl Global for SystemMotion {}

/// Whether the UI should reduce motion for the user's choice, given what
/// the system asks for. An unknown system setting allows motion.
pub(crate) fn reduces_motion(mode: MotionMode, system: Option<bool>) -> bool {
    match mode {
        MotionMode::System => system.unwrap_or(false),
        MotionMode::Reduce => true,
        MotionMode::Full => false,
    }
}

/// Applies `mode` to every window, with the last system setting read.
pub(crate) fn apply(mode: MotionMode, cx: &mut App) {
    let system = cx.try_global::<SystemMotion>().copied().unwrap_or_default();
    cx.set_reduce_motion(reduces_motion(mode, system.0));
}

/// Reads the system's reduce-motion setting. Blocks for up to
/// [`SYSTEM_READ_TIMEOUT`], so call it off the main thread.
pub(crate) fn read_system_setting() -> Option<bool> {
    if cfg!(target_os = "macos") {
        command_output(
            "defaults",
            &["read", "com.apple.universalaccess", "reduceMotion"],
        )
        .and_then(|output| parse_macos_reduce_motion(&output))
    } else if cfg!(all(unix, not(target_os = "macos"))) {
        command_output(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "enable-animations"],
        )
        .and_then(|output| parse_gnome_enable_animations(&output))
    } else {
        None
    }
}

/// `defaults read com.apple.universalaccess reduceMotion`: `1` reduces.
fn parse_macos_reduce_motion(output: &str) -> Option<bool> {
    match output.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// `gsettings get org.gnome.desktop.interface enable-animations`: `false`
/// reduces.
fn parse_gnome_enable_animations(output: &str) -> Option<bool> {
    match output.trim() {
        "false" => Some(true),
        "true" => Some(false),
        _ => None,
    }
}

/// Runs `program` and returns what it printed, or `None` if it could not
/// start, failed, or took longer than [`SYSTEM_READ_TIMEOUT`].
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + SYSTEM_READ_TIMEOUT;
    // The answer is one short line, which fits in the pipe, so waiting
    // before reading cannot deadlock.
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(Some(_)) => return None,
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut output = String::new();
    child.stdout.take()?.read_to_string(&mut output).ok()?;
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_wins_and_system_follows_the_platform() {
        for system in [None, Some(false), Some(true)] {
            assert!(reduces_motion(MotionMode::Reduce, system));
            assert!(!reduces_motion(MotionMode::Full, system));
        }
        assert!(reduces_motion(MotionMode::System, Some(true)));
        assert!(!reduces_motion(MotionMode::System, Some(false)));
        assert!(
            !reduces_motion(MotionMode::System, None),
            "an unknown system setting allows motion"
        );
    }

    #[test]
    fn platform_answers_parse_and_anything_else_is_unknown() {
        assert_eq!(parse_macos_reduce_motion("1\n"), Some(true));
        assert_eq!(parse_macos_reduce_motion("0\n"), Some(false));
        assert_eq!(parse_macos_reduce_motion(""), None);
        assert_eq!(
            parse_macos_reduce_motion("The domain/default pair does not exist"),
            None
        );
        assert_eq!(parse_gnome_enable_animations("false\n"), Some(true));
        assert_eq!(parse_gnome_enable_animations("true\n"), Some(false));
        assert_eq!(
            parse_gnome_enable_animations("No such schema “org.gnome.desktop.interface”"),
            None
        );
    }

    #[test]
    fn commands_that_fail_or_do_not_exist_are_unknown() {
        assert_eq!(command_output("zmail-no-such-program", &[]), None);
        if cfg!(unix) {
            assert_eq!(command_output("false", &[]), None);
            assert_eq!(command_output("echo", &["true"]).as_deref(), Some("true\n"));
        }
    }

    #[test]
    fn a_command_that_hangs_times_out() {
        if !cfg!(unix) {
            return;
        }
        let started = Instant::now();
        assert_eq!(command_output("sleep", &["5"]), None);
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}
