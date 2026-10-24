//! Zmail's web renderer for HTML mail (ADR 0006).
//!
//! A prepared message ([`WebDocument`]) is shown in a locked-down OS web
//! view placed inside the GPUI window through GPUI's native window
//! composition (Zed PR #62379): the web view sits between GPUI's base
//! surface and its overlay surface, so Zmail's menus, popovers, dialogs and
//! tooltips draw above it.
//!
//! Only macOS (`WKWebView`) is implemented. On other platforms
//! [`SUPPORTED`] is false and the reader keeps its text view.
//!
//! | Module | Contents |
//! |---|---|
//! | `document` | The prepared message, the page served for it, the CSP and the origin checks |
//! | `rules` | The content blocker's rule list |
//! | `host` | GPUI: the per-reader host, the viewport wrapper and the body element |
//! | `platform::macos` | AppKit and WebKit, the only `unsafe` code in Zmail |
//!
//! Set `ZMAIL_WEB_DEBUG=1` to log each step to stderr.

pub mod document;
mod host;
mod platform;
pub mod rules;

use std::sync::OnceLock;

pub use document::{Appearance, InlineResource, WebDocument};
pub use host::{WebBody, WebHost, WebKey, WebViewport};

/// Whether this platform has a web renderer.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// Something a web view reports back to Zmail.
#[derive(Debug, Clone, PartialEq)]
pub enum WebEvent {
    /// The page was laid out: its height in points, after any zoom used
    /// to fit its width.
    Measured {
        /// The page's height.
        height: f32,
    },
    /// The user clicked a link. Nothing was opened: Zmail decides.
    LinkActivated {
        /// The link's URL (`zmail-link:<n>` for sanitised mail).
        url: String,
    },
    /// A mouse press in the page.
    Pressed,
    /// The engine's content process ended (crash or jetsam).
    ContentProcessTerminated,
    /// The page could not be shown.
    LoadFailed(String),
}

/// Logs a step of the web renderer when `ZMAIL_WEB_DEBUG` is set.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn debug_log(args: std::fmt::Arguments<'_>) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        std::env::var_os("ZMAIL_WEB_DEBUG").is_some_and(|value| !value.is_empty() && value != "0")
    });
    if enabled {
        eprintln!("zmail: web renderer: {args}");
    }
}

/// The macOS backend without GPUI, for the canary test.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod macos_testing {
    pub use crate::platform::macos::{Frame, OffscreenWindow, PlatformWebView};
}
