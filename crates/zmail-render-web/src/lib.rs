//! Zmail's web renderer for HTML mail (ADR 0006).
//!
//! A prepared message (`zmail_render::PreparedMessage`, turned into a
//! [`WebDocument`] by [`WebDocument::from_prepared`]) is shown in a
//! locked-down OS web view placed inside the GPUI window through GPUI's native window
//! composition (Zed PR #62379): the web view sits between GPUI's base
//! surface and its overlay surface, so Zmail's menus, popovers, dialogs and
//! tooltips draw above it.
//!
//! macOS (`WKWebView`) and Windows (WebView2, hosted as DirectComposition
//! visuals) are implemented. On other platforms [`SUPPORTED`] is false and
//! the reader keeps its text view; so does a Windows machine without the
//! WebView2 runtime ([`WebHost::unavailable_reason`] says why).
//!
//! | Module | Contents |
//! |---|---|
//! | `document` | The prepared message, the page served for it, the CSP and the origin checks |
//! | `rules` | The content blocker's rule list |
//! | `remote` | Remote images the user allowed, fetched by Zmail (never by the engine) |
//! | `script` | Zmail's own scripts in a page (measuring, Copy, Select All) and their results |
//! | `host` | GPUI: the per-reader host, the viewport wrapper and the body element |
//! | `platform::macos_host` | The macOS backend of the host: the composition surface and its web views |
//! | `platform::macos` | AppKit and WebKit (`unsafe`) |
//! | `platform::windows_host` | The Windows backend of the host: composition surfaces, web views and the input forwarded to them |
//! | `platform::windows` | WebView2 and Win32 (`unsafe`) |
//! | `platform::linux` | WebKitGTK, loaded at runtime, on an engine thread of its own (`unsafe`) |
//! | `platform::linux_sandbox` | Checks that WebKitGTK's sandbox can start, and did |
//! | `platform::linux_proxy` | The Linux engine's black-hole network proxy |
//!
//! The `platform` FFI modules are the only `unsafe` code in Zmail.
//!
//! Set `ZMAIL_WEB_DEBUG=1` to log each step to stderr.

pub mod document;
pub(crate) mod host;
mod platform;
pub mod remote;
pub mod rules;
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
mod script;

use std::sync::OnceLock;

pub use document::{Appearance, InlineResource, WebDocument};
pub use host::{WebBody, WebHost, WebKey, WebViewport};
use zmail_render::{MessageRenderer, PreparedMessage, RendererKind};

/// Whether this platform has a web renderer.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// The web renderer behind ADR 0006's renderer boundary: it takes any
/// prepared message with sanitised HTML, on a platform that has it.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebRenderer;

impl MessageRenderer for WebRenderer {
    fn kind(&self) -> RendererKind {
        RendererKind::Web
    }

    fn accepts(&self, message: &PreparedMessage) -> bool {
        SUPPORTED && message.html().is_ok()
    }
}

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
    /// The engine's content process ended (crash, jetsam, or WebView2's
    /// browser or renderer process failing).
    ContentProcessTerminated,
    /// The page could not be shown.
    LoadFailed(String),
}

/// Logs a step of the web renderer when `ZMAIL_WEB_DEBUG` is set.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
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

/// The Windows backend without GPUI, for the canary test.
#[cfg(target_os = "windows")]
#[doc(hidden)]
pub mod windows_testing {
    pub use crate::platform::windows::{PlatformWebView, TestWindow, clipboard_text};
}

/// The Linux engine without GPUI, for the canary test.
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub mod linux_testing {
    pub use crate::platform::linux::{
        DisplayServer, MIN_VERSION, PlatformWebView, ViewEvent, ViewSize, blocked_connections,
        failure, start, verified_web_processes,
    };
}
