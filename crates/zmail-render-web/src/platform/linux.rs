//! The Linux web renderer: a locked-down WebKitGTK 4.1 web view per
//! message.
//!
//! This module and the other platform FFI modules are the only `unsafe`
//! code in Zmail: it calls WebKitGTK, GTK 3 and GLib through function
//! pointers, and every `unsafe` block says why it is sound. The rest of the
//! crate sees a small safe API: [`start`] and [`failure`] for the engine,
//! and a [`PlatformWebView`] per message.
//!
//! **Loading.** WebKitGTK is not linked: [`start`] loads
//! `libwebkit2gtk-4.1.so.0` (which brings GTK, GLib, libsoup and
//! JavaScriptCore) with `dlopen` the first time a message is shown
//! formatted. A missing library, or one older than [`MIN_VERSION`], makes
//! the web renderer unavailable and the reader keeps its text view; Zmail
//! itself never depends on WebKitGTK to start.
//!
//! **Sandbox.** Before the engine is touched, `linux_sandbox` refuses
//! environments where it would run without its bubblewrap sandbox and
//! probes bubblewrap (a failed sandbox start aborts the whole process
//! inside WebKitGTK, so it must never be reached). The engine is then
//! started with the sandbox on, loads a blank page first, and only once
//! that page's web process is confirmed to live in its own user and PID
//! namespaces is any mail loaded. The check is repeated after every page
//! load; a web process found outside its sandbox stops the engine.
//!
//! **Thread.** GPUI owns the main thread and its event loop, so GTK and
//! WebKit live on a thread of their own (`zmail-webkit`) that owns GLib's
//! default main context and runs its main loop. Commands reach it through
//! a channel and `g_main_context_invoke`; events come back on a channel the
//! caller passes in (a GPUI foreground task reads it).
//!
//! Each web view is hardened as ADR 0006 §3 requires:
//!
//! - Page JavaScript is off (`enable-javascript` and
//!   `enable-javascript-markup`). Zmail's own measuring script runs in an
//!   isolated script world (`zmail`) that page content cannot see.
//! - The page comes from Zmail's `zmail-mail:` scheme handler with a CSP
//!   response header (and a `<meta>` copy); `cid:` parts and remote images
//!   the user allowed (fetched by Zmail itself, `crate::remote`) come from
//!   the same handler, which serves nothing else.
//! - A user content filter (the same rule list as macOS, `crate::rules`)
//!   blocks every load that is not Zmail's scheme or a `data:`/`cid:`
//!   image, and the engine's only network proxy is a black hole
//!   (`linux_proxy`), so nothing reaches the network even past the filter.
//! - Navigation is allowed for exactly one URL, the page Zmail loads. Link
//!   clicks are cancelled and reported; no new windows, downloads,
//!   permissions, authentication, file choosers, printing or drops; the
//!   context menu keeps only Copy. WebGL, WebAudio, media, local storage
//!   and the page cache are off.
//! - Every view shares one ephemeral website data manager: no cookies,
//!   cache or storage on disk.

#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong, c_void},
    path::PathBuf,
    ptr,
    rc::Rc,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::Duration,
};

use futures_channel::mpsc::UnboundedSender;
use libloading::Library;

use crate::document::{
    CID_SCHEME, InlineResource, MAIL_SCHEME, SchemeRequest, WebDocument, content_security_policy,
    page_html, page_token, page_url, parse_scheme_request,
};
use crate::platform::{linux_proxy::BlackHole, linux_sandbox};
use crate::remote::{FetchResult, RemoteError, RemoteFetcher};
use crate::rules::content_rule_list;
use crate::script::{MAX_HEIGHT, MEASURE_SCRIPT, Measurement};
use crate::{WebEvent, debug_log};

/// The oldest WebKitGTK Zmail uses. Raise it as WebKitGTK security
/// advisories (WSA) land: distributions lag upstream fixes by weeks, and a
/// system below this keeps the text view.
pub const MIN_VERSION: (u32, u32, u32) = (2, 52, 0);

/// The library loaded at runtime (WebKitGTK 4.1: GTK 3, libsoup 3).
const LIBRARY: &str = "libwebkit2gtk-4.1.so.0";

/// The isolated script world Zmail's own scripts run in.
const WORLD: &CStr = c"zmail";

/// The tallest a view may be in device pixels: X11 windows and cairo
/// surfaces stop at 32767.
pub const MAX_DEVICE_HEIGHT: f64 = 32_000.;

/// The tallest page an offscreen view snapshots, in device pixels (each
/// snapshot holds the whole page, 4 bytes a pixel).
pub const MAX_SNAPSHOT_HEIGHT: f64 = 16_000.;

/// The height of an offscreen view, in device pixels: about a screen.
const OFFSCREEN_HEIGHT: c_int = 2_048;

/// How long the engine thread may take to start GTK.
const START_TIMEOUT: Duration = Duration::from_secs(15);

/// Where an embedded view waits out of sight: this far left in its
/// parent, in device pixels (X11 coordinates are 16-bit).
const HIDDEN_AT: c_int = -30_000;

/// How often remote image fetches are polled while any is waiting, in ms.
const FETCH_POLL_MS: c_uint = 30;

/// Zmail's repair of inline images, run in the isolated world before a page
/// is measured. WebKitGTK now and then shows a `cid:` image that Zmail's
/// scheme handler served as broken (a few percent of loads in CI, with no
/// error from the engine); asking for it again loads it. The same URL would
/// come from WebKit's cache, so the retry adds a query, which Zmail's
/// scheme handler ignores (`parse_scheme_request`). Each broken `cid:`
/// image is asked for at most twice; the script returns how many images are
/// being asked for again or are still loading.
const REPAIR_SCRIPT: &str = "(() => { let waiting = 0; \
    for (const image of document.images) { \
        const source = image.getAttribute('src') || ''; \
        if (!image.complete) { waiting++; continue; } \
        const tries = Number(image.dataset.zmailTries || 0); \
        if (image.naturalWidth === 0 && /^cid:/i.test(source) && tries < 2) { \
            image.dataset.zmailTries = String(tries + 1); \
            image.src = source.split('?')[0] + '?zmail-retry=' + (tries + 1); waiting++; } } \
    return String(waiting); })()";

/// How many times [`REPAIR_SCRIPT`] runs before the page is measured
/// anyway, and how long it waits between runs.
const REPAIR_ROUNDS: u32 = 5;
const REPAIR_DELAY_MS: c_uint = 200;

/// The page loaded before any mail, to start (and check) a web process.
const WARMUP_HTML: &str =
    "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body></body></html>";

// ---------------------------------------------------------------------------
// The C API
// ---------------------------------------------------------------------------

type Ptr = *mut c_void;
type GType = usize;
type SourceFunc = unsafe extern "C" fn(Ptr) -> c_int;
type AsyncReady = unsafe extern "C" fn(Ptr, Ptr, Ptr);
type SchemeHandler = unsafe extern "C" fn(Ptr, Ptr);
type GCallback = unsafe extern "C" fn();

/// `GError`.
#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *mut c_char,
}

/// `GdkRGBA`.
#[repr(C)]
struct GdkRgba {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}

const FALSE: c_int = 0;
const GTK_WINDOW_POPUP: c_int = 1;
const GDK_SCROLL_UP: c_int = 0;
const GDK_SCROLL_DOWN: c_int = 1;
const GDK_SHIFT_MASK: c_uint = 1;
const GDK_CONTROL_MASK: c_uint = 1 << 2;
const TRUE: c_int = 1;
const G_SOURCE_REMOVE: c_int = 0;
const G_SOURCE_CONTINUE: c_int = 1;
const G_IO_ERROR_NOT_FOUND: c_int = 1;
const SOUP_MESSAGE_HEADERS_RESPONSE: c_int = 1;
const WEBKIT_CACHE_MODEL_DOCUMENT_VIEWER: c_int = 0;
const WEBKIT_NETWORK_PROXY_MODE_CUSTOM: c_int = 2;
const WEBKIT_HARDWARE_ACCELERATION_POLICY_NEVER: c_int = 2;
const WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION: c_int = 0;
const WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION: c_int = 1;
const WEBKIT_NAVIGATION_TYPE_LINK_CLICKED: c_int = 0;
const WEBKIT_NAVIGATION_TYPE_OTHER: c_int = 5;
const WEBKIT_LOAD_FINISHED: c_int = 3;
const WEBKIT_CONTEXT_MENU_ACTION_COPY: c_int = 14;
const WEBKIT_SNAPSHOT_REGION_FULL_DOCUMENT: c_int = 1;
const WEBKIT_SNAPSHOT_OPTIONS_INCLUDE_SELECTION_HIGHLIGHTING: c_int = 1;
const CAIRO_FORMAT_ARGB32: c_int = 0;
const CAIRO_FORMAT_RGB24: c_int = 1;
const WEBKIT_NETWORK_ERROR_CANCELLED: c_int = 302;
const WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE: c_int = 102;

/// Declares a table of C functions: one function pointer per function, all
/// looked up in WebKitGTK's library (`dlsym` on it also finds the symbols
/// of the libraries it links: GTK, GDK, GLib, GObject, GIO, libsoup,
/// JavaScriptCore, Xlib). Variadic functions are listed with their whole
/// type.
macro_rules! api {
    (
        $(#[$meta:meta])* $api:ident {
            $($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*
        }
        $(variadic { $($vname:ident: $vty:ty;)* })?
    ) => {
        $(#[$meta])*
        #[allow(non_snake_case)]
        struct $api {
            $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            $($($vname: $vty,)*)?
        }

        impl $api {
            /// Looks every function up in `library`.
            ///
            /// # Safety
            ///
            /// `library` must be WebKitGTK 4.1, whose functions (and those
            /// of the libraries it links) have the signatures declared
            /// here, taken from their C headers. It is never unloaded, so
            /// the pointers stay valid.
            unsafe fn load(library: &'static Library) -> Result<Self, String> {
                // SAFETY: the caller guarantees each symbol has the declared
                // signature and stays loaded.
                unsafe {
                    Ok(Self {
                        $($name: *library
                            .get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                                concat!(stringify!($name), "\0").as_bytes(),
                            )
                            .map_err(|error| format!("{}: {error}", stringify!($name)))?,)*
                        $($($vname: *library
                            .get::<$vty>(concat!(stringify!($vname), "\0").as_bytes())
                            .map_err(|error| format!("{}: {error}", stringify!($vname)))?,)*)?
                    })
                }
            }
        }
    };
}

api! {
    /// WebKitGTK and the libraries it links, loaded at runtime.
    Api {
    // GLib, GObject, GIO
    g_main_context_default: fn() -> Ptr;
    g_main_context_acquire: fn(Ptr) -> c_int;
    g_main_context_invoke: fn(Ptr, SourceFunc, Ptr);
    g_main_loop_new: fn(Ptr, c_int) -> Ptr;
    g_main_loop_run: fn(Ptr);
    g_main_loop_quit: fn(Ptr);
    g_timeout_add: fn(c_uint, SourceFunc, Ptr) -> c_uint;
    g_free: fn(Ptr);
    g_bytes_new: fn(*const c_void, usize) -> Ptr;
    g_bytes_unref: fn(Ptr);
    g_error_free: fn(*mut GError);
    g_error_new_literal: fn(u32, c_int, *const c_char) -> *mut GError;
    g_io_error_quark: fn() -> u32;
    g_object_ref: fn(Ptr) -> Ptr;
    g_object_unref: fn(Ptr);
    g_signal_connect_data: fn(Ptr, *const c_char, GCallback, Ptr, Ptr, c_int) -> c_ulong;
    g_memory_input_stream_new_from_bytes: fn(Ptr) -> Ptr;
    // GTK 3
    gtk_disable_setlocale: fn();
    gdk_set_allowed_backends: fn(*const c_char);
    gtk_init_check: fn(*mut c_int, *mut c_void) -> c_int;
    gtk_offscreen_window_new: fn() -> Ptr;
    gtk_window_resize: fn(Ptr, c_int, c_int);
    gtk_widget_set_size_request: fn(Ptr, c_int, c_int);
    gtk_container_add: fn(Ptr, Ptr);
    gtk_widget_show_all: fn(Ptr);
    gtk_widget_destroy: fn(Ptr);
    gtk_drag_dest_unset: fn(Ptr);
    gtk_window_new: fn(c_int) -> Ptr;
    gtk_window_move: fn(Ptr, c_int, c_int);
    gtk_widget_realize: fn(Ptr);
    gtk_widget_get_window: fn(Ptr) -> Ptr;
    gtk_widget_get_screen: fn(Ptr) -> Ptr;
    gtk_widget_set_visual: fn(Ptr, Ptr);
    gdk_screen_get_rgba_visual: fn(Ptr) -> Ptr;
    gdk_display_get_default: fn() -> Ptr;
    gdk_event_get_scroll_deltas: fn(Ptr, *mut f64, *mut f64) -> c_int;
    gdk_event_get_scroll_direction: fn(Ptr, *mut c_int) -> c_int;
    gdk_event_get_coords: fn(Ptr, *mut f64, *mut f64) -> c_int;
    gdk_event_get_state: fn(Ptr, *mut c_uint) -> c_int;
    // WebKitGTK
    webkit_get_major_version: fn() -> c_uint;
    webkit_get_minor_version: fn() -> c_uint;
    webkit_get_micro_version: fn() -> c_uint;
    webkit_network_error_quark: fn() -> u32;
    webkit_policy_error_quark: fn() -> u32;
    webkit_website_data_manager_new_ephemeral: fn() -> Ptr;
    webkit_website_data_manager_set_network_proxy_settings: fn(Ptr, c_int, Ptr);
    webkit_network_proxy_settings_new: fn(*const c_char, *const *const c_char) -> Ptr;
    webkit_network_proxy_settings_free: fn(Ptr);
    webkit_web_context_new_with_website_data_manager: fn(Ptr) -> Ptr;
    webkit_web_context_set_sandbox_enabled: fn(Ptr, c_int);
    webkit_web_context_get_sandbox_enabled: fn(Ptr) -> c_int;
    webkit_web_context_set_cache_model: fn(Ptr, c_int);
    webkit_web_context_register_uri_scheme: fn(Ptr, *const c_char, SchemeHandler, Ptr, Ptr);
    webkit_user_content_filter_store_new: fn(*const c_char) -> Ptr;
    webkit_user_content_filter_store_save: fn(Ptr, *const c_char, Ptr, Ptr, AsyncReady, Ptr);
    webkit_user_content_filter_store_save_finish: fn(Ptr, Ptr, *mut *mut GError) -> Ptr;
    webkit_user_content_manager_new: fn() -> Ptr;
    webkit_user_content_manager_add_filter: fn(Ptr, Ptr);
    webkit_settings_new: fn() -> Ptr;
    webkit_web_view_get_type: fn() -> GType;
    webkit_web_view_load_uri: fn(Ptr, *const c_char);
    webkit_web_view_stop_loading: fn(Ptr);
    webkit_web_view_set_zoom_level: fn(Ptr, f64);
    webkit_web_view_set_background_color: fn(Ptr, *const GdkRgba);
    webkit_web_view_evaluate_javascript: fn(Ptr, *const c_char, isize, *const c_char, *const c_char, Ptr, AsyncReady, Ptr);
    webkit_web_view_evaluate_javascript_finish: fn(Ptr, Ptr, *mut *mut GError) -> Ptr;
    webkit_web_view_execute_editing_command: fn(Ptr, *const c_char);
    webkit_web_view_get_snapshot: fn(Ptr, c_int, c_int, Ptr, AsyncReady, Ptr);
    webkit_web_view_get_snapshot_finish: fn(Ptr, Ptr, *mut *mut GError) -> Ptr;
    webkit_policy_decision_use: fn(Ptr);
    webkit_policy_decision_ignore: fn(Ptr);
    webkit_navigation_policy_decision_get_navigation_action: fn(Ptr) -> Ptr;
    webkit_navigation_action_get_navigation_type: fn(Ptr) -> c_int;
    webkit_navigation_action_get_request: fn(Ptr) -> Ptr;
    webkit_uri_request_get_uri: fn(Ptr) -> *const c_char;
    webkit_response_policy_decision_get_response: fn(Ptr) -> Ptr;
    webkit_response_policy_decision_is_main_frame_main_resource: fn(Ptr) -> c_int;
    webkit_response_policy_decision_is_mime_type_supported: fn(Ptr) -> c_int;
    webkit_uri_response_get_uri: fn(Ptr) -> *const c_char;
    webkit_permission_request_deny: fn(Ptr);
    webkit_authentication_request_cancel: fn(Ptr);
    webkit_file_chooser_request_cancel: fn(Ptr);
    webkit_download_cancel: fn(Ptr);
    webkit_context_menu_get_n_items: fn(Ptr) -> c_uint;
    webkit_context_menu_get_item_at_position: fn(Ptr, c_uint) -> Ptr;
    webkit_context_menu_item_get_stock_action: fn(Ptr) -> c_int;
    webkit_context_menu_remove: fn(Ptr, Ptr);
    webkit_uri_scheme_request_get_uri: fn(Ptr) -> *const c_char;
    webkit_uri_scheme_request_get_web_view: fn(Ptr) -> Ptr;
    webkit_uri_scheme_request_finish_error: fn(Ptr, *mut GError);
    webkit_uri_scheme_request_finish_with_response: fn(Ptr, Ptr);
    webkit_uri_scheme_response_new: fn(Ptr, i64) -> Ptr;
    webkit_uri_scheme_response_set_content_type: fn(Ptr, *const c_char);
    webkit_uri_scheme_response_set_http_headers: fn(Ptr, Ptr);
    // cairo
    cairo_surface_flush: fn(Ptr);
    cairo_surface_destroy: fn(Ptr);
    cairo_image_surface_get_format: fn(Ptr) -> c_int;
    cairo_image_surface_get_data: fn(Ptr) -> *mut u8;
    cairo_image_surface_get_width: fn(Ptr) -> c_int;
    cairo_image_surface_get_height: fn(Ptr) -> c_int;
    cairo_image_surface_get_stride: fn(Ptr) -> c_int;
    // JavaScriptCore, libsoup
    jsc_value_to_string: fn(Ptr) -> *mut c_char;
    soup_message_headers_new: fn(c_int) -> Ptr;
    soup_message_headers_append: fn(Ptr, *const c_char, *const c_char);
    }
    variadic {
        g_object_new: unsafe extern "C" fn(GType, *const c_char, ...) -> Ptr;
        g_object_set: unsafe extern "C" fn(Ptr, *const c_char, ...);
    }
}

api! {
    /// GDK's X11 backend and Xlib, loaded only when GPUI runs on X11.
    X11Api {
        gdk_x11_display_get_xdisplay: fn(Ptr) -> Ptr;
        gdk_x11_display_set_window_scale: fn(Ptr, c_int);
        gdk_x11_window_get_xid: fn(Ptr) -> c_ulong;
        XReparentWindow: fn(Ptr, c_ulong, c_ulong, c_int, c_int) -> c_int;
        XFlush: fn(Ptr) -> c_int;
        XSetErrorHandler: fn(Option<XErrorHandler>) -> Option<XErrorHandler>;
    }
}

/// Xlib's `XErrorEvent`.
#[repr(C)]
struct XErrorEvent {
    kind: c_int,
    display: Ptr,
    resource: c_ulong,
    serial: c_ulong,
    error_code: u8,
    request_code: u8,
    minor_code: u8,
}

type XErrorHandler = unsafe extern "C" fn(Ptr, *mut XErrorEvent) -> c_int;

/// Xlib's `BadWindow` and `BadDrawable`.
const BAD_WINDOW: u8 = 3;
const BAD_DRAWABLE: u8 = 9;

/// A C string for `text`, with any NUL removed (none of Zmail's strings
/// have one, and a URL or header must not be cut short).
fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

/// Copies a C string WebKit owns.
///
/// # Safety
///
/// `text` must be null or a valid NUL-terminated string.
unsafe fn copy_c_str(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: non-null and NUL-terminated by the caller's guarantee.
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// Takes an error WebKit returned: its domain, code and message, freeing it.
fn take_error(api: &Api, error: *mut GError) -> Option<(u32, c_int, String)> {
    if error.is_null() {
        return None;
    }
    // SAFETY: a non-null GError from a `_finish` call or signal is a valid
    // GError whose message is a valid string; we own it (transfer full)
    // and free it once.
    unsafe {
        let taken = ((*error).domain, (*error).code, copy_c_str((*error).message));
        (api.g_error_free)(error);
        Some(taken)
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// The display server GPUI uses, which GTK must use too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayServer {
    /// X11 (or XWayland).
    X11,
    /// Wayland.
    Wayland,
}

/// GLib's default main context, owned by the engine thread.
struct MainContext(Ptr);

// SAFETY: a GMainContext is thread-safe; the only call made on it from
// other threads is `g_main_context_invoke`, which GLib documents as safe
// from any thread.
unsafe impl Send for MainContext {}
// SAFETY: as above.
unsafe impl Sync for MainContext {}

/// The running engine, shared by every window.
struct Engine {
    api: &'static Api,
    context: MainContext,
    commands: Mutex<Sender<Command>>,
    proxy: BlackHole,
}

/// WebKitGTK's library, loaded once and never unloaded.
static LIBRARY_HANDLE: OnceLock<Library> = OnceLock::new();
static API: OnceLock<Result<Api, String>> = OnceLock::new();
static X11_API: OnceLock<Result<X11Api, String>> = OnceLock::new();
static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();
static NEXT_VIEW: AtomicU64 = AtomicU64::new(1);
/// The engine stopped for good ([`fail`]); [`FAILURE`] says why.
static FAILED: AtomicBool = AtomicBool::new(false);
static FAILURE: Mutex<Option<String>> = Mutex::new(None);
/// Web processes confirmed sandboxed by the last check.
static VERIFIED: AtomicUsize = AtomicUsize::new(0);

/// Loads WebKitGTK and checks its version, without starting anything.
fn api() -> Result<&'static Api, String> {
    API.get_or_init(|| {
        // SAFETY: loading WebKitGTK runs its libraries' initialisers, which
        // only set up their own state; nothing of the engine starts until
        // it is called.
        let library = unsafe { Library::new(LIBRARY) }.map_err(|error| {
            debug_log(format_args!("{LIBRARY}: {error}"));
            format!("WebKitGTK 4.1 is not installed ({LIBRARY} could not be loaded)")
        })?;
        let library = LIBRARY_HANDLE.get_or_init(|| library);
        // SAFETY: the library is WebKitGTK 4.1, which `api!` describes, and
        // it is never unloaded.
        let api = unsafe { Api::load(library) }
            .map_err(|missing| format!("{LIBRARY} lacks a function Zmail needs: {missing}"))?;
        // SAFETY: the version functions take no arguments and only return
        // constants.
        let version = unsafe {
            (
                (api.webkit_get_major_version)(),
                (api.webkit_get_minor_version)(),
                (api.webkit_get_micro_version)(),
            )
        };
        if version < MIN_VERSION {
            return Err(format!(
                "WebKitGTK {}.{}.{} is older than {}.{}.{}, the oldest Zmail uses",
                version.0, version.1, version.2, MIN_VERSION.0, MIN_VERSION.1, MIN_VERSION.2
            ));
        }
        debug_log(format_args!(
            "WebKitGTK {}.{}.{} loaded",
            version.0, version.1, version.2
        ));
        Ok(api)
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Starts the engine for `display` if it is not running yet: loads
/// WebKitGTK, checks the sandbox can start, and starts GTK on the engine
/// thread. Returns why the web renderer cannot be used, if it cannot.
pub fn start(display: DisplayServer) -> Result<(), String> {
    ENGINE
        .get_or_init(|| start_engine(display))
        .as_ref()
        .map_err(Clone::clone)?;
    match stopped() {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// Why the engine stopped, if it was started and stopped.
pub fn failure() -> Option<String> {
    match ENGINE.get()? {
        Ok(_) => stopped(),
        Err(reason) => Some(reason.clone()),
    }
}

/// How many connections reached the engine's black-hole proxy (none
/// should: the content filter stops loads first).
pub fn blocked_connections() -> usize {
    engine().map_or(0, |engine| engine.proxy.connections())
}

/// How many web processes have been confirmed to run sandboxed.
pub fn verified_web_processes() -> usize {
    VERIFIED.load(Ordering::SeqCst)
}

/// Why the running engine stopped, if it did.
fn stopped() -> Option<String> {
    if !FAILED.load(Ordering::SeqCst) {
        return None;
    }
    FAILURE.lock().ok().and_then(|failure| failure.clone())
}

fn engine() -> Option<&'static Engine> {
    ENGINE.get()?.as_ref().ok()
}

fn start_engine(display: DisplayServer) -> Result<Engine, String> {
    let api = api()?;
    // Nothing below may spawn a web process before the sandbox checks.
    if let Some(reason) = linux_sandbox::this_environment_refusal() {
        return Err(reason);
    }
    linux_sandbox::probe_bubblewrap()?;
    let proxy = BlackHole::start()
        .map_err(|error| format!("the web engine's network wall did not start: {error}"))?;
    let (commands, receiver) = mpsc::channel();
    // SAFETY: returns GLib's default main context, a process-wide object
    // that is never freed.
    let context = MainContext(unsafe { (api.g_main_context_default)() });
    let (started, wait) = mpsc::channel();
    let proxy_uri = proxy.uri();
    thread::Builder::new()
        .name("zmail-webkit".to_owned())
        .spawn(move || engine_thread(api, display, proxy_uri, receiver, started))
        .map_err(|error| format!("the web engine thread did not start: {error}"))?;
    wait.recv_timeout(START_TIMEOUT)
        .map_err(|_| "the web engine did not start in time".to_owned())??;
    Ok(Engine {
        api,
        context,
        commands: Mutex::new(commands),
        proxy,
    })
}

/// Sends `command` to the engine thread and wakes it.
fn send(command: Command) {
    let Some(engine) = engine() else {
        return;
    };
    let sent = engine
        .commands
        .lock()
        .map(|commands| commands.send(command).is_ok())
        .unwrap_or(false);
    if sent {
        // SAFETY: the context is GLib's default context, valid forever;
        // `g_main_context_invoke` may be called from any thread and runs
        // `drain_commands` on the thread that owns the context (the engine
        // thread). The data pointer is unused.
        unsafe {
            (engine.api.g_main_context_invoke)(engine.context.0, drain_commands, ptr::null_mut())
        };
    }
}

/// Stops the engine for good: every view is told why, and destroyed.
fn fail(reason: String) {
    eprintln!("zmail: web renderer: stopped: {reason}");
    if let Ok(mut failure) = FAILURE.lock() {
        failure.get_or_insert_with(|| reason.clone());
    }
    FAILED.store(true, Ordering::SeqCst);
    let views: Vec<Rc<View>> = with_thread(|thread| {
        thread.ready = false;
        thread.queued.clear();
        thread.views.drain().map(|(_, view)| view).collect()
    })
    .unwrap_or_default();
    for view in views {
        view.emit(WebEvent::LoadFailed(reason.clone()));
        view.destroy();
    }
}

// ---------------------------------------------------------------------------
// The engine thread
// ---------------------------------------------------------------------------

/// Something the engine thread is asked to do.
enum Command {
    Create(Box<CreateView>),
    Resize {
        id: u64,
        size: ViewSize,
    },
    /// Where an embedded view sits in its parent window, in device pixels,
    /// and whether it shows.
    Move {
        id: u64,
        x: c_int,
        y: c_int,
        visible: bool,
    },
    Editing {
        id: u64,
        command: &'static CStr,
    },
    Evaluate {
        id: u64,
        script: String,
        tag: u64,
    },
    /// An offscreen view's page changed (a selection): snapshot it again.
    Snapshot {
        id: u64,
    },
    Destroy {
        id: u64,
    },
    /// Destroys every view and stops the engine thread's main loop.
    Shutdown {
        done: Sender<()>,
    },
    /// Destroys views now and says so, for a parent window about to go.
    DestroyNow {
        ids: Vec<u64>,
        done: Sender<()>,
    },
    /// Tests: a plain X11 window to embed views in.
    TestParent {
        width: c_int,
        height: c_int,
        done: Sender<Result<u64, String>>,
    },
}

/// A new view.
struct CreateView {
    id: u64,
    page: ServedPage,
    size: ViewSize,
    background: Option<u32>,
    /// The X11 window to embed the view in; offscreen when `None`.
    parent: Option<u64>,
    /// Where it sits in `parent` and whether it shows, once known.
    position: Option<(c_int, c_int, bool)>,
    events: UnboundedSender<ViewEvent>,
}

/// The page a view serves.
struct ServedPage {
    url: String,
    html: Vec<u8>,
    policy: String,
    resources: HashMap<String, InlineResource>,
    /// Remote images Zmail may fetch, by the `n` of `zmail-mail://remote/<n>`.
    remote: Vec<Option<String>>,
}

/// The engine thread's state.
struct EngineThread {
    api: &'static Api,
    receiver: Receiver<Command>,
    /// The `WebKitWebContext` every view shares.
    context: Ptr,
    /// The compiled `WebKitUserContentFilter`, once ready.
    filter: Option<Ptr>,
    /// Mail may be loaded: the filter is compiled and a web process was
    /// seen sandboxed.
    ready: bool,
    views: HashMap<u64, Rc<View>>,
    /// Views asked for before the engine was ready.
    queued: Vec<CreateView>,
    /// Remote image fetches whose scheme requests are waiting.
    pending: Vec<PendingFetch>,
    polling: bool,
    /// The running main loop, which `Command::Shutdown` quits.
    main_loop: Ptr,
    /// Told once the main loop has stopped.
    stopped: Option<Sender<()>>,
}

/// A remote image being fetched for a scheme request.
struct PendingFetch {
    view: u64,
    /// A `WebKitURISchemeRequest`, referenced.
    request: Ptr,
    result: Receiver<FetchResult>,
}

thread_local! {
    static THREAD: RefCell<Option<EngineThread>> = const { RefCell::new(None) };
}

/// Runs `f` on the engine thread's state (none on other threads). Never
/// call into WebKit inside `f`: WebKit may call back into this module.
fn with_thread<R>(f: impl FnOnce(&mut EngineThread) -> R) -> Option<R> {
    THREAD.with(|state| state.try_borrow_mut().ok()?.as_mut().map(f))
}

fn view_by_id(id: u64) -> Option<Rc<View>> {
    with_thread(|thread| thread.views.get(&id).cloned()).flatten()
}

fn engine_thread(
    api: &'static Api,
    display: DisplayServer,
    proxy: String,
    receiver: Receiver<Command>,
    started: Sender<Result<(), String>>,
) {
    // SAFETY: called once, on this thread, before any other GTK call: GTK
    // must not change the process's locale (GPUI's), only use the display
    // server GPUI uses, and run on this thread, which owns GLib's default
    // main context from here on.
    let context = unsafe {
        let main_context = (api.g_main_context_default)();
        if (api.g_main_context_acquire)(main_context) == FALSE {
            let _ = started.send(Err("GLib's main context is owned by another thread".into()));
            return;
        }
        (api.gtk_disable_setlocale)();
        (api.gdk_set_allowed_backends)(match display {
            DisplayServer::X11 => c"x11".as_ptr(),
            DisplayServer::Wayland => c"wayland".as_ptr(),
        });
        if (api.gtk_init_check)(ptr::null_mut(), ptr::null_mut()) == FALSE {
            let _ = started.send(Err("GTK could not open the display".into()));
            return;
        }
        match display {
            DisplayServer::X11 => match x11_display(api) {
                Ok(x11) => X11.with(|cell| cell.set(Some(x11))),
                Err(reason) => {
                    let _ = started.send(Err(reason));
                    return;
                }
            },
            DisplayServer::Wayland => {}
        }
        match web_context(api, &proxy) {
            Ok(context) => context,
            Err(reason) => {
                let _ = started.send(Err(reason));
                return;
            }
        }
    };
    THREAD.with(|state| {
        *state.borrow_mut() = Some(EngineThread {
            api,
            receiver,
            context,
            filter: None,
            ready: false,
            views: HashMap::new(),
            queued: Vec::new(),
            pending: Vec::new(),
            polling: false,
            main_loop: ptr::null_mut(),
            stopped: None,
        });
    });
    compile_filter(api);
    let _ = started.send(Ok(()));
    debug_log(format_args!("engine thread running ({display:?})"));
    // SAFETY: runs GLib's default main loop on the thread that acquired
    // the default context, until `shutdown` quits it.
    unsafe {
        let main_loop = (api.g_main_loop_new)(ptr::null_mut(), FALSE);
        with_thread(|thread| thread.main_loop = main_loop);
        (api.g_main_loop_run)(main_loop);
    }
    debug_log(format_args!("engine thread stopped"));
    let done = with_thread(|thread| thread.stopped.take()).flatten();
    if let Some(done) = done {
        let _ = done.send(());
    }
}

/// GTK's X11 connection on the engine thread.
#[derive(Clone, Copy)]
struct X11Display {
    api: &'static X11Api,
    /// GDK's Xlib `Display`.
    display: Ptr,
}

thread_local! {
    static X11: Cell<Option<X11Display>> = const { Cell::new(None) };
}

/// The handler Xlib called for errors before Zmail's (GDK's, which aborts
/// on any error it did not expect).
static PREVIOUS_X_ERROR_HANDLER: OnceLock<Option<XErrorHandler>> = OnceLock::new();

/// Loads GDK's X11 functions and sets GTK's X11 connection up for views
/// embedded in GPUI's window: no GDK window scaling (Zmail zooms pages to
/// GPUI's scale itself), and errors about windows that are already gone
/// ignored. GPUI destroys its native surface's window (and so the GTK
/// windows inside it) when the reader goes away, possibly before GTK
/// destroys its own, and GDK would abort on the `BadWindow` that follows.
///
/// # Safety
///
/// Must run on the engine thread, right after GTK opened the X11 display.
unsafe fn x11_display(api: &Api) -> Result<X11Display, String> {
    let library = LIBRARY_HANDLE
        .get()
        .ok_or_else(|| "WebKitGTK is not loaded".to_owned())?;
    let x11 = X11_API
        .get_or_init(|| {
            // SAFETY: the library is WebKitGTK 4.1 built with GTK's X11
            // backend, whose functions `X11Api` declares; it is never
            // unloaded.
            unsafe { X11Api::load(library) }
                .map_err(|missing| format!("GTK lacks its X11 backend: {missing}"))
        })
        .as_ref()
        .map_err(Clone::clone)?;
    // SAFETY: GTK is initialised on X11, so the default display is a
    // GdkX11Display with a live Xlib connection. The error handler is a
    // static function that never unwinds; it is installed once.
    unsafe {
        let gdk_display = (api.gdk_display_get_default)();
        if gdk_display.is_null() {
            return Err("GTK has no display".into());
        }
        (x11.gdk_x11_display_set_window_scale)(gdk_display, 1);
        let display = (x11.gdk_x11_display_get_xdisplay)(gdk_display);
        PREVIOUS_X_ERROR_HANDLER.get_or_init(|| (x11.XSetErrorHandler)(Some(on_x_error)));
        Ok(X11Display { api: x11, display })
    }
}

/// Ignores `BadWindow` and `BadDrawable` (a window GPUI already destroyed);
/// hands every other error to GDK's handler.
unsafe extern "C" fn on_x_error(display: Ptr, event: *mut XErrorEvent) -> c_int {
    // SAFETY: Xlib passes a valid error event for the call.
    let code = unsafe { (*event).error_code };
    if code == BAD_WINDOW || code == BAD_DRAWABLE {
        debug_log(format_args!(
            "X11 error {code} on a window that is gone, ignored"
        ));
        return 0;
    }
    match PREVIOUS_X_ERROR_HANDLER.get().copied().flatten() {
        // SAFETY: the previous handler was installed by GDK for this
        // display and takes the same arguments.
        Some(previous) => unsafe { previous(display, event) },
        None => 0,
    }
}

/// The shared web context: ephemeral data, the sandbox on, Zmail's
/// schemes, the black-hole proxy, downloads cancelled.
///
/// # Safety
///
/// Must run on the engine thread after GTK is initialised.
unsafe fn web_context(api: &Api, proxy: &str) -> Result<Ptr, String> {
    let proxy = c_string(proxy);
    // SAFETY: plain constructor and setter calls on objects created here;
    // the sandbox is enabled before any web process exists (none has been
    // started yet); the scheme handler is a static function that never
    // unwinds; the proxy settings are copied by WebKit and freed after.
    unsafe {
        let data = (api.webkit_website_data_manager_new_ephemeral)();
        let settings = (api.webkit_network_proxy_settings_new)(proxy.as_ptr(), ptr::null());
        (api.webkit_website_data_manager_set_network_proxy_settings)(
            data,
            WEBKIT_NETWORK_PROXY_MODE_CUSTOM,
            settings,
        );
        (api.webkit_network_proxy_settings_free)(settings);
        let context = (api.webkit_web_context_new_with_website_data_manager)(data);
        (api.g_object_unref)(data);
        (api.webkit_web_context_set_sandbox_enabled)(context, TRUE);
        if (api.webkit_web_context_get_sandbox_enabled)(context) == FALSE {
            return Err("the web engine did not enable its sandbox".into());
        }
        (api.webkit_web_context_set_cache_model)(context, WEBKIT_CACHE_MODEL_DOCUMENT_VIEWER);
        for scheme in [MAIL_SCHEME, CID_SCHEME] {
            let scheme = c_string(scheme);
            (api.webkit_web_context_register_uri_scheme)(
                context,
                scheme.as_ptr(),
                on_scheme_request,
                ptr::null_mut(),
                ptr::null_mut(),
            );
        }
        connect(
            api,
            context,
            c"download-started",
            on_download_started as unsafe extern "C" fn(Ptr, Ptr, Ptr),
            0,
        );
        Ok(context)
    }
}

/// Connects `handler` to `signal` of `instance`, with `id` as its data.
///
/// # Safety
///
/// `instance` must be a live GObject with `signal`, and `handler` a
/// function with that signal's C signature (the instance first and the
/// data pointer last) that never unwinds.
unsafe fn connect<F: Copy>(api: &Api, instance: Ptr, signal: &CStr, handler: F, id: u64) {
    const { assert!(size_of::<F>() == size_of::<GCallback>()) };
    // SAFETY: `F` is a function pointer (the assertion checks its size);
    // GObject casts it back to the signal's signature, which the caller
    // guarantees, before calling it.
    unsafe {
        let callback = std::mem::transmute_copy::<F, GCallback>(&handler);
        (api.g_signal_connect_data)(
            instance,
            signal.as_ptr(),
            callback,
            id as usize as Ptr,
            ptr::null_mut(),
            0,
        );
    }
}

/// The view id a signal's data pointer carries.
fn id_of(data: Ptr) -> u64 {
    data as usize as u64
}

/// Compiles the content filter, then starts the warm-up view.
fn compile_filter(api: &Api) {
    let rules = content_rule_list();
    let directory = filter_directory();
    if let Err(error) = std::fs::create_dir_all(&directory) {
        fail(format!(
            "the content filter store could not be created: {error}"
        ));
        return;
    }
    let directory = c_string(&directory.to_string_lossy());
    // SAFETY: on the engine thread; the store copies its path; `g_bytes_new`
    // copies the rules; the callback runs once, on this thread.
    unsafe {
        let store = (api.webkit_user_content_filter_store_new)(directory.as_ptr());
        let bytes = (api.g_bytes_new)(rules.as_ptr().cast(), rules.len());
        let identifier = c_string(&crate::rules::rule_list_identifier(&rules));
        (api.webkit_user_content_filter_store_save)(
            store,
            identifier.as_ptr(),
            bytes,
            ptr::null_mut(),
            on_filter_compiled,
            ptr::null_mut(),
        );
        (api.g_bytes_unref)(bytes);
        (api.g_object_unref)(store);
    }
}

/// Where compiled content filters are kept: a directory of the user's own.
fn filter_directory() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| std::env::temp_dir().join(format!("zmail-{}", std::process::id())));
    base.join("zmail-webkit-filters")
}

unsafe extern "C" fn on_filter_compiled(store: Ptr, result: Ptr, _data: Ptr) {
    let Some(api) = API.get().and_then(|api| api.as_ref().ok()) else {
        return;
    };
    let mut error = ptr::null_mut();
    // SAFETY: `store` and `result` are the objects GIO passes to this
    // callback; the filter (if any) is returned with a reference we keep
    // for the engine's lifetime.
    let filter =
        unsafe { (api.webkit_user_content_filter_store_save_finish)(store, result, &mut error) };
    if let Some((_, _, message)) = take_error(api, error) {
        fail(format!("the content filter did not compile: {message}"));
        return;
    }
    if filter.is_null() {
        fail("the content filter did not compile".into());
        return;
    }
    debug_log(format_args!("content filter compiled"));
    with_thread(|thread| thread.filter = Some(filter));
    // The first web process loads a blank page and is checked before any
    // mail is loaded.
    let (events, _) = futures_channel::mpsc::unbounded();
    create_view(CreateView {
        id: 0,
        page: ServedPage {
            url: page_url(&page_token()),
            html: WARMUP_HTML.as_bytes().to_vec(),
            policy: content_security_policy(true),
            resources: HashMap::new(),
            remote: Vec::new(),
        },
        size: ViewSize {
            width: 100.,
            height: 100.,
            scale: 1.,
        },
        background: None,
        parent: None,
        position: None,
        events,
    });
}

/// The warm-up page loaded: check its web process, then load the mail
/// that is waiting.
fn warmed_up() {
    match linux_sandbox::check_web_processes() {
        Ok(count) => {
            debug_log(format_args!("{count} web process(es) confirmed sandboxed"));
            VERIFIED.store(count, Ordering::SeqCst);
        }
        Err(reason) => {
            fail(reason);
            return;
        }
    }
    if let Some(view) = with_thread(|thread| thread.views.remove(&0)).flatten() {
        view.destroy();
    }
    let queued = with_thread(|thread| {
        thread.ready = true;
        std::mem::take(&mut thread.queued)
    })
    .unwrap_or_default();
    for create in queued {
        create_view(create);
    }
}

/// Runs the commands waiting for the engine thread.
unsafe extern "C" fn drain_commands(_data: Ptr) -> c_int {
    loop {
        let Some(next) = with_thread(|thread| thread.receiver.try_recv()) else {
            return G_SOURCE_REMOVE;
        };
        match next {
            Ok(command) => handle(command),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return G_SOURCE_REMOVE,
        }
    }
}

fn handle(command: Command) {
    match command {
        Command::Create(create) => {
            if let Some(reason) = stopped() {
                let _ = create
                    .events
                    .unbounded_send(ViewEvent::Web(WebEvent::LoadFailed(reason)));
                return;
            }
            let ready = with_thread(|thread| thread.ready).unwrap_or(false);
            if ready {
                create_view(*create);
            } else {
                with_thread(|thread| thread.queued.push(*create));
            }
        }
        Command::Resize { id, size } => {
            if let Some(view) = view_by_id(id) {
                view.resize(size);
            } else {
                with_thread(|thread| {
                    if let Some(queued) = thread.queued.iter_mut().find(|queued| queued.id == id) {
                        queued.size = size;
                    }
                });
            }
        }
        Command::Move { id, x, y, visible } => {
            if let Some(view) = view_by_id(id) {
                view.position.set((x, y));
                view.visible.set(visible);
                view.apply_position();
            } else {
                with_thread(|thread| {
                    if let Some(queued) = thread.queued.iter_mut().find(|queued| queued.id == id) {
                        queued.position = Some((x, y, visible));
                    }
                });
            }
        }
        Command::DestroyNow { ids, done } => {
            for id in ids {
                handle(Command::Destroy { id });
            }
            let _ = done.send(());
        }
        Command::TestParent {
            width,
            height,
            done,
        } => {
            let _ = done.send(test_parent(width, height));
        }
        Command::Editing { id, command } => {
            if let Some(view) = view_by_id(id) {
                // SAFETY: a live web view and a valid editing command name.
                unsafe {
                    (view.api.webkit_web_view_execute_editing_command)(
                        view.web_view,
                        command.as_ptr(),
                    )
                };
            }
        }
        Command::Evaluate { id, script, tag } => {
            if let Some(view) = view_by_id(id) {
                view.evaluate(&script, ScriptPurpose::Caller(tag));
            } else {
                // The view does not exist (yet, or any more).
                let events = with_thread(|thread| {
                    thread
                        .queued
                        .iter()
                        .find(|queued| queued.id == id)
                        .map(|queued| queued.events.clone())
                })
                .flatten();
                if let Some(events) = events {
                    let _ = events.unbounded_send(ViewEvent::Script {
                        tag,
                        result: Err("the web view does not exist yet".into()),
                    });
                }
            }
        }
        Command::Shutdown { done } => {
            let views: Vec<Rc<View>> = with_thread(|thread| {
                thread.ready = false;
                thread.queued.clear();
                thread.views.drain().map(|(_, view)| view).collect()
            })
            .unwrap_or_default();
            for view in views {
                view.destroy();
            }
            FAILED.store(true, Ordering::SeqCst);
            if let Ok(mut failure) = FAILURE.lock() {
                failure.get_or_insert_with(|| "the web engine was shut down".to_owned());
            }
            let main_loop = with_thread(|thread| {
                thread.stopped = Some(done);
                thread.main_loop
            });
            if let (Some(api), Some(main_loop)) = (loaded_api(), main_loop)
                && !main_loop.is_null()
            {
                // SAFETY: the loop this thread is running; quitting it makes
                // `g_main_loop_run` return once this callback is done.
                unsafe { (api.g_main_loop_quit)(main_loop) };
            }
        }
        Command::Snapshot { id } => {
            if let Some(view) = view_by_id(id) {
                view.snapshot();
            }
        }
        Command::Destroy { id } => {
            with_thread(|thread| thread.queued.retain(|queued| queued.id != id));
            if let Some(view) = with_thread(|thread| thread.views.remove(&id)).flatten() {
                view.destroy();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Views (engine thread)
// ---------------------------------------------------------------------------

/// A view's size in GPUI's logical pixels, and the window's scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewSize {
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
    /// Device pixels per logical pixel.
    pub scale: f64,
}

impl ViewSize {
    fn device(self) -> (c_int, c_int) {
        let scale = self.scale.max(0.5);
        (
            (self.width * scale).round().clamp(1., 16_000.) as c_int,
            (self.height * scale).round().clamp(1., MAX_DEVICE_HEIGHT) as c_int,
        )
    }

    /// The tallest page shown at this scale, in logical pixels: below X11's
    /// window limit for an embedded view, and below what a snapshot of the
    /// whole page may cost for an offscreen one.
    fn max_height(self, embedded: bool) -> f64 {
        let device = if embedded {
            MAX_DEVICE_HEIGHT
        } else {
            MAX_SNAPSHOT_HEIGHT
        };
        MAX_HEIGHT.min(device / self.scale.max(0.5)).floor()
    }
}

/// Something a view reports to its owner.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewEvent {
    /// What every platform reports.
    Web(WebEvent),
    /// The wheel turned over an embedded view, vertically: the page does
    /// not scroll itself, the conversation should.
    Scroll {
        /// Where, in logical pixels from the view's top-left corner.
        x: f64,
        /// Where, in logical pixels from the view's top-left corner.
        y: f64,
        /// How far, in lines; positive is down.
        lines: f64,
        /// Control was held (zoom, in some apps).
        control: bool,
    },
    /// A new picture of an offscreen view's whole page, with where its
    /// links are.
    Snapshot(Snapshot),
    /// The result of [`PlatformWebView::evaluate`].
    Script {
        /// The caller's tag.
        tag: u64,
        /// The script's result as a string, or why it failed.
        result: Result<String, String>,
    },
}

/// A picture of an offscreen view's page: strips of it, top to bottom,
/// and its links, all in the view's logical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Device pixels per logical pixel in the tiles.
    pub scale: f64,
    /// The page's zoom: logical pixels per CSS pixel (below 1 for a page
    /// zoomed out to fit).
    pub zoom: f64,
    /// The page, top to bottom.
    pub tiles: Vec<Tile>,
    /// Where the links are.
    pub links: Vec<LinkArea>,
}

/// A strip of a [`Snapshot`], as a BMP image (32-bit, top-down).
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    /// Its top edge, in device pixels from the top of the page.
    pub top: u32,
    /// Its width, in device pixels.
    pub width: u32,
    /// Its height, in device pixels.
    pub height: u32,
    /// The BMP file.
    pub bmp: Vec<u8>,
}

/// A link's box in a [`Snapshot`] (a link that wraps has one per line).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkArea {
    /// The link's URL, as a click on it would report.
    pub url: String,
    /// Left edge, logical pixels from the page's left.
    pub x: f64,
    /// Top edge, logical pixels from the page's top.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// The rows of each snapshot tile, in device pixels.
const TILE_ROWS: usize = 512;

/// Zmail's link hit map, run in the isolated world: one line per box of
/// each link, `left top width height url`, in CSS pixels from the page's
/// top-left corner.
const LINKS_SCRIPT: &str = "Array.from(document.querySelectorAll('a[href]')).flatMap(a => \
    Array.from(a.getClientRects()).map(r => [r.left + scrollX, r.top + scrollY, r.width, \
    r.height, a.href].join(' '))).join('\\n')";

/// Encodes `rows` of 32-bit BGRx pixels (`stride` bytes apart, as cairo
/// lays them out) as a top-down 32-bit BMP.
fn bmp(width: usize, height: usize, stride: usize, rows: &[u8]) -> Vec<u8> {
    const HEADERS: usize = 14 + 40;
    let row_bytes = width * 4;
    let size = HEADERS + row_bytes * height;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(HEADERS as u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    // Negative: rows run top to bottom.
    out.extend_from_slice(&(-(height as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    // BI_RGB, then the image size, resolution and palette (none).
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&((row_bytes * height) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for row in 0..height {
        let start = row * stride;
        match rows.get(start..start + row_bytes) {
            Some(pixels) => out.extend_from_slice(pixels),
            None => out.resize(out.len() + row_bytes, 0),
        }
    }
    out
}

/// Parses [`LINKS_SCRIPT`]'s lines, scaled by `zoom` (CSS pixels to the
/// view's logical pixels).
fn parse_links(text: &str, zoom: f64) -> Vec<LinkArea> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(5, ' ');
            let mut number = || parts.next()?.parse::<f64>().ok();
            let (x, y, width, height) = (number()?, number()?, number()?, number()?);
            let url = parts.next()?.to_owned();
            (width > 0. && height > 0. && !url.is_empty()).then_some(LinkArea {
                url,
                x: x * zoom,
                y: y * zoom,
                width: width * zoom,
                height: height * zoom,
            })
        })
        .collect()
}

/// Why a script runs.
#[derive(Debug, Clone, Copy)]
enum ScriptPurpose {
    Measure,
    /// Inline images that failed are asked for again; then the page is
    /// measured. The number is the round.
    Repair(u32),
    /// The link hit map, after a snapshot.
    Links,
    Caller(u64),
}

/// One web view, on the engine thread.
struct View {
    api: &'static Api,
    id: u64,
    /// The `WebKitWebView`.
    web_view: Ptr,
    /// The window holding it: a `GtkOffscreenWindow`, or a popup window
    /// reparented into GPUI's native surface on X11.
    window: Ptr,
    /// The window is embedded in GPUI's window (X11).
    embedded: bool,
    /// Where an embedded view sits in its parent, in device pixels.
    position: Cell<(c_int, c_int)>,
    /// The host shows it.
    visible: Cell<bool>,
    /// The page has been measured once: until then an embedded view stays
    /// out of sight, so no half-laid-out page flashes.
    revealed: Cell<bool>,
    page: ServedPage,
    events: UnboundedSender<ViewEvent>,
    size: Cell<ViewSize>,
    /// The page finished loading, so it can be measured.
    loaded: Cell<bool>,
    /// The next measurement may zoom the page out to fit its width.
    fit_pending: Cell<bool>,
    /// The zoom fitting the page's width (1 when it fits).
    fit: Cell<f64>,
    destroyed: Cell<bool>,
    /// A snapshot is being taken; another is wanted after it.
    snapshotting: Cell<bool>,
    snapshot_again: Cell<bool>,
    /// The tiles of the snapshot whose links are being found.
    tiles: RefCell<Vec<Tile>>,
}

fn create_view(create: CreateView) {
    let Some((api, context, filter)) =
        with_thread(|thread| (thread.api, thread.context, thread.filter))
            .and_then(|(api, context, filter)| filter.map(|filter| (api, context, filter)))
    else {
        return;
    };
    let CreateView {
        id,
        page,
        size,
        background,
        parent,
        position,
        events,
    } = create;
    let x11 = X11.with(Cell::get);
    let parent = match (parent, x11) {
        (Some(parent), Some(x11)) => Some((parent, x11)),
        (Some(_), None) => {
            let _ = events.unbounded_send(ViewEvent::Web(WebEvent::LoadFailed(
                "the web engine is not on X11".into(),
            )));
            return;
        }
        (None, _) => None,
    };
    let url = c_string(&page.url);
    // SAFETY: on the engine thread, with GTK initialised. Every object is
    // created here; `g_object_new` gets property names with values of the
    // properties' types and a NULL terminator; the window takes the floating
    // web view; signal handlers match their signals (see each one) and get
    // the view id, not a pointer, so a late signal for a destroyed view
    // finds nothing.
    let view = unsafe {
        let content = (api.webkit_user_content_manager_new)();
        (api.webkit_user_content_manager_add_filter)(content, filter);
        let settings = (api.webkit_settings_new)();
        for (name, value) in HARDENED_SETTINGS {
            (api.g_object_set)(settings, name.as_ptr(), value, ptr::null::<c_char>());
        }
        let web_view = (api.g_object_new)(
            (api.webkit_web_view_get_type)(),
            c"web-context".as_ptr(),
            context,
            c"user-content-manager".as_ptr(),
            content,
            c"settings".as_ptr(),
            settings,
            ptr::null::<c_char>(),
        );
        (api.g_object_unref)(content);
        (api.g_object_unref)(settings);
        let window = if parent.is_some() {
            (api.gtk_window_new)(GTK_WINDOW_POPUP)
        } else {
            (api.gtk_offscreen_window_new)()
        };
        (api.gtk_container_add)(window, web_view);
        (api.gtk_drag_dest_unset)(web_view);
        if let Some(background) = background {
            let color = GdkRgba {
                red: f64::from((background >> 16) & 0xff) / 255.,
                green: f64::from((background >> 8) & 0xff) / 255.,
                blue: f64::from(background & 0xff) / 255.,
                alpha: 1.,
            };
            (api.webkit_web_view_set_background_color)(web_view, &color);
        }
        connect(
            api,
            web_view,
            c"decide-policy",
            on_decide_policy as unsafe extern "C" fn(Ptr, Ptr, c_int, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"create",
            on_create as unsafe extern "C" fn(Ptr, Ptr, Ptr) -> Ptr,
            id,
        );
        connect(
            api,
            web_view,
            c"permission-request",
            on_permission_request as unsafe extern "C" fn(Ptr, Ptr, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"authenticate",
            on_authenticate as unsafe extern "C" fn(Ptr, Ptr, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"run-file-chooser",
            on_file_chooser as unsafe extern "C" fn(Ptr, Ptr, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"print",
            on_refuse as unsafe extern "C" fn(Ptr, Ptr, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"show-notification",
            on_refuse as unsafe extern "C" fn(Ptr, Ptr, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"enter-fullscreen",
            on_refuse_plain as unsafe extern "C" fn(Ptr, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"context-menu",
            on_context_menu as unsafe extern "C" fn(Ptr, Ptr, Ptr, Ptr, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"load-changed",
            on_load_changed as unsafe extern "C" fn(Ptr, c_int, Ptr),
            id,
        );
        connect(
            api,
            web_view,
            c"load-failed",
            on_load_failed
                as unsafe extern "C" fn(Ptr, c_int, *const c_char, *mut GError, Ptr) -> c_int,
            id,
        );
        connect(
            api,
            web_view,
            c"web-process-terminated",
            on_web_process_terminated as unsafe extern "C" fn(Ptr, c_int, Ptr),
            id,
        );
        connect(
            api,
            web_view,
            c"button-press-event",
            on_button_press as unsafe extern "C" fn(Ptr, Ptr, Ptr) -> c_int,
            id,
        );
        if parent.is_some() {
            connect(
                api,
                web_view,
                c"scroll-event",
                on_scroll as unsafe extern "C" fn(Ptr, Ptr, Ptr) -> c_int,
                id,
            );
        }
        let (x, y, visible) = position.unwrap_or((0, 0, false));
        Rc::new(View {
            api,
            id,
            web_view,
            window,
            embedded: parent.is_some(),
            position: Cell::new((x, y)),
            visible: Cell::new(visible),
            revealed: Cell::new(false),
            page,
            events,
            size: Cell::new(size),
            loaded: Cell::new(false),
            fit_pending: Cell::new(true),
            fit: Cell::new(1.),
            destroyed: Cell::new(false),
            snapshotting: Cell::new(false),
            snapshot_again: Cell::new(false),
            tiles: RefCell::new(Vec::new()),
        })
    };
    with_thread(|thread| thread.views.insert(id, view.clone()));
    view.apply_size();
    if let Some((parent, x11)) = parent {
        // SAFETY: the popup window is realised here, so it has an X11
        // window; GDK's Xlib connection reparents it into GPUI's native
        // surface window (a window of the same X server, which outlives the
        // view: the host destroys views before dropping the surface), out
        // of sight until the page is measured.
        unsafe {
            // GPUI's window and its native surface use the 32-bit ARGB
            // visual; a child of the same depth is drawn and captured
            // like the rest of the window.
            let visual = (api.gdk_screen_get_rgba_visual)((api.gtk_widget_get_screen)(view.window));
            if !visual.is_null() {
                (api.gtk_widget_set_visual)(view.window, visual);
            }
            (api.gtk_widget_realize)(view.window);
            let xid = (x11.api.gdk_x11_window_get_xid)((api.gtk_widget_get_window)(view.window));
            (x11.api.XReparentWindow)(x11.display, xid, parent as c_ulong, HIDDEN_AT, 0);
            (x11.api.XFlush)(x11.display);
            debug_log(format_args!(
                "view {id}: X11 window {xid:#x} in surface {parent:#x}"
            ));
        }
        view.apply_position();
    }
    // SAFETY: shows the new window and loads Zmail's page URL, which the
    // scheme handler serves.
    unsafe {
        (api.gtk_widget_show_all)(view.window);
        (api.webkit_web_view_load_uri)(view.web_view, url.as_ptr());
    }
    debug_log(format_args!("view {id}: loading {}", view.page.url));
}

/// Every `WebKitSettings` property Zmail sets, all booleans or enums
/// passed as C ints.
const HARDENED_SETTINGS: [(&CStr, c_int); 26] = [
    // Page scripts never run; Zmail's own scripts run in an isolated world,
    // which these do not stop.
    (c"enable-javascript", FALSE),
    (c"enable-javascript-markup", FALSE),
    (c"javascript-can-open-windows-automatically", FALSE),
    (c"javascript-can-access-clipboard", FALSE),
    (c"enable-html5-database", FALSE),
    (c"enable-html5-local-storage", FALSE),
    (c"enable-page-cache", FALSE),
    (c"enable-dns-prefetching", FALSE),
    (c"enable-webgl", FALSE),
    (c"enable-webaudio", FALSE),
    (c"enable-media", FALSE),
    (c"enable-mediasource", FALSE),
    (c"enable-encrypted-media", FALSE),
    (c"enable-media-stream", FALSE),
    (c"enable-webrtc", FALSE),
    (c"enable-fullscreen", FALSE),
    (c"enable-developer-extras", FALSE),
    (c"enable-site-specific-quirks", FALSE),
    (c"allow-file-access-from-file-urls", FALSE),
    (c"allow-universal-access-from-file-urls", FALSE),
    (c"allow-top-navigation-to-data-urls", FALSE),
    (c"enable-back-forward-navigation-gestures", FALSE),
    (c"enable-tabs-to-links", FALSE),
    (c"enable-caret-browsing", FALSE),
    // No GPU process work for mail: less attack surface, same pixels.
    (
        c"hardware-acceleration-policy",
        WEBKIT_HARDWARE_ACCELERATION_POLICY_NEVER,
    ),
    // Images load (Zmail serves them); remote ones are blocked or fetched
    // by Zmail.
    (c"auto-load-images", TRUE),
];

impl View {
    fn emit(&self, event: WebEvent) {
        let _ = self.events.unbounded_send(ViewEvent::Web(event));
    }

    /// Sizes the window to the view's device size and zooms the page so
    /// CSS pixels are GPUI's logical pixels (times the fitting zoom).
    fn apply_size(&self) {
        let size = self.size.get();
        let (width, mut height) = size.device();
        if !self.embedded {
            // The snapshot draws the whole page whatever the view's height;
            // the view only needs to be a screen tall, for selections.
            height = height.min(OFFSCREEN_HEIGHT);
        }
        // SAFETY: live widgets on the engine thread.
        unsafe {
            (self.api.gtk_widget_set_size_request)(self.web_view, width, height);
            (self.api.gtk_window_resize)(self.window, width, height);
            (self.api.webkit_web_view_set_zoom_level)(self.web_view, size.scale * self.fit.get());
        }
    }

    /// Moves an embedded view to its position, or out of sight while it is
    /// hidden or not measured yet. (It stays mapped, so WebKit keeps laying
    /// it out at its real size.)
    fn apply_position(&self) {
        if !self.embedded {
            return;
        }
        let (x, y) = if self.visible.get() && self.revealed.get() {
            self.position.get()
        } else {
            // Left of the parent: views are at most 16000 pixels wide.
            (HIDDEN_AT, 0)
        };
        // SAFETY: a live popup window; for a window whose parent is GPUI's
        // surface, GDK moves it within that parent.
        unsafe { (self.api.gtk_window_move)(self.window, x, y) };
    }

    fn resize(&self, size: ViewSize) {
        let old = self.size.replace(size);
        let refit = (old.width - size.width).abs() >= 0.5 || old.scale != size.scale;
        if refit {
            self.fit.set(1.);
        }
        self.apply_size();
        if refit && self.loaded.get() {
            self.fit_pending.set(true);
            self.evaluate(MEASURE_SCRIPT, ScriptPurpose::Measure);
        }
    }

    fn evaluate(&self, script: &str, purpose: ScriptPurpose) {
        let script = c_string(script);
        let request = Box::into_raw(Box::new(ScriptRequest {
            view: self.id,
            purpose,
        }));
        // SAFETY: a live web view; the script and world name are copied by
        // WebKit; the callback runs exactly once (GIO's contract), on this
        // thread, and takes back the boxed request.
        unsafe {
            (self.api.webkit_web_view_evaluate_javascript)(
                self.web_view,
                script.as_ptr(),
                -1,
                WORLD.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                on_script_finished,
                request.cast(),
            );
        }
    }

    /// A measurement came back.
    fn measured(&self, text: &str) {
        let Some(measurement) = Measurement::parse(text) else {
            eprintln!("zmail: web renderer: unexpected measurement {text:?}");
            self.emit(WebEvent::LoadFailed("unexpected measurement".into()));
            return;
        };
        if self.fit_pending.replace(false)
            && let Some(fit) = measurement.fit_zoom()
        {
            // Wider than the pane: zoom out to fit, then measure again.
            debug_log(format_args!(
                "view {}: page {}px wide in a {}px pane: zoom {fit:.2}",
                self.id, measurement.scroll_width, measurement.viewport_width
            ));
            self.fit.set(fit);
            self.apply_size();
            self.evaluate(MEASURE_SCRIPT, ScriptPurpose::Measure);
            return;
        }
        let fit = self.fit.get();
        let max = self.size.get().max_height(self.embedded);
        let height = measurement.points(fit).min(max);
        if measurement.height * fit > max {
            eprintln!("zmail: web renderer: message cut off at {max} points");
        }
        debug_log(format_args!(
            "view {}: measured {}px at zoom {fit:.2}",
            self.id, measurement.height
        ));
        if !self.revealed.replace(true) {
            self.apply_position();
        }
        self.emit(WebEvent::Measured {
            height: height as f32,
        });
        if !self.embedded && self.id != 0 {
            self.snapshot();
        }
    }

    /// Takes a picture of an offscreen view's whole page (selection
    /// included); [`ViewEvent::Snapshot`] follows once its links are found
    /// too. Calls while one is under way are folded into one more.
    fn snapshot(&self) {
        if self.embedded || self.destroyed.get() {
            return;
        }
        if self.snapshotting.replace(true) {
            self.snapshot_again.set(true);
            return;
        }
        // SAFETY: a live web view; the callback runs exactly once, on this
        // thread, with the view id as its data.
        unsafe {
            (self.api.webkit_web_view_get_snapshot)(
                self.web_view,
                WEBKIT_SNAPSHOT_REGION_FULL_DOCUMENT,
                WEBKIT_SNAPSHOT_OPTIONS_INCLUDE_SELECTION_HIGHLIGHTING,
                ptr::null_mut(),
                on_snapshot,
                self.id as usize as Ptr,
            );
        }
    }

    /// A snapshot (or its links) is done: take the next one if wanted.
    fn snapshot_done(&self) {
        self.snapshotting.set(false);
        if self.snapshot_again.replace(false) {
            self.snapshot();
        }
    }

    /// Destroys the window (and the web view in it) and fails its waiting
    /// scheme requests.
    fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        let pending: Vec<PendingFetch> = with_thread(|thread| {
            let (mine, others) = std::mem::take(&mut thread.pending)
                .into_iter()
                .partition(|fetch| fetch.view == self.id);
            thread.pending = others;
            mine
        })
        .unwrap_or_default();
        for fetch in pending {
            fail_request(self.api, fetch.request);
            // SAFETY: drops the reference taken when the fetch started.
            unsafe { (self.api.g_object_unref)(fetch.request) };
        }
        // SAFETY: live widgets on the engine thread; destroying the window
        // destroys the web view in it. Signals carry ids, so any that
        // arrive later find no view.
        unsafe {
            (self.api.webkit_web_view_stop_loading)(self.web_view);
            (self.api.gtk_widget_destroy)(self.window);
        }
    }
}

/// What a script evaluation is for, boxed through WebKit's callback.
struct ScriptRequest {
    view: u64,
    purpose: ScriptPurpose,
}

/// Asks for a page's failed inline images again after a pause.
unsafe extern "C" fn repair_again(data: Ptr) -> c_int {
    // SAFETY: `data` is the box leaked for this one-shot timeout.
    let (id, round) = *unsafe { Box::from_raw(data.cast::<(u64, u32)>()) };
    if let Some(view) = view_by_id(id) {
        view.evaluate(REPAIR_SCRIPT, ScriptPurpose::Repair(round));
    }
    G_SOURCE_REMOVE
}

unsafe extern "C" fn on_script_finished(web_view: Ptr, result: Ptr, data: Ptr) {
    // SAFETY: `data` is the box `View::evaluate` leaked for this callback,
    // which GIO calls exactly once.
    let request = unsafe { Box::from_raw(data.cast::<ScriptRequest>()) };
    let Some(api) = API.get().and_then(|api| api.as_ref().ok()) else {
        return;
    };
    let mut error = ptr::null_mut();
    // SAFETY: `web_view` and `result` are what GIO passes; the value is
    // a new reference we release after reading it as a string (freed with
    // g_free).
    let value = unsafe {
        let value = (api.webkit_web_view_evaluate_javascript_finish)(web_view, result, &mut error);
        if value.is_null() {
            None
        } else {
            let text = (api.jsc_value_to_string)(value);
            let string = copy_c_str(text);
            (api.g_free)(text.cast());
            (api.g_object_unref)(value);
            Some(string)
        }
    };
    let result = match (value, take_error(api, error)) {
        (_, Some((_, _, message))) => Err(message),
        (Some(value), None) => Ok(value),
        (None, None) => Err("no result".to_owned()),
    };
    let Some(view) = view_by_id(request.view) else {
        return;
    };
    match (request.purpose, result) {
        (ScriptPurpose::Measure, Ok(text)) => view.measured(&text),
        (ScriptPurpose::Repair(round), result) => {
            let waiting: u32 = result.ok().and_then(|text| text.parse().ok()).unwrap_or(0);
            if waiting > 0 && round < REPAIR_ROUNDS {
                debug_log(format_args!(
                    "view {}: {waiting} inline image(s) asked for again",
                    view.id
                ));
                let data = Box::into_raw(Box::new((view.id, round + 1)));
                // SAFETY: a static callback on this thread's main context;
                // it takes back the box once.
                unsafe { (api.g_timeout_add)(REPAIR_DELAY_MS, repair_again, data.cast()) };
            } else {
                view.evaluate(MEASURE_SCRIPT, ScriptPurpose::Measure);
            }
        }
        (ScriptPurpose::Measure, Err(message)) => {
            eprintln!("zmail: web renderer: measuring the message failed: {message}");
            view.emit(WebEvent::LoadFailed(format!("measuring failed: {message}")));
        }
        (ScriptPurpose::Links, result) => {
            let links = match result {
                Ok(text) => parse_links(&text, view.fit.get()),
                Err(message) => {
                    debug_log(format_args!("view {}: no link map: {message}", view.id));
                    Vec::new()
                }
            };
            let tiles = std::mem::take(&mut *view.tiles.borrow_mut());
            debug_log(format_args!(
                "view {}: snapshot of {} tile(s), {} link box(es)",
                view.id,
                tiles.len(),
                links.len()
            ));
            let _ = view.events.unbounded_send(ViewEvent::Snapshot(Snapshot {
                scale: view.size.get().scale.max(0.5),
                zoom: view.fit.get(),
                tiles,
                links,
            }));
            view.snapshot_done();
        }
        (ScriptPurpose::Caller(tag), result) => {
            let _ = view
                .events
                .unbounded_send(ViewEvent::Script { tag, result });
        }
    }
}

unsafe extern "C" fn on_snapshot(web_view: Ptr, result: Ptr, data: Ptr) {
    let Some(api) = loaded_api() else {
        return;
    };
    let mut error = ptr::null_mut();
    // SAFETY: the objects GIO passes to this callback; the surface (if any)
    // is ours to destroy.
    let surface =
        unsafe { (api.webkit_web_view_get_snapshot_finish)(web_view, result, &mut error) };
    let failure = take_error(api, error);
    let Some(view) = view_by_id(id_of(data)) else {
        if !surface.is_null() {
            // SAFETY: as above.
            unsafe { (api.cairo_surface_destroy)(surface) };
        }
        return;
    };
    if surface.is_null() {
        let message = failure.map_or_else(|| "no picture".to_owned(), |(_, _, message)| message);
        eprintln!("zmail: web renderer: the page could not be drawn: {message}");
        view.emit(WebEvent::LoadFailed(format!(
            "the page could not be drawn: {message}"
        )));
        view.snapshot_done();
        return;
    }
    // SAFETY: a cairo image surface WebKit drew; after a flush its data is
    // `stride * height` bytes of 32-bit pixels for the two formats
    // accepted, valid until the surface is destroyed (after the copy).
    let tiles = unsafe {
        (api.cairo_surface_flush)(surface);
        let format = (api.cairo_image_surface_get_format)(surface);
        let width = usize::try_from((api.cairo_image_surface_get_width)(surface)).unwrap_or(0);
        let height = usize::try_from((api.cairo_image_surface_get_height)(surface)).unwrap_or(0);
        let stride = usize::try_from((api.cairo_image_surface_get_stride)(surface)).unwrap_or(0);
        let data = (api.cairo_image_surface_get_data)(surface);
        let tiles = if (format == CAIRO_FORMAT_ARGB32 || format == CAIRO_FORMAT_RGB24)
            && !data.is_null()
            && width > 0
            && stride >= width * 4
        {
            let pixels = std::slice::from_raw_parts(data, stride * height);
            (0..height)
                .step_by(TILE_ROWS)
                .map(|top| {
                    let rows = TILE_ROWS.min(height - top);
                    Tile {
                        top: top as u32,
                        width: width as u32,
                        height: rows as u32,
                        bmp: bmp(width, rows, stride, &pixels[top * stride..]),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        (api.cairo_surface_destroy)(surface);
        tiles
    };
    *view.tiles.borrow_mut() = tiles;
    view.evaluate(LINKS_SCRIPT, ScriptPurpose::Links);
}

// ---------------------------------------------------------------------------
// Signal handlers (engine thread)
// ---------------------------------------------------------------------------

fn loaded_api() -> Option<&'static Api> {
    API.get()?.as_ref().ok()
}

/// `decide-policy`: only Zmail's own page load; link clicks are reported.
unsafe extern "C" fn on_decide_policy(
    _web_view: Ptr,
    decision: Ptr,
    kind: c_int,
    data: Ptr,
) -> c_int {
    let Some(api) = loaded_api() else {
        return FALSE;
    };
    let view = view_by_id(id_of(data));
    // SAFETY: `decision` is the live WebKitPolicyDecision of this signal, of
    // the subclass `kind` names; each getter returns borrowed objects and
    // strings valid during the call; it is used or ignored exactly once.
    unsafe {
        let allowed = match kind {
            WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION => {
                let action =
                    (api.webkit_navigation_policy_decision_get_navigation_action)(decision);
                let navigation = (api.webkit_navigation_action_get_navigation_type)(action);
                let request = (api.webkit_navigation_action_get_request)(action);
                let url = copy_c_str((api.webkit_uri_request_get_uri)(request));
                if navigation == WEBKIT_NAVIGATION_TYPE_LINK_CLICKED
                    && let Some(view) = &view
                {
                    view.emit(WebEvent::LinkActivated { url: url.clone() });
                }
                let allowed = navigation == WEBKIT_NAVIGATION_TYPE_OTHER
                    && view.as_ref().is_some_and(|view| url == view.page.url);
                if !allowed {
                    debug_log(format_args!("navigation denied: {url} ({navigation})"));
                }
                allowed
            }
            WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION => false,
            _ => {
                let response = (api.webkit_response_policy_decision_get_response)(decision);
                let url = copy_c_str((api.webkit_uri_response_get_uri)(response));
                let main =
                    (api.webkit_response_policy_decision_is_main_frame_main_resource)(decision)
                        != FALSE;
                // Nothing is ever downloaded.
                let shown =
                    (api.webkit_response_policy_decision_is_mime_type_supported)(decision) != FALSE;
                shown && (!main || view.as_ref().is_some_and(|view| url == view.page.url))
            }
        };
        if allowed {
            (api.webkit_policy_decision_use)(decision);
        } else {
            (api.webkit_policy_decision_ignore)(decision);
        }
    }
    TRUE
}

/// `create`: no new windows, ever.
unsafe extern "C" fn on_create(_web_view: Ptr, _action: Ptr, _data: Ptr) -> Ptr {
    ptr::null_mut()
}

/// `permission-request`: every permission is denied.
unsafe extern "C" fn on_permission_request(_web_view: Ptr, request: Ptr, _data: Ptr) -> c_int {
    if let Some(api) = loaded_api() {
        // SAFETY: the live WebKitPermissionRequest of this signal.
        unsafe { (api.webkit_permission_request_deny)(request) };
    }
    TRUE
}

/// `authenticate`: no credentials are ever given.
unsafe extern "C" fn on_authenticate(_web_view: Ptr, request: Ptr, _data: Ptr) -> c_int {
    if let Some(api) = loaded_api() {
        // SAFETY: the live WebKitAuthenticationRequest of this signal.
        unsafe { (api.webkit_authentication_request_cancel)(request) };
    }
    TRUE
}

/// `run-file-chooser`: no files are ever offered.
unsafe extern "C" fn on_file_chooser(_web_view: Ptr, request: Ptr, _data: Ptr) -> c_int {
    if let Some(api) = loaded_api() {
        // SAFETY: the live WebKitFileChooserRequest of this signal.
        unsafe { (api.webkit_file_chooser_request_cancel)(request) };
    }
    TRUE
}

/// `print`, `show-notification`: handled by doing nothing.
unsafe extern "C" fn on_refuse(_web_view: Ptr, _object: Ptr, _data: Ptr) -> c_int {
    TRUE
}

/// `enter-fullscreen`: refused.
unsafe extern "C" fn on_refuse_plain(_web_view: Ptr, _data: Ptr) -> c_int {
    TRUE
}

/// `context-menu`: only Copy is kept; no menu when nothing is left.
unsafe extern "C" fn on_context_menu(
    _web_view: Ptr,
    menu: Ptr,
    _event: Ptr,
    _hit: Ptr,
    _data: Ptr,
) -> c_int {
    let Some(api) = loaded_api() else {
        return TRUE;
    };
    // SAFETY: the live WebKitContextMenu of this signal; items are borrowed
    // and removed one at a time, re-reading the count after each removal.
    unsafe {
        let mut index = 0;
        while index < (api.webkit_context_menu_get_n_items)(menu) {
            let item = (api.webkit_context_menu_get_item_at_position)(menu, index);
            if (api.webkit_context_menu_item_get_stock_action)(item)
                == WEBKIT_CONTEXT_MENU_ACTION_COPY
            {
                index += 1;
            } else {
                (api.webkit_context_menu_remove)(menu, item);
            }
        }
        if (api.webkit_context_menu_get_n_items)(menu) == 0 {
            TRUE
        } else {
            FALSE
        }
    }
}

/// `load-changed`: a finished page is measured (the warm-up page starts
/// the engine), and its web process is checked again.
unsafe extern "C" fn on_load_changed(_web_view: Ptr, event: c_int, data: Ptr) {
    if event != WEBKIT_LOAD_FINISHED {
        return;
    }
    let id = id_of(data);
    if id == 0 {
        warmed_up();
        return;
    }
    match linux_sandbox::check_web_processes() {
        Ok(count) => VERIFIED.store(count, Ordering::SeqCst),
        Err(reason) => {
            fail(reason);
            return;
        }
    }
    if let Some(view) = view_by_id(id) {
        debug_log(format_args!("view {id}: page loaded"));
        view.loaded.set(true);
        view.fit_pending.set(true);
        view.evaluate(REPAIR_SCRIPT, ScriptPurpose::Repair(1));
    }
}

/// `load-failed`: reported, except cancellations; WebKit's own error page
/// is never shown.
unsafe extern "C" fn on_load_failed(
    _web_view: Ptr,
    _event: c_int,
    uri: *const c_char,
    error: *mut GError,
    data: Ptr,
) -> c_int {
    let Some(api) = loaded_api() else {
        return TRUE;
    };
    // SAFETY: the signal's URI and GError are valid during the call; the
    // error is borrowed (not freed here).
    let (uri, domain, code, message) = unsafe {
        let error = &*error;
        (
            copy_c_str(uri),
            error.domain,
            error.code,
            copy_c_str(error.message),
        )
    };
    // SAFETY: the quark functions only return constants.
    let (network, policy) = unsafe {
        (
            (api.webkit_network_error_quark)(),
            (api.webkit_policy_error_quark)(),
        )
    };
    let interrupted = (domain == network && code == WEBKIT_NETWORK_ERROR_CANCELLED)
        || (domain == policy
            && code == WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE);
    if interrupted {
        return TRUE;
    }
    let id = id_of(data);
    eprintln!("zmail: web renderer: the message did not load: {message} ({code})");
    debug_log(format_args!("view {id}: {uri} failed"));
    if id == 0 {
        fail(format!(
            "the web engine's first page did not load: {message}"
        ));
    } else if let Some(view) = view_by_id(id) {
        view.emit(WebEvent::LoadFailed(message));
    }
    TRUE
}

/// `web-process-terminated`.
unsafe extern "C" fn on_web_process_terminated(_web_view: Ptr, reason: c_int, data: Ptr) {
    eprintln!("zmail: web renderer: the web content process ended ({reason})");
    let id = id_of(data);
    if id == 0 {
        fail("the web engine's first web process ended".into());
    } else if let Some(view) = view_by_id(id) {
        view.emit(WebEvent::ContentProcessTerminated);
    }
}

/// `button-press-event`: reported; the press goes on to WebKit.
unsafe extern "C" fn on_button_press(_widget: Ptr, _event: Ptr, data: Ptr) -> c_int {
    if let Some(view) = view_by_id(id_of(data)) {
        view.emit(WebEvent::Pressed);
    }
    FALSE
}

/// `scroll-event` on an embedded view: vertical wheel turns go to GPUI
/// (the conversation scrolls as one list); sideways ones, and Shift with
/// the wheel, stay with a page wider than the pane.
unsafe extern "C" fn on_scroll(_widget: Ptr, event: Ptr, data: Ptr) -> c_int {
    let (Some(api), Some(view)) = (loaded_api(), view_by_id(id_of(data))) else {
        return FALSE;
    };
    let (mut dx, mut dy, mut direction, mut x, mut y, mut state) = (0., 0., 0, 0., 0., 0);
    // SAFETY: the signal's live GdkEventScroll; the getters only write to
    // the locals passed.
    let (smooth, discrete) = unsafe {
        (api.gdk_event_get_coords)(event, &mut x, &mut y);
        (api.gdk_event_get_state)(event, &mut state);
        (
            (api.gdk_event_get_scroll_deltas)(event, &mut dx, &mut dy) != FALSE,
            (api.gdk_event_get_scroll_direction)(event, &mut direction) != FALSE,
        )
    };
    let lines = if smooth {
        dy
    } else if discrete && direction == GDK_SCROLL_UP {
        -1.
    } else if discrete && direction == GDK_SCROLL_DOWN {
        1.
    } else {
        return FALSE;
    };
    if state & GDK_SHIFT_MASK != 0 || dx.abs() > lines.abs() || lines == 0. {
        return FALSE;
    }
    let scale = view.size.get().scale.max(0.5);
    let _ = view.events.unbounded_send(ViewEvent::Scroll {
        x: x / scale,
        y: y / scale,
        lines,
        control: state & GDK_CONTROL_MASK != 0,
    });
    TRUE
}

/// The context's `download-started`: every download is cancelled.
unsafe extern "C" fn on_download_started(_context: Ptr, download: Ptr, _data: Ptr) {
    eprintln!("zmail: web renderer: a download was cancelled");
    if let Some(api) = loaded_api() {
        // SAFETY: the live WebKitDownload of this signal.
        unsafe { (api.webkit_download_cancel)(download) };
    }
}

// ---------------------------------------------------------------------------
// The scheme handler (engine thread)
// ---------------------------------------------------------------------------

/// Serves `zmail-mail:` and `cid:` requests: the view's page, its inline
/// images and the remote images Zmail fetches. Everything else fails.
unsafe extern "C" fn on_scheme_request(request: Ptr, _data: Ptr) {
    let Some(api) = loaded_api() else {
        return;
    };
    // SAFETY: the live WebKitURISchemeRequest of this callback; the URI and
    // web view are borrowed for the call.
    let (url, web_view) = unsafe {
        (
            copy_c_str((api.webkit_uri_scheme_request_get_uri)(request)),
            (api.webkit_uri_scheme_request_get_web_view)(request),
        )
    };
    let view = with_thread(|thread| {
        thread
            .views
            .values()
            .find(|view| view.web_view == web_view)
            .cloned()
    })
    .flatten();
    let Some(view) = view else {
        fail_request(api, request);
        return;
    };
    let page = &view.page;
    let served = match parse_scheme_request(&url) {
        SchemeRequest::Page(_) if url == page.url => Some((
            "text/html; charset=utf-8".to_owned(),
            page.html.clone(),
            Some(page.policy.as_str()),
        )),
        SchemeRequest::Inline(content_id) => page
            .resources
            .get(&content_id)
            // Inline parts are only ever images.
            .filter(|resource| resource.mime_type.starts_with("image/"))
            .map(|resource| (resource.mime_type.clone(), resource.data.to_vec(), None)),
        SchemeRequest::Remote(index) => {
            if let Some(Some(remote)) = page.remote.get(index) {
                debug_log(format_args!("fetching remote image {index}"));
                // SAFETY: keeps the request alive until it is answered.
                let request = unsafe { (api.g_object_ref)(request) };
                with_thread(|thread| {
                    thread.pending.push(PendingFetch {
                        view: view.id,
                        request,
                        result: RemoteFetcher::shared().fetch(remote.clone()),
                    })
                });
                start_polling(api);
                return;
            }
            None
        }
        _ => None,
    };
    match served {
        Some((mime_type, body, policy)) => {
            debug_log(format_args!(
                "view {}: served {url} ({} bytes of {mime_type})",
                view.id,
                body.len()
            ));
            respond(api, request, &mime_type, &body, policy);
        }
        None => {
            debug_log(format_args!("scheme request refused: {url}"));
            fail_request(api, request);
        }
    }
}

/// Answers a scheme request with `body`.
fn respond(api: &Api, request: Ptr, mime_type: &str, body: &[u8], policy: Option<&str>) {
    let mime_type = c_string(mime_type);
    let mut headers = vec![
        (c"Cache-Control", c_string("no-store")),
        (c"X-Content-Type-Options", c_string("nosniff")),
        (c"Referrer-Policy", c_string("no-referrer")),
    ];
    if let Some(policy) = policy {
        headers.push((c"Content-Security-Policy", c_string(policy)));
    }
    // SAFETY: `request` is a live, unanswered scheme request (we are in the
    // handler, or it was referenced when its fetch started); the bytes are
    // copied into a GBytes the stream keeps; the headers are handed to the
    // response (transfer full); the response is released after WebKit has
    // taken it.
    unsafe {
        let bytes = (api.g_bytes_new)(body.as_ptr().cast(), body.len());
        let stream = (api.g_memory_input_stream_new_from_bytes)(bytes);
        (api.g_bytes_unref)(bytes);
        let response = (api.webkit_uri_scheme_response_new)(stream, body.len() as i64);
        (api.g_object_unref)(stream);
        (api.webkit_uri_scheme_response_set_content_type)(response, mime_type.as_ptr());
        let soup_headers = (api.soup_message_headers_new)(SOUP_MESSAGE_HEADERS_RESPONSE);
        for (name, value) in &headers {
            (api.soup_message_headers_append)(soup_headers, name.as_ptr(), value.as_ptr());
        }
        (api.webkit_uri_scheme_response_set_http_headers)(response, soup_headers);
        (api.webkit_uri_scheme_request_finish_with_response)(request, response);
        (api.g_object_unref)(response);
    }
}

/// Fails a scheme request: Zmail serves nothing but its page and parts.
fn fail_request(api: &Api, request: Ptr) {
    // SAFETY: a live, unanswered scheme request (see `respond`); WebKit
    // copies the error, which is freed after.
    unsafe {
        let error = (api.g_error_new_literal)(
            (api.g_io_error_quark)(),
            G_IO_ERROR_NOT_FOUND,
            c"not found".as_ptr(),
        );
        (api.webkit_uri_scheme_request_finish_error)(request, error);
        (api.g_error_free)(error);
    }
}

/// Polls the remote image fetches on the engine thread while any waits.
fn start_polling(api: &Api) {
    let start =
        with_thread(|thread| !std::mem::replace(&mut thread.polling, true)).unwrap_or(false);
    if start {
        // SAFETY: a static callback on this thread's main context; it
        // returns G_SOURCE_REMOVE when no fetch is left.
        unsafe { (api.g_timeout_add)(FETCH_POLL_MS, answer_fetches, ptr::null_mut()) };
    }
}

unsafe extern "C" fn answer_fetches(_data: Ptr) -> c_int {
    let Some(api) = loaded_api() else {
        return G_SOURCE_REMOVE;
    };
    let finished: Vec<(PendingFetch, FetchResult)> = with_thread(|thread| {
        let mut finished = Vec::new();
        let mut waiting = Vec::new();
        for fetch in thread.pending.drain(..) {
            match fetch.result.try_recv() {
                Ok(result) => finished.push((fetch, result)),
                Err(TryRecvError::Empty) => waiting.push(fetch),
                Err(TryRecvError::Disconnected) => finished.push((
                    fetch,
                    Err(RemoteError::Request("the fetcher stopped".to_owned())),
                )),
            }
        }
        thread.pending = waiting;
        finished
    })
    .unwrap_or_default();
    for (fetch, result) in finished {
        match result {
            Ok(image) => {
                debug_log(format_args!(
                    "remote image fetched: {} bytes of {}",
                    image.bytes.len(),
                    image.content_type
                ));
                respond(api, fetch.request, &image.content_type, &image.bytes, None);
            }
            Err(error) => {
                debug_log(format_args!("remote image refused: {error}"));
                fail_request(api, fetch.request);
            }
        }
        // SAFETY: drops the reference taken when the fetch started.
        unsafe { (api.g_object_unref)(fetch.request) };
    }
    let more = with_thread(|thread| {
        thread.polling = !thread.pending.is_empty();
        thread.polling
    })
    .unwrap_or(false);
    if more {
        G_SOURCE_CONTINUE
    } else {
        G_SOURCE_REMOVE
    }
}

// ---------------------------------------------------------------------------
// The safe handle
// ---------------------------------------------------------------------------

/// One message's web view. Dropping it destroys the view.
pub struct PlatformWebView {
    id: u64,
}

impl PlatformWebView {
    /// Starts showing `document` at `size`, offscreen or (with `parent`,
    /// on X11) embedded in that X11 window; `events` hears about
    /// measurements, link clicks, failures, wheel turns and script
    /// results. The engine must have been started ([`start`]).
    pub fn new(
        document: &WebDocument,
        size: ViewSize,
        parent: Option<u64>,
        events: UnboundedSender<ViewEvent>,
    ) -> Result<Self, String> {
        start_failed()?;
        let id = NEXT_VIEW.fetch_add(1, Ordering::SeqCst);
        let dark = document.draws_dark();
        send(Command::Create(Box::new(CreateView {
            id,
            page: ServedPage {
                url: page_url(&page_token()),
                html: page_html(document).into_bytes(),
                policy: content_security_policy(true),
                resources: document.resources_by_id(),
                remote: document.remote.clone(),
            },
            size,
            background: dark.then_some(document.appearance.background),
            parent,
            position: None,
            events,
        })));
        Ok(Self { id })
    }

    /// Places an embedded view at `(x, y)` device pixels in its parent
    /// window, shown or hidden. It stays hidden until its page is measured.
    pub fn move_to(&self, x: i32, y: i32, visible: bool) {
        send(Command::Move {
            id: self.id,
            x,
            y,
            visible,
        });
    }

    /// Resizes the view (logical pixels at `size.scale`).
    pub fn resize(&self, size: ViewSize) {
        send(Command::Resize { id: self.id, size });
    }

    /// Copies the page's selection to the clipboard.
    pub fn copy(&self) {
        send(Command::Editing {
            id: self.id,
            command: c"Copy",
        });
    }

    /// Selects the whole page.
    pub fn select_all(&self) {
        send(Command::Editing {
            id: self.id,
            command: c"SelectAll",
        });
    }

    /// Asks an offscreen view for a new [`ViewEvent::Snapshot`] (after a
    /// change to its selection).
    pub fn request_snapshot(&self) {
        send(Command::Snapshot { id: self.id });
    }

    /// Runs `script` in Zmail's isolated world; its string result comes
    /// back as [`ViewEvent::Script`] with `tag`.
    pub fn evaluate(&self, script: &str, tag: u64) {
        send(Command::Evaluate {
            id: self.id,
            script: script.to_owned(),
            tag,
        });
    }
}

impl Drop for PlatformWebView {
    fn drop(&mut self) {
        send(Command::Destroy { id: self.id });
    }
}

/// Stops the engine before the process exits: destroys every view and
/// stops the engine thread's main loop, waiting (briefly) until it has. A
/// process that exits while WebKit is busy on its thread can crash in
/// WebKit's exit handlers (CI saw a SIGSEGV after the canary finished).
/// Nothing can be shown afterwards.
pub fn shutdown() {
    if engine().is_none() {
        return;
    }
    let (done, wait) = mpsc::channel();
    send(Command::Shutdown { done });
    if wait.recv_timeout(Duration::from_secs(2)).is_err() {
        eprintln!("zmail: web renderer: the web engine did not stop in time");
    }
}

/// Destroys `views` and waits (briefly) until the engine thread has: for
/// views embedded in a window that is about to be destroyed.
pub fn destroy_now(views: Vec<PlatformWebView>) {
    // A stopped engine has destroyed every view already.
    if views.is_empty() || stopped().is_some() {
        return;
    }
    let (done, wait) = mpsc::channel();
    send(Command::DestroyNow {
        ids: views.iter().map(|view| view.id).collect(),
        done,
    });
    if wait.recv_timeout(Duration::from_secs(1)).is_err() {
        eprintln!("zmail: web renderer: web views were not destroyed in time");
    }
}

/// Tests: a plain, shown X11 window of `width` x `height` pixels, owned by
/// the engine thread, to embed views in (GPUI's native surface in the
/// app). Never destroyed.
pub fn test_parent_window(width: i32, height: i32) -> Result<u64, String> {
    start_failed()?;
    let (done, wait) = mpsc::channel();
    send(Command::TestParent {
        width,
        height,
        done,
    });
    wait.recv_timeout(START_TIMEOUT)
        .map_err(|_| "the engine thread did not answer".to_owned())?
}

fn test_parent(width: c_int, height: c_int) -> Result<u64, String> {
    let (Some(api), Some(x11)) = (loaded_api(), X11.with(Cell::get)) else {
        return Err("the web engine is not on X11".into());
    };
    // SAFETY: on the engine thread with GTK initialised on X11; a new
    // toplevel window, shown and realised, so it has an X11 window.
    unsafe {
        let window = (api.gtk_window_new)(0);
        (api.gtk_window_resize)(window, width, height);
        (api.gtk_widget_show_all)(window);
        (api.gtk_widget_realize)(window);
        let xid: c_ulong = (x11.api.gdk_x11_window_get_xid)((api.gtk_widget_get_window)(window));
        Ok(xid as u64)
    }
}

/// Fails when the engine is not running.
fn start_failed() -> Result<(), String> {
    match ENGINE.get() {
        None => Err("the web engine was not started".into()),
        Some(Err(reason)) => Err(reason.clone()),
        Some(Ok(_)) => stopped().map_or(Ok(()), Err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bmp_is_top_down_bgrx() {
        // Two rows of two pixels, with two bytes of row padding.
        let rows = [
            1, 2, 3, 255, 4, 5, 6, 255, 0, 0, 7, 8, 9, 255, 10, 11, 12, 255, 0, 0,
        ];
        let file = bmp(2, 2, 10, &rows);
        assert_eq!(&file[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes(file[2..6].try_into().expect("4 bytes")),
            54 + 16
        );
        assert_eq!(
            i32::from_le_bytes(file[22..26].try_into().expect("4 bytes")),
            -2
        );
        assert_eq!(
            u16::from_le_bytes(file[28..30].try_into().expect("2 bytes")),
            32
        );
        assert_eq!(&file[54..62], &rows[..8]);
        assert_eq!(&file[62..70], &rows[10..18]);
    }

    #[test]
    fn link_lines_parse_and_scale() {
        let links = parse_links(
            "10 20 30 15 zmail-link:0\n\nbad line\n0 0 0 0 zmail-link:1\n4 8 12 16 https://x.test/a b",
            0.5,
        );
        assert_eq!(
            links,
            vec![
                LinkArea {
                    url: "zmail-link:0".into(),
                    x: 5.,
                    y: 10.,
                    width: 15.,
                    height: 7.5,
                },
                LinkArea {
                    url: "https://x.test/a b".into(),
                    x: 2.,
                    y: 4.,
                    width: 6.,
                    height: 8.,
                },
            ]
        );
    }
}
